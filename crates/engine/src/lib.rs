//! Engine do PawnPro: análise de Pawn e servidor LSP.
//!
//! Não abre transporte nem lê configuração por conta própria. Quem chama
//! entrega o par de fluxos e o canal de configuração — na prática, o core, que
//! é quem possui os arquivos e sabe quando eles mudam. A engine nunca decide
//! nada sozinha: sem entrega, fica no padrão dela.

mod analyzer;
mod config;
mod intellisense;
mod messages;
mod naming;
mod parser;
#[cfg(test)]
mod probe;
mod server;
mod similar;
mod text;
mod util;
mod workspace;

use std::sync::OnceLock;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;
use tower_lsp::{LspService, Server};

pub use config::{NamingConfig, StyleConfig};
pub use intellisense::{BracePlacement, FormatStyle, Preset};
pub use messages::Locale;
pub use server::Settings;

/// Para onde a engine manda o que registra.
///
/// Recebe o nível (`error`, `warn`, `info`) e a mensagem. A engine não sabe
/// onde isso vai parar — quem escreve o arquivo é o core, que é o dono do
/// diagnóstico. Sem sink, nada é registrado.
type LogSink = Box<dyn Fn(&str, &str) + Send + Sync>;

static LOG: OnceLock<LogSink> = OnceLock::new();

/// Define para onde vai o que a engine registra. Só a primeira chamada vale.
pub fn set_log_sink(sink: impl Fn(&str, &str) + Send + Sync + 'static) {
    let _ = LOG.set(Box::new(sink));
}

/// Registra um evento, se houver para onde mandar.
pub(crate) fn log(level: &str, message: &str) {
    if let Some(sink) = LOG.get() {
        sink(level, message);
    }
}

/// A ponta pela qual o core entrega configuração.
///
/// Cada entrega substitui a anterior e é aplicada de imediato: o que muda
/// diagnóstico faz a engine republicar sem o editor pedir.
pub type SettingsSender = watch::Sender<Settings>;

/// A ponta que a engine lê. Clonável: uma sessão por conexão, todas seguindo a
/// mesma configuração.
pub type SettingsReceiver = watch::Receiver<Settings>;

/// Abre o canal de configuração entre o core e a engine.
#[must_use]
pub fn settings_channel(initial: Settings) -> (SettingsSender, SettingsReceiver) {
    watch::channel(initial)
}

/// Atende uma sessão LSP no par de fluxos dado e retorna quando ela termina.
///
/// Cada chamada cria um servidor com estado próprio: duas sessões não
/// compartilham o workspace analisado, mas compartilham a configuração.
pub async fn serve<I, O>(input: I, output: O, settings: SettingsReceiver)
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (service, socket) =
        LspService::new(move |client| server::PawnProServer::new(client, settings.clone()));
    Server::new(input, output, socket).serve(service).await;
}
