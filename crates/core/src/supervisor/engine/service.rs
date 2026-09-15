//! A engine como subsistema: o endereço, a thread que o atende e a
//! configuração que o core entrega.
//!
//! Três coisas com tempos de vida diferentes. O endereço é reservado uma vez e
//! vive enquanto o core viver; a thread supervisionada é o que reinicia; a
//! configuração muda quando o desenvolvedor edita um arquivo. É por isso que
//! uma queda da engine não obriga a extensão a perguntar o endereço de novo,
//! nem faz a configuração se perder.
//!
//! A reserva é sob demanda, e não na partida do core: um soquete que não pode
//! ser criado não deve impedir o RCON e o controle de processos de funcionarem.
//!
//! A configuração não é da engine: ela assina o [`ConfigService`], que é quem
//! observa os arquivos, e recebe cada mudança como qualquer outro assinante.

use std::io;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use pawnpro_engine::{Settings, SettingsSender, settings_channel};

use crate::config::ConfigManager;
use crate::config::service::ConfigService;
use crate::diagnostics::{self, Level};
use crate::rpc::Sender;
use crate::supervisor::{Subsystem, Supervised};

use super::host::EngineHost;
use super::settings::build_settings;

/// A engine hospedada, supervisionada e alimentada pelo core.
pub struct EngineService {
    /// `None` até a primeira reserva; depois disso, o mesmo até o core morrer.
    host: Mutex<Option<Arc<EngineHost>>>,
    /// `None` enquanto não foi iniciada.
    task: Mutex<Option<Supervised>>,
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
    /// Uma engine com a sua própria configuração.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ConfigService::new())
    }

    /// Uma engine alimentada pela configuração dada.
    #[must_use]
    pub fn with_config(config: Arc<ConfigService>) -> Self {
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

        Self {
            host: Mutex::new(None),
            task: Mutex::new(None),
            settings,
            config,
            editor_language,
        }
    }

    /// Reserva o endereço, ou devolve o que já está reservado.
    ///
    /// # Errors
    /// Falha do sistema ao criar o diretório privado ou o soquete.
    fn ensure_host(&self) -> io::Result<Arc<EngineHost>> {
        // A trava cobre o `bind`: soltá-la antes abriria espaço para duas
        // reservas concorrentes, e um dos soquetes ficaria órfão.
        let mut host = self
            .host
            .lock()
            .map_err(|_| io::Error::other("estado da engine corrompido"))?;
        let reserved = if let Some(existing) = host.as_ref() {
            Arc::clone(existing)
        } else {
            let created = Arc::new(EngineHost::bind(self.settings.subscribe())?);
            *host = Some(Arc::clone(&created));
            created
        };
        drop(host);
        Ok(reserved)
    }

    /// O endereço reservado, se já houver um.
    #[must_use]
    pub fn address(&self) -> Option<String> {
        self.host
            .lock()
            .ok()
            .and_then(|host| host.as_ref().map(|h| h.address().to_string()))
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

        let host = self.ensure_host()?;
        let address = host.address().to_string();

        {
            let mut task = self
                .task
                .lock()
                .map_err(|_| io::Error::other("estado da engine corrompido"))?;
            if task.as_ref().is_some_and(Supervised::is_running) {
                return Ok(address);
            }
            *task = Some(Supervised::spawn(
                Subsystem::Engine,
                sender.clone(),
                move |running: &AtomicBool| host.serve(running),
            )?);
        }
        crate::diag_info!("core/engine", "atendendo em {address}");
        Ok(address)
    }

    /// Relê a configuração do projeto e a entrega, sem esperar o observador.
    pub fn deliver_settings(&self) {
        self.config.reload();
    }

    /// Pede o encerramento. O endereço continua reservado para um novo início.
    pub fn stop(&self) {
        if let Ok(task) = self.task.lock()
            && let Some(task) = task.as_ref()
        {
            task.stop();
        }
    }

    /// `true` se a engine está de pé.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.task
            .lock()
            .is_ok_and(|task| task.as_ref().is_some_and(Supervised::is_running))
    }

    /// Quantas vezes a engine caiu e voltou.
    #[must_use]
    pub fn restarts(&self) -> u32 {
        self.task
            .lock()
            .map_or(0, |task| task.as_ref().map_or(0, Supervised::restarts))
    }
}

impl Drop for EngineService {
    /// Encerra a engine e apaga o soquete.
    ///
    /// A thread supervisionada também segura o host, e pode não ter saído
    /// quando o core encerra: sem esta limpeza o soquete sobreviveria ao
    /// processo.
    fn drop(&mut self) {
        self.stop();
        if let Ok(host) = self.host.lock()
            && let Some(host) = host.as_ref()
        {
            host.release();
        }
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
            .map_or("nenhum".to_string(), |p| p.display().to_string())
    );
    // `send_replace`, e não `send`: a primeira entrega acontece antes de a
    // engine subir, e um `send` sem ouvintes descartaria o valor — a sessão
    // seguinte encontraria o canal ainda no padrão.
    settings.send_replace(built);
}
