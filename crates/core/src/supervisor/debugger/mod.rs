//! O depurador como subsistema: sessões DAP e as conexões dos plugins.
//!
//! Chegam as duas pelo gateway. Uma conexão `dap` vira uma sessão numa thread
//! própria, do `initialize` ao fim; a sessão reserva um canal a cada servidor
//! que sobe, e a conexão `plugin` daquele servidor é entregue a ela pelo id.
//!
//! Não há um laço para o supervisor reiniciar: cada sessão é isolada, e um
//! panic numa delas encerra só aquela — a conexão fecha, o editor vê o fim e o
//! servidor dela cai no `Drop`. Reiniciar as outras não consertaria nada.

use std::collections::HashMap;
use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use pawnpro_dap_adapter::{PluginHost, PluginLink, PluginTicket};
use tokio::runtime::Handle;
use tokio_util::io::SyncIoBridge;

use crate::gateway::{Channel, Gateway, Route, Stream};
use crate::rpc;

/// O depurador ligado ao gateway.
pub struct DebuggerService {
    gateway: Arc<Gateway>,
}

impl DebuggerService {
    /// Passa a atender os canais `dap` e `plugin` do gateway.
    #[must_use]
    pub fn new(gateway: Arc<Gateway>) -> Self {
        let plugins = Arc::new(PluginRegistry::default());
        gateway.route(
            Channel::Dap,
            Arc::new(DapRoute {
                gateway: Arc::downgrade(&gateway),
                plugins: Arc::clone(&plugins),
            }),
        );
        gateway.route(
            Channel::Plugin,
            Arc::new(PluginRoute {
                handle: gateway.handle(),
                plugins,
            }),
        );
        Self { gateway }
    }

    /// Garante que o gateway atende e devolve o endereço.
    ///
    /// # Errors
    /// Falha do sistema ao criar o soquete ou a thread.
    pub fn start(&self, sender: &rpc::Sender) -> io::Result<String> {
        self.gateway.open(sender)
    }
}

/// As reservas abertas, de id para o canal da sessão que espera o plugin.
#[derive(Default)]
struct PluginRegistry {
    next_id: AtomicU64,
    waiting: Mutex<HashMap<u64, Sender<PluginLink>>>,
}

impl PluginRegistry {
    fn waiting(&self) -> MutexGuard<'_, HashMap<u64, Sender<PluginLink>>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Solta a reserva quando a sessão troca de servidor ou acaba.
struct Registration {
    plugins: Arc<PluginRegistry>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.plugins.waiting().remove(&self.id);
    }
}

/// O núcleo, do ponto de vista de uma sessão.
struct CoreHost {
    endpoint: String,
    plugins: Arc<PluginRegistry>,
}

impl PluginHost for CoreHost {
    fn reserve(&self) -> io::Result<PluginTicket> {
        let id = self.plugins.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.plugins.waiting().insert(id, tx);
        Ok(PluginTicket {
            endpoint: self.endpoint.clone(),
            session: id.to_string(),
            link: rx,
            registration: Box::new(Registration {
                plugins: Arc::clone(&self.plugins),
                id,
            }),
        })
    }
}

/// Uma conexão do runtime como leitor e escritor síncronos.
///
/// O adaptador é síncrono e roda em threads próprias; as leituras bloqueiam
/// nelas, não no runtime.
fn bridge(stream: Stream, handle: &Handle) -> PluginLink {
    let (read, write) = tokio::io::split(stream);
    PluginLink {
        reader: Box::new(SyncIoBridge::new_with_handle(read, handle.clone())),
        writer: Box::new(SyncIoBridge::new_with_handle(write, handle.clone())),
    }
}

struct DapRoute {
    /// Fraco: o gateway guarda as rotas, e uma referência forte daqui o
    /// impediria de cair.
    gateway: Weak<Gateway>,
    plugins: Arc<PluginRegistry>,
}

impl Route for DapRoute {
    fn accept(&self, stream: Stream, _argument: Option<&str>) {
        let Some(gateway) = self.gateway.upgrade() else {
            return;
        };
        let Some(endpoint) = gateway.address() else {
            return;
        };
        let link = bridge(stream, &gateway.handle());
        let host = CoreHost {
            endpoint,
            plugins: Arc::clone(&self.plugins),
        };
        // A sessão inteira nesta thread: o servidor que ela sobe morre com a
        // thread que o criou (ver `pawnpro_dap_adapter::serve`).
        let spawned = std::thread::Builder::new()
            .name("pawnpro-dap".into())
            .spawn(move || run_session(link, &host));
        if let Err(error) = spawned {
            crate::diag_error!("core/debugger", "sessão DAP sem thread: {error}");
        }
    }
}

fn run_session(link: PluginLink, host: &CoreHost) {
    crate::diag_info!("core/debugger", "sessão DAP aberta");
    let PluginLink { reader, writer } = link;
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        pawnpro_dap_adapter::serve(reader, writer, host)
    }));
    match result {
        Ok(Ok(())) => crate::diag_info!("core/debugger", "sessão DAP encerrada"),
        Ok(Err(error)) => {
            crate::diag_warn!("core/debugger", "sessão DAP encerrada por erro: {error}");
        }
        Err(_) => crate::diag_error!("core/debugger", "sessão DAP caiu em panic"),
    }
}

struct PluginRoute {
    handle: Handle,
    plugins: Arc<PluginRegistry>,
}

impl Route for PluginRoute {
    fn accept(&self, stream: Stream, argument: Option<&str>) {
        let Some(id) = argument.and_then(|a| a.parse::<u64>().ok()) else {
            crate::diag_warn!(
                "core/debugger",
                "plugin recusado: sessão ausente ou inválida"
            );
            return;
        };
        // Sai do registro: uma reserva atende um servidor só, e um segundo
        // plugin com o mesmo id não rouba a conexão do primeiro.
        let Some(waiting) = self.plugins.waiting().remove(&id) else {
            crate::diag_warn!(
                "core/debugger",
                "plugin recusado: nenhuma sessão espera {id}"
            );
            return;
        };
        if waiting.send(bridge(stream, &self.handle)).is_err() {
            crate::diag_warn!("core/debugger", "plugin recusado: a sessão {id} já acabou");
        }
    }
}
