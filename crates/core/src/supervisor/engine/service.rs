//! A engine como subsistema: a thread que atende as sessões e a configuração
//! que o core entrega.
//!
//! Três coisas com tempos de vida diferentes. O endereço é do gateway e vive
//! enquanto o core viver; a thread supervisionada é o que reinicia; a
//! configuração muda quando o desenvolvedor edita um arquivo. É por isso que
//! uma queda da engine não obriga a extensão a perguntar o endereço de novo,
//! nem faz a configuração se perder.
//!
//! O endereço é reservado sob demanda, e não na partida do core: um soquete que
//! não pode ser criado não deve impedir o RCON e o controle de processos de
//! funcionarem.
//!
//! A configuração não é da engine: ela assina o [`ConfigService`], que é quem
//! observa os arquivos, e recebe cada mudança como qualquer outro assinante.

use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};

use pawnpro_engine::{Settings, SettingsSender, settings_channel};

use crate::config::ConfigManager;
use crate::config::service::ConfigService;
use crate::diagnostics::{self, Level};
use crate::gateway::{Channel, Gateway, Stream};
use crate::rpc::Sender;
use crate::supervisor::{Subsystem, Supervised};

use super::sessions::{EngineLoop, EngineRoute, EngineTask};
use super::settings::build_settings;

/// A engine hospedada, supervisionada e alimentada pelo core.
pub struct EngineService {
    /// Por onde as conexões LSP chegam.
    gateway: Arc<Gateway>,
    /// A fila que a rota `lsp` alimenta. Sobrevive aos reinícios da engine.
    incoming: Arc<Mutex<mpsc::Receiver<Stream>>>,
    /// `None` enquanto não foi iniciada. A rota também olha: é o que diz se
    /// uma conexão pode esperar na fila.
    task: EngineTask,
    /// Por onde a configuração chega até a engine.
    settings: SettingsSender,
    /// De onde vem o projeto aberto e a configuração dele.
    config: Arc<ConfigService>,
    /// Idioma do editor, usado quando a configuração não fixa um. Compartilhado
    /// com o assinante, que monta as settings a cada mudança.
    editor_language: Arc<Mutex<String>>,
}

impl Default for EngineService {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineService {
    /// Uma engine com a sua própria configuração e o seu próprio gateway.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ConfigService::new(), Gateway::new())
    }

    /// Uma engine alimentada pela configuração dada, atendendo pelo gateway
    /// dado.
    #[must_use]
    pub fn with_config(config: Arc<ConfigService>, gateway: Arc<Gateway>) -> Self {
        // A engine não sabe escrever log: manda para cá, e o core resolve onde
        // isso vai parar e se vai.
        pawnpro_engine::set_log_sink(|level, message| {
            diagnostics::write(Level::from_name(level), "engine/lsp", message);
        });
        let (settings, _) = settings_channel(Settings::default());
        let editor_language = Arc::new(Mutex::new(String::new()));

        let delivery = settings.clone();
        let language = Arc::clone(&editor_language);
        config.subscribe(Box::new(move |manager| {
            deliver(&delivery, manager, &language);
        }));

        let (queue, incoming) = mpsc::channel();
        let task = EngineTask::default();
        gateway.route(
            Channel::Lsp,
            Arc::new(EngineRoute::new(queue, Arc::clone(&task))),
        );

        Self {
            gateway,
            incoming: Arc::new(Mutex::new(incoming)),
            task,
            settings,
            config,
            editor_language,
        }
    }

    /// Sobe a engine para um projeto, se ainda não estiver de pé, e devolve o
    /// endereço.
    ///
    /// Abre o projeto **antes** de subir: a engine nunca chega a atender sem
    /// saber onde estão os includes. Chamar duas vezes não cria uma segunda
    /// thread — só relê a configuração, o que atualiza o projeto aberto.
    ///
    /// # Errors
    /// Falha do sistema ao reservar o endereço ou ao criar a thread.
    pub fn start(
        &self,
        sender: &Sender,
        workspace_root: &Path,
        editor_language: &str,
    ) -> io::Result<String> {
        if let Ok(mut language) = self.editor_language.lock() {
            *language = editor_language.to_string();
        }
        self.config.open(workspace_root);

        let address = self.gateway.open(sender)?;

        {
            let mut task = self
                .task
                .lock()
                .map_err(|_| io::Error::other("estado da engine corrompido"))?;
            if task.as_ref().is_some_and(Supervised::is_running) {
                return Ok(address);
            }
            let engine = EngineLoop {
                incoming: Arc::clone(&self.incoming),
                handle: self.gateway.handle(),
                settings: self.settings.subscribe(),
            };
            *task = Some(Supervised::spawn(
                Subsystem::Engine,
                sender.clone(),
                move |running: &AtomicBool| engine.serve(running),
            )?);
        }
        crate::diag_info!("core/engine", "atendendo em {address}");
        Ok(address)
    }

    /// Pede o encerramento. O endereço é do gateway e continua valendo para um
    /// novo início.
    pub fn stop(&self) {
        if let Ok(task) = self.task.lock()
            && let Some(task) = task.as_ref()
        {
            task.stop();
        }
    }
}

impl Drop for EngineService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Monta as settings a partir da configuração e as entrega à engine.
fn deliver(settings: &SettingsSender, manager: &ConfigManager, language: &Mutex<String>) {
    let language = language.lock().map(|l| l.clone()).unwrap_or_default();
    let built = build_settings(manager.get_all(), manager.project_root(), &language);
    crate::diag_info!(
        "core/engine",
        "configuração entregue: {} caminho(s) de include, sdk={}",
        built.include_paths.as_ref().map_or(0, Vec::len),
        built
            .sdk_file
            .as_ref()
            .and_then(|p| p.as_ref())
            .map_or_else(|| "nenhum".to_string(), |p| p.display().to_string())
    );
    // `send_replace`, e não `send`: a primeira entrega acontece antes de a
    // engine subir, e um `send` sem ouvintes descartaria o valor — a sessão
    // seguinte encontraria o canal ainda no padrão.
    settings.send_replace(built);
}
