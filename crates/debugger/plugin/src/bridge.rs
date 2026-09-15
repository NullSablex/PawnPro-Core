//! Ponte plugin↔adaptador via socket local (Unix socket / named pipe). Roda numa
//! thread separada da VM: aceita o adaptador, lê [`Command`]s e os aplica ao
//! estado; o hook usa [`Bridge::send`] para avisar a pausa e [`PauseGate`] para
//! bloquear.
//!
//! Esta camada é I/O puro e fina; toda a decisão está em [`crate::control`],
//! [`crate::gate`] e [`crate::inspect`] (testáveis). Por isso não tem testes
//! próprios — exercitar exigiria sockets reais e um servidor.

use std::io::{BufRead, BufReader, Write};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::traits::{ListenerExt, Stream as _};
use pawnpro_dbg_protocol::transport::{self, LocalListener, LocalStream};
use pawnpro_dbg_protocol::{self as wire, Command, Event, Step};

use crate::control::StepMode;
use crate::gate::{PauseGate, Resume};

/// Metade de envio do socket local — para escrever eventos ao adaptador.
type SendHalf = <LocalStream as interprocess::local_socket::traits::Stream>::SendHalf;

/// Estado global da ponte. O hook (thread da VM) e a thread do socket
/// compartilham: o portão de pausa e a metade de envio ao adaptador.
pub struct Bridge {
    gate: PauseGate,
    /// Metade de envio para o adaptador (preenchida ao conectar).
    out: Mutex<Option<SendHalf>>,
    /// `true` quando o adaptador já enviou a configuração inicial (breakpoints).
    /// A primeira VM espera por isto na carga — ver [`Bridge::wait_configured`].
    configured: Mutex<bool>,
    configured_cv: Condvar,
}

impl Bridge {
    const fn new() -> Self {
        Self {
            gate: PauseGate::new(),
            out: Mutex::new(None),
            configured: Mutex::new(false),
            configured_cv: Condvar::new(),
        }
    }

    /// Bloqueia a thread da VM até o adaptador sinalizar `Configured` (breakpoints
    /// já enviados) ou até esgotar `timeout`. Garante que breakpoints em código de
    /// carga (ex.: `OnGameModeInit`) não passem batido por causa do tempo que o
    /// adaptador leva para conectar. O timeout evita travar o servidor se nenhum
    /// adaptador conectar.
    pub fn wait_configured(&self, timeout: Duration) {
        let Ok(guard) = self.configured.lock() else {
            return;
        };
        // `wait_timeout_while` retoma assim que `configured` vira `true`.
        let _ = self
            .configured_cv
            .wait_timeout_while(guard, timeout, |done| !*done);
    }

    /// Marca a configuração inicial como concluída e acorda a VM em espera.
    pub fn mark_configured(&self) {
        if let Ok(mut done) = self.configured.lock() {
            *done = true;
            self.configured_cv.notify_all();
        }
    }

    /// Bloqueia a VM até o adaptador mandar continuar/step.
    pub fn wait_resume(&self) -> Resume {
        self.gate.wait()
    }

    /// Envia um evento ao adaptador (no-op se ninguém conectado).
    pub fn send(&self, ev: &Event) {
        let Ok(line) = wire::to_line(ev) else { return };
        if let Ok(mut guard) = self.out.lock()
            && let Some(stream) = guard.as_mut()
        {
            // Erro de escrita = adaptador caiu; descarta a conexão.
            if stream.write_all(line.as_bytes()).is_err() {
                *guard = None;
            }
        }
    }
}

/// Instância única da ponte (o hook `extern "C"` não tem contexto próprio).
pub static BRIDGE: Bridge = Bridge::new();

/// Sobe a thread que escuta o adaptador no socket local identificado por `id`.
/// Chamar uma vez no `on_load` do plugin.
pub fn start(id: String) {
    thread::spawn(move || {
        // O socket pode estar momentaneamente "em uso" se o servidor anterior
        // (de uma sessão reiniciada) ainda estiver saindo. Tenta por alguns
        // segundos antes de desistir, em vez de falhar de imediato.
        let listener = match listen_with_retry(&id) {
            Ok(l) => {
                eprintln!("[pawnpro-dbg] canal de depuração aberto (sessão {id:?})");
                l
            }
            Err(e) => {
                eprintln!("[pawnpro-dbg] falha ao abrir o canal de depuração (sessão {id:?}): {e}");
                return;
            }
        };
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            handle_client(stream);
        }
    });
}

/// Abre o listener com retry enquanto o erro for `AddrInUse` (socket de uma
/// execução anterior que ainda está liberando). Até ~5 s.
fn listen_with_retry(id: &str) -> std::io::Result<LocalListener> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match transport::listen(id) {
            Ok(l) => return Ok(l),
            Err(e)
                if e.kind() == std::io::ErrorKind::AddrInUse
                    && std::time::Instant::now() < deadline =>
            {
                thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Atende um adaptador conectado: separa o stream em leitura/escrita, guarda a
/// metade de envio e lê comandos linha a linha até desconectar.
fn handle_client(stream: LocalStream) {
    let (recv, send) = stream.split();
    if let Ok(mut guard) = BRIDGE.out.lock() {
        *guard = Some(send);
    }
    // Antes de qualquer outra coisa: quem somos. O adaptador compara com a
    // própria versão e avisa o usuário se forem incompatíveis, em vez de
    // deixar a depuração falhar sem explicação.
    BRIDGE.send(&Event::Hello {
        version: env!("CARGO_PKG_VERSION").to_string(),
    });
    let reader = BufReader::new(recv);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(cmd) = wire::from_line::<Command>(&line) {
            apply(cmd);
        }
    }
    // Cliente saiu: limpa a metade de envio e libera qualquer VM ainda em espera
    // pela configuração (senão a carga ficaria presa até o timeout).
    if let Ok(mut guard) = BRIDGE.out.lock() {
        *guard = None;
    }
    BRIDGE.mark_configured();
}

/// Aplica um comando do adaptador ao estado.
fn apply(cmd: Command) {
    match cmd {
        Command::SetBreakpoints { breakpoints } => crate::hook::set_breakpoints(breakpoints),
        Command::Continue => BRIDGE.gate.resume(Resume::Continue),
        Command::Step { mode } => {
            let m = match mode {
                Step::In => StepMode::In,
                Step::Over => StepMode::Over,
                Step::Out => StepMode::Out,
            };
            BRIDGE.gate.resume(Resume::Step(m));
        }
        Command::Configured => BRIDGE.mark_configured(),
        Command::SetVariable {
            frame,
            name,
            index,
            value,
        } => {
            // Aplica na pausa atual, no frame selecionado. `index` edita um elemento
            // de array; `None`, um escalar. O adaptador responde ao editor de forma
            // otimista; aqui só efetivamos a escrita na VM (no-op se não houver pausa
            // ou a variável não for editável).
            let _ = crate::hook::set_variable(frame, &name, index, value);
        }
        Command::SetDataBreakpoints { watches } => crate::hook::set_data_breakpoints(watches),
        Command::SetExceptionFilter { runtime } => crate::hook::set_runtime_errors(runtime),
        Command::ReadMemory {
            id,
            frame,
            name,
            index,
            offset,
            count,
        } => crate::hook::read_memory(id, frame, &name, index, offset, count),
    }
}
