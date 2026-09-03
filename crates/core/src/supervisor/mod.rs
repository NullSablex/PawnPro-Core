//! Supervisão dos subsistemas.
//!
//! Um panic na engine ou no depurador não pode derrubar os outros nem o core:
//! cada um roda numa thread própria, com `catch_unwind` na borda, e volta a
//! subir sozinho.
//!
//! É por isso que o release não usa `panic = "abort"` — sem unwind o
//! `catch_unwind` não pega nada.

pub mod engine;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    /// DAP e o servidor do jogo.
    Debugger,
}

impl Subsystem {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::Debugger => "debugger",
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
    subsystem: Subsystem,
    /// Ligado enquanto o subsistema deve rodar; desligar pede o encerramento.
    running: Arc<AtomicBool>,
    restarts: Arc<AtomicU32>,
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
        let thread_restarts = Arc::clone(&restarts);
        std::thread::Builder::new()
            .name(format!("pawnpro-{}", subsystem.name()))
            .spawn(move || {
                supervise(subsystem, &sender, &work, &thread_running, &thread_restarts);
            })?;

        Ok(Self {
            subsystem,
            running,
            restarts,
        })
    }

    #[must_use]
    pub fn subsystem(&self) -> Subsystem {
        self.subsystem
    }

    /// Quantas vezes caiu e voltou.
    #[must_use]
    pub fn restarts(&self) -> u32 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// Pede o encerramento e retorna. Quem quiser confirmar observa o
    /// `Health`.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
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
            // Insistir esconderia o problema.
            notify(sender, subsystem, Health::Failed, count);
            return;
        }

        let _ = panicked;
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
        // Espera a SEGUNDA tentativa entrar em `running`: parar antes disso
        // encerraria o laço no meio do intervalo de reinício.
        assert!(wait_for(&rx, "running"), "não subiu de novo");
        sup.stop();
        assert!(attempts.load(Ordering::Relaxed) >= 2, "não tentou de novo");
    }

    #[test]
    fn repeated_failures_stop_instead_of_looping_forever() {
        // Um subsistema que cai sempre não vai se consertar: insistir só
        // esconderia o problema.
        let (sender, rx) = collector();
        let sup = Supervised::spawn(Subsystem::Debugger, sender, |_| {
            panic!("sempre falha");
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "failed"), "não desistiu");
        assert!(sup.restarts() > MAX_RESTARTS);
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
        let _broken = Supervised::spawn(Subsystem::Debugger, sender, |_| {
            panic!("cai sempre");
        })
        .expect("criar thread");

        assert!(wait_for(&rx, "failed"), "o quebrado não desistiu");
        assert!(alive.load(Ordering::Relaxed), "o saudável não subiu");
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
