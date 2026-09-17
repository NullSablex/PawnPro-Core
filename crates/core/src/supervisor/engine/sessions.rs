//! As sessões LSP da engine.
//!
//! As conexões chegam pelo gateway, já apresentadas como `lsp`, e esperam numa
//! fila até o laço supervisionado as pegar. Cada uma vira uma tarefa no runtime
//! do núcleo; um panic numa delas derruba o laço, e o supervisor sobe a engine
//! de novo com as sessões encerradas.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pawnpro_engine::SettingsReceiver;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::gateway::{Route, Stream};
use crate::supervisor::Supervised;

/// De quanto em quanto tempo o laço volta a olhar o sinalizador de parada e as
/// sessões que caíram.
const POLL: Duration = Duration::from_millis(200);

/// A tarefa supervisionada da engine, dividida entre o serviço e a rota.
pub type EngineTask = Arc<Mutex<Option<Supervised>>>;

/// Entrega as conexões `lsp` à fila da engine.
pub struct EngineRoute {
    queue: Sender<Stream>,
    task: EngineTask,
}

impl EngineRoute {
    pub const fn new(queue: Sender<Stream>, task: EngineTask) -> Self {
        Self { queue, task }
    }
}

impl Route for EngineRoute {
    /// Enfileira enquanto a engine deve estar de pé — inclusive no intervalo
    /// de um reinício, em que a conexão espera o laço voltar. Parada ou
    /// desistente, fecha a conexão na hora, em vez de deixar o cliente esperando
    /// uma resposta que não vem.
    fn accept(&self, stream: Stream, _argument: Option<&str>) {
        let running = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(Supervised::is_running);
        if !running {
            crate::diag_warn!(
                "core/engine",
                "conexão LSP recusada: a engine não está de pé"
            );
            return;
        }
        // Falha só se o serviço acabou; a conexão cai junto, como deve.
        let _ = self.queue.send(stream);
    }
}

/// O que o laço supervisionado precisa para atender.
pub struct EngineLoop {
    pub incoming: Arc<Mutex<Receiver<Stream>>>,
    pub handle: Handle,
    pub settings: SettingsReceiver,
}

impl EngineLoop {
    /// Atende sessões enquanto o sinalizador estiver ligado.
    ///
    /// # Panics
    /// Repassa o panic de uma sessão, para o supervisor tratá-lo como queda.
    /// Sem isso a engine ficaria de pé sem atender.
    pub fn serve(&self, running: &AtomicBool) {
        let incoming = self.incoming.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sessions = Sessions::new(&self.settings, &self.handle);
        while running.load(Ordering::Relaxed) {
            match incoming.recv_timeout(POLL) {
                Ok(stream) => sessions.accept(stream),
                Err(RecvTimeoutError::Timeout) => {}
                // A rota sumiu junto com o serviço: não há mais o que atender.
                Err(RecvTimeoutError::Disconnected) => return,
            }
            sessions.check();
        }
        // Parada a pedido: o que chegou no instante da parada não é atendido
        // agora, e não pode ser atendido por um início futuro, com o cliente
        // já tendo desistido. Fechar é a resposta certa.
        while incoming.try_recv().is_ok() {}
    }
}

/// As sessões abertas e o aviso de que alguma caiu.
struct Sessions {
    open: Vec<JoinHandle<()>>,
    crashed: Arc<AtomicBool>,
    settings: SettingsReceiver,
    handle: Handle,
}

impl Sessions {
    fn new(settings: &SettingsReceiver, handle: &Handle) -> Self {
        Self {
            open: Vec::new(),
            crashed: Arc::new(AtomicBool::new(false)),
            settings: settings.clone(),
            handle: handle.clone(),
        }
    }

    /// Entrega a conexão à engine numa tarefa própria.
    fn accept(&mut self, stream: Stream) {
        let (input, output) = tokio::io::split(stream);
        let guard = SessionGuard(Arc::clone(&self.crashed));
        let settings = self.settings.clone();
        self.open.push(self.handle.spawn(async move {
            pawnpro_engine::serve(input, output, settings).await;
            drop(guard);
        }));
    }

    /// Descarta as terminadas e repassa o panic de qualquer uma delas.
    ///
    /// # Panics
    /// Quando uma sessão caiu: é o que faz o supervisor reiniciar a engine.
    fn check(&mut self) {
        self.open.retain(|session| !session.is_finished());
        assert!(
            !self.crashed.load(Ordering::Relaxed),
            "uma sessão da engine caiu em panic"
        );
    }
}

impl Drop for Sessions {
    /// Encerra o que ainda estiver aberto, também quando o laço sai por panic.
    ///
    /// O runtime é do núcleo e sobrevive ao reinício: sem abortar aqui, as
    /// sessões da tentativa anterior continuariam rodando.
    fn drop(&mut self) {
        for session in &self.open {
            session.abort();
        }
    }
}

/// Acusa o panic de uma sessão para o laço que a criou.
///
/// A tarefa do tokio engole o unwind: sem este aviso, a engine ficaria de pé
/// sem ninguém para atender.
struct SessionGuard(Arc<AtomicBool>);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}
