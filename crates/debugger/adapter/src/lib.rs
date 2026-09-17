//! Adaptador DAP do PawnPro, como biblioteca do núcleo.
//!
//! Traduz Debug Adapter Protocol ⇄ protocolo do plugin, usando
//! `samp_sdk::debug` para mapear linha ↔ endereço. Não faz E/S por conta
//! própria: o núcleo entrega a conexão do editor a [`serve`] e, pelo
//! [`PluginHost`], a conexão do plugin de cada servidor que a sessão sobe.
//!
//! Uma chamada de [`serve`] é uma sessão de depuração, do `initialize` ao fim,
//! inteira na thread que a chamou.

mod dap;
mod expr;
mod frames;
mod framing;
mod plugin;
mod server;
mod session;
mod sources;

use std::io::{self, BufReader, Read, Write};
use std::sync::mpsc::Receiver;

use pawnpro_dbg_protocol::messages::{self, MsgKey};

use dap::{DapOut, Response};
use plugin::{PluginClient, PluginContext};
use server::{ServerChild, spawn_server};
use session::{Outgoing, Session, SpawnSpec};

/// A conexão com o plugin de um servidor, já apresentada ao núcleo.
pub struct PluginLink {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

/// O canal reservado para o plugin de um servidor que a sessão vai subir.
pub struct PluginTicket {
    /// Onde o plugin conecta.
    pub endpoint: String,
    /// Com que id o plugin se apresenta.
    pub session: String,
    /// Por onde a conexão do plugin chega.
    pub link: Receiver<PluginLink>,
    /// Mantém a reserva: soltá-la faz o núcleo recusar este id dali em diante.
    pub registration: Box<dyn Send>,
}

/// O que a sessão pede ao núcleo.
pub trait PluginHost {
    /// Reserva um canal para o plugin do próximo servidor.
    ///
    /// # Errors
    /// Quando o núcleo não consegue oferecer um endereço.
    fn reserve(&self) -> io::Result<PluginTicket>;
}

/// Faixa dos `seq` dos eventos emitidos fora da sessão, longe dos que ela
/// numera a partir de 1.
const OUTSIDE_SEQ_START: i64 = 1_000_000;

/// O que está de pé por causa da sessão. A ordem dos campos é a ordem do
/// `Drop`: primeiro aposenta a conexão, depois derruba o servidor, e só então
/// solta a reserva — assim nada do servidor velho alcança o editor.
#[derive(Default)]
struct Running {
    plugin: Option<PluginClient>,
    server: Option<ServerChild>,
    registration: Option<Box<dyn Send>>,
}

impl Running {
    fn stop(&mut self) {
        self.plugin = None;
        self.server = None;
        self.registration = None;
    }
}

/// Atende uma sessão de depuração até o editor encerrá-la ou fechar a conexão.
///
/// # Errors
/// Erro de E/S na conexão do editor, ou mensagem DAP mal enquadrada.
pub fn serve(
    input: impl Read,
    output: Box<dyn Write + Send>,
    host: &dyn PluginHost,
) -> io::Result<()> {
    let mut reader = BufReader::new(input);
    let out = DapOut::new(output, OUTSIDE_SEQ_START);
    let mut session = Session::new();
    let mut running = Running::default();

    while let Some(raw) = framing::read_message(&mut reader)? {
        // Mensagem que não é um request válido é ignorada, não derruba a sessão.
        let Ok(req) = serde_json::from_str::<dap::Request>(&raw) else {
            continue;
        };
        for outgoing in session.handle(&req) {
            match outgoing {
                Outgoing::Response(response) => out.send(&response),
                Outgoing::Event(event) => out.send(&event),
                Outgoing::SpawnServer(spec) => {
                    if let Err(error) = start_server(&spec, &session, host, &out, &mut running) {
                        let text = messages::format(
                            session.locale(),
                            MsgKey::ServerStartFailed,
                            &[&error.to_string()],
                        );
                        out.event(
                            "output",
                            serde_json::json!({ "category": "stderr", "output": format!("{text}\n") }),
                        );
                        out.event("terminated", serde_json::Value::Null);
                    }
                }
                Outgoing::StopServer => running.stop(),
                Outgoing::WriteVariable(write) => {
                    let written = running.plugin.as_ref().is_some_and(|plugin| {
                        plugin.set_variable(
                            write.frame,
                            write.name.clone(),
                            write.path.clone(),
                            write.value,
                        )
                    });
                    if written {
                        session.frames().update_path(
                            write.frame,
                            write.var,
                            &write.path,
                            &write.shown,
                        );
                        let body =
                            serde_json::json!({ "value": write.shown, "variablesReference": 0 });
                        out.send(&Response::ok(write.seq, &req, body));
                    } else {
                        let detail = messages::format(
                            session.locale(),
                            MsgKey::VariableNotWritten,
                            &[&write.label()],
                        );
                        out.send(&Response::fail(write.seq, &req, detail));
                    }
                }
                Outgoing::ToPlugin(cmd) => {
                    if let Some(plugin) = &running.plugin {
                        plugin.send(&cmd);
                    }
                }
                Outgoing::ReadMemory {
                    seq,
                    address,
                    frame,
                    name,
                    path,
                    offset,
                    count,
                } => {
                    let bytes = running
                        .plugin
                        .as_ref()
                        .and_then(|p| p.read_memory(frame, name, path, offset, count))
                        .unwrap_or_default();
                    let body = serde_json::json!({
                        "address": address,
                        "data": dap::base64_encode(&bytes),
                    });
                    out.send(&Response::ok(seq, &req, body));
                }
            }
        }
        if session.is_terminated() {
            break;
        }
    }
    running.stop();
    Ok(())
}

/// Põe um servidor no lugar do que houver, com um canal novo para o plugin.
fn start_server(
    spec: &SpawnSpec,
    session: &Session,
    host: &dyn PluginHost,
    out: &DapOut,
    running: &mut Running,
) -> io::Result<()> {
    // O servidor velho sai antes de o novo subir: os dois disputariam a porta.
    running.stop();
    // O processo que produziu a última pausa não existe mais.
    session.frames().clear();

    let ticket = host.reserve()?;
    let server = spawn_server(spec, &ticket.endpoint, &ticket.session, out)?;
    running.plugin = Some(PluginClient::attach(
        ticket.link,
        PluginContext {
            out: out.clone(),
            frames: session.frames(),
            locale: session.locale(),
        },
    ));
    running.server = Some(server);
    running.registration = Some(ticket.registration);
    Ok(())
}
