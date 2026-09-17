//! Supervisão dos subsistemas.
//!
//! Um panic na engine ou no soquete não pode derrubar os outros nem o core:
//! cada um roda numa thread própria, com `catch_unwind` na borda, e volta a
//! subir sozinho. O depurador não passa por aqui — cada sessão é isolada na
//! própria thread (ver [`debugger`]).
//!
//! É por isso que o release não usa `panic = "abort"` — sem unwind o
//! `catch_unwind` não pega nada.

pub mod debugger;
pub mod engine;

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::rpc::Sender;

/// Sem esta espera, um subsistema que falha ao iniciar giraria em laço
/// apertado consumindo CPU.
const RESTART_DELAY: Duration = Duration::from_millis(500);

/// Quantas quedas seguidas antes de desistir: insistir para sempre esconderia
/// o problema.
const MAX_RESTARTS: u32 = 5;

/// Depois de rodar bem por este tempo o contador zera: uma queda hoje e outra
/// daqui a uma hora não são o mesmo problema.
const HEALTHY_AFTER: Duration = Duration::from_secs(30);

/// Qual subsistema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subsystem {
    /// Análise de Pawn e LSP.
    Engine,
    /// O soquete único por onde LSP, DAP e o plugin chegam.
    Gateway,
}

impl Subsystem {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Gateway => "gateway",
        }
    }
}

/// Em que estado um subsistema está.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Starting,
    Running,
    /// Caiu e está esperando para subir de novo.
    Restarting,
    /// Caiu vezes demais: não sobe mais sozinho.
    Failed,
    /// Encerrado a pedido.
    Stopped,
}

/// Estado observável de um subsistema.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub subsystem: Subsystem,
    pub health: Health,
    /// Quantas vezes caiu e voltou desde a última janela saudável.
    pub restarts: u32,
}

/// Um subsistema supervisionado.
///
/// Retornar da função de trabalho é queda: nenhum subsistema termina de
/// propósito enquanto o core vive.
pub struct Supervised {
    /// Ligado enquanto o subsistema deve rodar; desligar pede o encerramento.
    running: Arc<AtomicBool>,
    /// A thread do laço, para quem precisa esperá-la sair.
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Supervised {
    /// Sobe o subsistema numa thread própria e passa a vigiá-lo.
    ///
    /// A função recebe um sinalizador: enquanto for `true`, continua; quando
    /// virar `false`, deve retornar. É assim que o encerramento chega sem
    /// matar a thread no meio de uma operação.
    ///
    /// # Errors
    /// Falha do sistema ao criar a thread. Sem ela não há supervisão.
    pub fn spawn<F>(subsystem: Subsystem, sender: Sender, work: F) -> std::io::Result<Self>
    where
        F: Fn(&AtomicBool) + Send + 'static,
    {
        let running = Arc::new(AtomicBool::new(true));
        let restarts = Arc::new(AtomicU32::new(0));

        let thread_running = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name(format!("pawnpro-{}", subsystem.name()))
            .spawn(move || {
                supervise(subsystem, &sender, &work, &thread_running, &restarts);
            })?;

        Ok(Self {
            running,
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Pede o encerramento e retorna. Quem quiser confirmar observa o
    /// `Health`.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Pede o encerramento e espera a thread sair.
    ///
    /// Para quem vai desmontar algo de que o laço depende — o runtime onde ele
    /// espera, por exemplo — e não pode fazê-lo com o laço ainda no meio.
    pub fn stop_and_join(&self) {
        self.stop();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    /// `true` se ainda deve estar rodando.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// O laço que mantém um subsistema de pé.
fn supervise<F>(
    subsystem: Subsystem,
    sender: &Sender,
    work: &F,
    running: &AtomicBool,
    restarts: &AtomicU32,
) where
    F: Fn(&AtomicBool),
{
    notify(sender, subsystem, Health::Starting, 0);

    while running.load(Ordering::Relaxed) {
        let started = Instant::now();
        notify(
            sender,
            subsystem,
            Health::Running,
            restarts.load(Ordering::Relaxed),
        );

        // A fronteira: um panic aqui vira queda comum, e os outros
        // subsistemas nem ficam sabendo.
        let panicked = std::panic::catch_unwind(AssertUnwindSafe(|| work(running))).is_err();

        if !running.load(Ordering::Relaxed) {
            notify(
                sender,
                subsystem,
                Health::Stopped,
                restarts.load(Ordering::Relaxed),
            );
            return;
        }

        // Rodou bem o bastante: as quedas anteriores são outro problema.
        if started.elapsed() >= HEALTHY_AFTER {
            restarts.store(0, Ordering::Relaxed);
        }

        let count = restarts.fetch_add(1, Ordering::Relaxed) + 1;
        if count > MAX_RESTARTS {
            // Insistir esconderia o problema. Desligar o sinalizador é o que
            // diz a quem pergunta que o subsistema não está de pé — sem isso
            // ninguém pediria para subi-lo de novo.
            running.store(false, Ordering::Relaxed);
            crate::diag_error!(
                "core/supervisor",
                "{} desistiu depois de {count} quedas",
                subsystem.name()
            );
            notify(sender, subsystem, Health::Failed, count);
            return;
        }

        crate::diag_warn!(
            "core/supervisor",
            "{} caiu ({}) e vai subir de novo; queda {count}",
            subsystem.name(),
            if panicked { "panic" } else { "retornou" }
        );
        notify(sender, subsystem, Health::Restarting, count);
        std::thread::sleep(RESTART_DELAY);
    }

    notify(
        sender,
        subsystem,
        Health::Stopped,
        restarts.load(Ordering::Relaxed),
    );
}

fn notify(sender: &Sender, subsystem: Subsystem, health: Health, restarts: u32) {
    sender.notify(
        "core.subsystemStatus",
        json!(Status {
            subsystem,
            health,
            restarts
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::mpsc;

    struct Collector(mpsc::Sender<String>);

    impl Write for Collector {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = String::from_utf8_lossy(buf).into_owned();
            if !text.trim().is_empty() {
                let _ = self.0.send(text.trim().to_string());
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn collector() -> (Sender, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (Sender::new(Box::new(Collector(tx))), rx)
    }

    /// Espera uma notificação com o `health` dado, ou desiste.
    fn wait_for(rx: &mpsc::Receiver<String>, health: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(line) = rx.recv_timeout(Duration::from_millis(200))
                && line.contains(health)
            {
                return true;
            }
        }
        false
    }

    /// Espera uma condição virar verdadeira, ou desiste.
    ///
    /// As notificações são enviadas **antes** do trabalho começar — `Running`
    /// sai e só então `work` é chamado. Conferir um efeito logo depois de ver
    /// a notificação é uma corrida que o CI perde de vez em quando.
    fn wait_until(condition: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_panic_becomes_a_restart_instead_of_a_crash() {
        // O ponto do supervisor: um subsistema que explode não leva o processo
        // junto — vira uma queda comum e sobe de novo.
        let (sender, rx) = collector();
        let attempts = Arc::new(AtomicU32::new(0));
        let seen = Arc::clone(&attempts);
        let sup = Supervised::spawn(Subsystem::Engine, sender, move |_| {
            // O panic da primeira tentativa é o objeto do teste: é ele que o
            // `catch_unwind` do supervisor precisa conter.
            assert!(seen.fetch_add(1, Ordering::Relaxed) != 0, "falha simulada");
            // Na segunda tentativa fica de pé por um instante.
            std::thread::sleep(Duration::from_millis(100));
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "restarting"), "não avisou o reinício");
        // Espera a SEGUNDA tentativa começar de fato. Parar antes encerraria o
        // laço no meio do intervalo de reinício, e conferir o contador logo
        // após a notificação de `running` chegaria antes de `work` rodar.
        assert!(
            wait_until(|| attempts.load(Ordering::Relaxed) >= 2),
            "não tentou de novo"
        );
        sup.stop();
    }

    #[test]
    fn repeated_failures_stop_instead_of_looping_forever() {
        // Um subsistema que cai sempre não vai se consertar: insistir só
        // esconderia o problema.
        let (sender, rx) = collector();
        let sup = Supervised::spawn(Subsystem::Gateway, sender, |_| {
            panic!("sempre falha");
        })
        .expect("criar thread");

        // O número de quedas vai na notificação: é o que a extensão mostra.
        let deadline = Instant::now() + Duration::from_secs(5);
        let failed = loop {
            assert!(Instant::now() < deadline, "não desistiu");
            if let Ok(line) = rx.recv_timeout(Duration::from_millis(200))
                && line.contains("failed")
            {
                break line;
            }
        };
        let status: serde_json::Value = serde_json::from_str(&failed).expect("json");
        let restarts = status["params"]["restarts"].as_u64().expect("restarts");
        assert!(restarts > u64::from(MAX_RESTARTS), "{status}");
        drop(sup);
    }

    /// Quem desistiu não está de pé: se `is_running` seguisse verdadeiro, quem
    /// sobe o subsistema sob demanda acharia que não há o que fazer.
    #[test]
    fn a_subsystem_that_gave_up_is_not_running() {
        let (sender, rx) = collector();
        let sup = Supervised::spawn(Subsystem::Gateway, sender, |_| {
            panic!("sempre falha");
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "failed"), "não desistiu");
        assert!(wait_until(|| !sup.is_running()));
    }

    #[test]
    fn stopping_ends_the_loop() {
        let (sender, rx) = collector();
        let sup = Supervised::spawn(Subsystem::Engine, sender, |running| {
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "running"), "não subiu");
        sup.stop();
        assert!(wait_for(&rx, "stopped"), "não encerrou");
        assert!(!sup.is_running());
    }

    #[test]
    fn stop_and_join_returns_after_the_loop_ends() {
        let (sender, _rx) = collector();
        let started = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let (begun, flag) = (Arc::clone(&started), Arc::clone(&finished));
        let sup = Supervised::spawn(Subsystem::Gateway, sender, move |running| {
            begun.store(true, Ordering::Relaxed);
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Um trabalho que ainda arruma a casa depois do pedido de parada.
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        })
        .expect("criar thread");

        // Parar antes de o trabalho começar não testaria a espera: o laço nem
        // chamaria o trabalho.
        assert!(
            wait_until(|| started.load(Ordering::Relaxed)),
            "não começou"
        );
        sup.stop_and_join();
        assert!(
            finished.load(Ordering::Relaxed),
            "voltou antes do laço sair"
        );
    }

    #[test]
    fn one_subsystem_falling_does_not_affect_the_other() {
        // É a razão de cada um ter a própria thread e o próprio contador.
        let (sender, rx) = collector();
        let alive = Arc::new(AtomicBool::new(false));

        let flag = Arc::clone(&alive);
        let healthy = Supervised::spawn(Subsystem::Engine, sender.clone(), move |running| {
            flag.store(true, Ordering::Relaxed);
            while running.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .expect("criar thread");
        let _broken = Supervised::spawn(Subsystem::Gateway, sender, |_| {
            panic!("cai sempre");
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "failed"), "o quebrado não desistiu");
        assert!(
            wait_until(|| alive.load(Ordering::Relaxed)),
            "o saudável não subiu"
        );
        assert!(healthy.is_running(), "o saudável foi derrubado junto");
        healthy.stop();
    }

    #[test]
    fn the_status_carries_the_subsystem_name() {
        // A extensão precisa saber de qual dos dois é o aviso.
        let status = Status {
            subsystem: Subsystem::Engine,
            health: Health::Running,
            restarts: 0,
        };
        let json = serde_json::to_value(status).expect("serializar");
        assert_eq!(json["subsystem"], "engine");
        assert_eq!(json["health"], "running");
    }
}
