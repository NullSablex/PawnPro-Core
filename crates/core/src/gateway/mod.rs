//! O ponto de entrada único do núcleo.
//!
//! Um endereço local atende todo mundo que não é o JSON-RPC do stdio: o LSP e
//! o DAP da extensão e o plugin de depuração dentro do servidor. Cada conexão
//! se apresenta ([`greeting`]) e é entregue à rota do canal que declarou.
//!
//! Um endereço só quer dizer um diretório privado, uma permissão, uma limpeza —
//! e nenhum canal com proteção própria, mais fraca, ao lado.
//!
//! O endereço nasce sob demanda e vive enquanto o núcleo viver: a engine cai e
//! volta no mesmo lugar, e uma sessão de depuração não depende dela. O runtime
//! que serve as conexões também é daqui, e não de um subsistema: reiniciar a
//! engine não pode derrubar as sessões de depuração que usam o mesmo soquete.

pub mod endpoint;
pub mod greeting;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use tokio::runtime::{Builder, Handle, Runtime};

use crate::rpc::Sender;
use crate::supervisor::{Subsystem, Supervised};

pub use endpoint::{Connection, Endpoint, Stream};
pub use greeting::Channel;

/// Quem recebe as conexões de um canal.
pub trait Route: Send + Sync {
    /// Recebe uma conexão já apresentada. Roda dentro do runtime do núcleo:
    /// trabalho bloqueante vai para outra thread.
    fn accept(&self, stream: Stream, argument: Option<&str>);
}

type Routes = RwLock<HashMap<Channel, Arc<dyn Route>>>;

/// O soquete único e o runtime que atende as conexões dele.
pub struct Gateway {
    /// `None` só depois do `Drop` começar.
    runtime: Option<Runtime>,
    /// `None` até a primeira reserva; depois, o mesmo até o núcleo morrer.
    endpoint: Mutex<Option<Arc<Endpoint>>>,
    routes: Arc<Routes>,
    task: Mutex<Option<Supervised>>,
}

impl Gateway {
    /// Um ponto de entrada ainda sem endereço.
    ///
    /// # Panics
    /// Quando o sistema não consegue criar as threads do runtime — sem elas o
    /// núcleo não tem como atender ninguém.
    #[must_use]
    pub fn new() -> Arc<Self> {
        let runtime = Builder::new_multi_thread()
            .thread_name("pawnpro-io")
            .enable_all()
            .build()
            .expect("criar o runtime do núcleo");
        Arc::new(Self {
            runtime: Some(runtime),
            endpoint: Mutex::new(None),
            routes: Arc::default(),
            task: Mutex::new(None),
        })
    }

    /// O runtime onde as conexões vivem.
    ///
    /// # Panics
    /// Nunca antes do `Drop`, que é o único a tirar o runtime.
    #[must_use]
    pub fn handle(&self) -> Handle {
        self.runtime
            .as_ref()
            .expect("o runtime só sai no Drop")
            .handle()
            .clone()
    }

    /// Passa a entregar as conexões do canal à rota dada.
    pub fn route(&self, channel: Channel, route: Arc<dyn Route>) {
        self.routes
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(channel, route);
    }

    /// O endereço, se já foi reservado.
    #[must_use]
    pub fn address(&self) -> Option<String> {
        self.endpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|e| e.address().to_string())
    }

    /// Reserva o endereço e passa a atender, se ainda não estiver atendendo.
    /// Devolve o endereço.
    ///
    /// # Errors
    /// Falha do sistema ao criar o soquete ou a thread.
    pub fn open(&self, sender: &Sender) -> io::Result<String> {
        let endpoint = {
            // A trava cobre a criação: duas reservas concorrentes deixariam um
            // soquete órfão.
            let mut slot = self.endpoint.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(existing) = slot.as_ref() {
                Arc::clone(existing)
            } else {
                let created = Arc::new(Endpoint::open()?);
                *slot = Some(Arc::clone(&created));
                created
            }
        };
        let address = endpoint.address().to_string();

        let mut task = self.task.lock().unwrap_or_else(PoisonError::into_inner);
        if task.as_ref().is_some_and(Supervised::is_running) {
            return Ok(address);
        }
        let handle = self.handle();
        let routes = Arc::clone(&self.routes);
        *task = Some(Supervised::spawn(
            Subsystem::Gateway,
            sender.clone(),
            move |running: &AtomicBool| {
                let spawner = handle.clone();
                handle.block_on(endpoint.accept_loop(running, |stream| {
                    spawner.spawn(dispatch(stream, Arc::clone(&routes)));
                }));
            },
        )?);
        drop(task);
        crate::diag_info!("core/gateway", "atendendo em {address}");
        Ok(address)
    }
}

impl Drop for Gateway {
    /// Para de aceitar, apaga o soquete e encerra o runtime.
    ///
    /// O runtime encerrado faz toda leitura pendente nas conexões falhar: as
    /// sessões de depuração saem do laço, e cada uma derruba o próprio servidor.
    fn drop(&mut self) {
        // Esperar a thread sair antes de encerrar o runtime: ela está parada
        // num `block_on` dele, e o timer de um runtime encerrado entra em panic.
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            task.stop_and_join();
        }
        if let Some(endpoint) = self
            .endpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            endpoint.release();
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Lê a apresentação e entrega a conexão a quem cuida do canal.
async fn dispatch(mut stream: Stream, routes: Arc<Routes>) {
    let greeting = match greeting::read(&mut stream).await {
        Ok(greeting) => greeting,
        Err(error) => {
            crate::diag_warn!("core/gateway", "conexão recusada: {error}");
            return;
        }
    };
    let route = routes
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&greeting.channel)
        .cloned();
    match route {
        Some(route) => route.accept(stream, greeting.argument.as_deref()),
        None => crate::diag_warn!(
            "core/gateway",
            "conexão recusada: ninguém atende {:?}",
            greeting.channel
        ),
    }
}
