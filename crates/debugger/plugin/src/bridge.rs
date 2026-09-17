//! Ponte plugin↔núcleo pelo soquete local do núcleo (Unix socket / named pipe).
//! Roda numa thread separada da VM: conecta, se apresenta com o id da sessão,
//! lê [`Command`]s e os aplica ao estado; o hook usa [`Bridge::send`] para
//! avisar a pausa e [`PauseGate`] para bloquear.
//!
//! Esta camada é I/O fina; a decisão está em [`crate::control`],
//! [`crate::gate`] e [`crate::inspect`] (testáveis). O que se testa aqui é
//! o que vem do ambiente; a conexão em si é exercitada pelos testes do núcleo.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{GenericFilePath, Stream as LocalStream, ToFsName};
use pawnpro_dbg_protocol::messages::{self, MsgKey};
use pawnpro_dbg_protocol::transport::{self, env};
use pawnpro_dbg_protocol::{self as wire, Command, Event, Step};

use crate::control::StepMode;
use crate::gate::{PauseGate, Resume};

/// Metade de envio do socket local — para escrever eventos à sessão.
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

/// Onde conectar e com que id se apresentar.
#[derive(Debug, PartialEq, Eq)]
pub struct Session {
    pub endpoint: String,
    pub id: String,
}

impl Session {
    /// Lê a sessão do ambiente. `None` quando o servidor não foi subido por
    /// uma sessão de depuração do PawnPro.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let present = |name| var(name).filter(|value| !value.is_empty());
        Some(Self {
            endpoint: present(env::ENDPOINT)?,
            id: present(env::SESSION)?,
        })
    }
}

/// Conecta na sessão numa thread e passa a atendê-la. Chamar uma vez no
/// `on_load` do plugin.
///
/// Sem sessão, ou sem conseguir conectar, libera a carga da VM na hora: não
/// há quem vá mandar a configuração, e segurar o servidor pelo prazo inteiro
/// seria atrasá-lo à toa.
pub fn start(session: Option<Session>) {
    let Some(session) = session else {
        BRIDGE.mark_configured();
        return;
    };
    thread::spawn(move || match connect(&session) {
        Ok(stream) => handle_client(stream),
        Err(e) => {
            let error = e.to_string();
            eprintln!(
                "{}",
                messages::format(
                    crate::hook::locale(),
                    MsgKey::PluginConnectFailed,
                    &[&session.endpoint, &session.id, &error],
                )
            );
            BRIDGE.mark_configured();
        }
    });
}

/// Conecta no núcleo e se apresenta.
///
/// Não precisa de nova tentativa: o núcleo já atende antes de subir o servidor.
fn connect(session: &Session) -> io::Result<LocalStream> {
    let name = session.endpoint.as_str().to_fs_name::<GenericFilePath>()?;
    let mut stream = LocalStream::connect(name)?;
    stream.write_all(transport::plugin_greeting(&session.id).as_bytes())?;
    Ok(stream)
}

/// Atende a sessão conectada: separa o stream em leitura/escrita, guarda a
/// metade de envio e lê comandos linha a linha até desconectar.
fn handle_client(stream: LocalStream) {
    let (recv, send) = stream.split();
    if let Ok(mut guard) = BRIDGE.out.lock() {
        *guard = Some(send);
    }
    // Antes de qualquer outra coisa: quem somos. A sessão compara com a
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
    // A sessão saiu: limpa a metade de envio e libera qualquer VM ainda em espera
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
            id,
            frame,
            name,
            path,
            value,
        } => {
            // Aplica na pausa atual, no frame selecionado, e confirma: o editor só
            // mostra o valor novo se ele de fato foi gravado.
            let ok = crate::hook::set_variable(frame, &name, &path, value).is_some();
            BRIDGE.send(&Event::VariableSet { id, ok });
        }
        Command::SetDataBreakpoints { watches } => crate::hook::set_data_breakpoints(watches),
        Command::SetExceptionFilter { runtime } => crate::hook::set_runtime_errors(runtime),
        Command::ReadMemory {
            id,
            frame,
            name,
            path,
            offset,
            count,
        } => crate::hook::read_memory(id, frame, &name, &path, offset, count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn session_comes_from_the_environment() {
        let session = Session::from_env(vars(&[
            (env::ENDPOINT, "/run/user/1000/pawnpro-core-1-0/core.sock"),
            (env::SESSION, "7"),
        ]));
        assert_eq!(
            session,
            Some(Session {
                endpoint: "/run/user/1000/pawnpro-core-1-0/core.sock".into(),
                id: "7".into(),
            })
        );
    }

    /// Servidor subido fora do PawnPro com o plugin instalado: não há sessão,
    /// e o plugin não pode ficar esperando uma.
    #[test]
    fn no_session_without_endpoint_and_id() {
        assert_eq!(Session::from_env(vars(&[])), None);
        assert_eq!(Session::from_env(vars(&[(env::SESSION, "7")])), None);
        assert_eq!(
            Session::from_env(vars(&[(env::ENDPOINT, "/x"), (env::SESSION, "")])),
            None
        );
    }
}
