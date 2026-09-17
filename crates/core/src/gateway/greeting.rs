//! A apresentação de quem conecta no núcleo.
//!
//! Um soquete só atende a extensão (LSP e DAP) e o plugin dentro do servidor.
//! Quem conecta diz a que veio numa primeira linha, antes de qualquer byte do
//! protocolo:
//!
//! ```text
//! PAWNPRO/1 lsp
//! PAWNPRO/1 dap
//! PAWNPRO/1 plugin <sessão>
//! ```
//!
//! É declaração explícita, não adivinhação pelo conteúdo: LSP e DAP usam o
//! mesmo enquadramento, e distingui-los pelos primeiros bytes seria frágil.

use std::fmt;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Prefixo e versão da apresentação. Mudar o formato muda a versão, para uma
/// ponta velha ser recusada com motivo em vez de mal interpretada.
const MAGIC: &str = "PAWNPRO/1";

/// Tamanho máximo da linha. Quem manda mais que isto não está se apresentando.
const MAX_LEN: usize = 128;

/// Prazo para a apresentação chegar: uma conexão muda não pode ficar ocupando
/// o núcleo para sempre.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// A que canal a conexão pertence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Lsp,
    Dap,
    Plugin,
}

impl Channel {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "lsp" => Some(Self::Lsp),
            "dap" => Some(Self::Dap),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }
}

/// Uma apresentação válida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Greeting {
    pub channel: Channel,
    /// O que veio depois do canal (a sessão, para o plugin).
    pub argument: Option<String>,
}

/// Por que uma apresentação foi recusada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreetingError {
    /// Fechou antes de terminar a linha.
    Closed,
    TimedOut,
    TooLong,
    /// Não começa com `PAWNPRO/`.
    NotAGreeting,
    /// `PAWNPRO/` de outra versão.
    UnsupportedVersion(String),
    UnknownChannel(String),
    Io(String),
}

impl fmt::Display for GreetingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "a conexão fechou antes da apresentação"),
            Self::TimedOut => write!(f, "a apresentação não chegou em {TIMEOUT:?}"),
            Self::TooLong => write!(f, "a apresentação passou de {MAX_LEN} bytes"),
            Self::NotAGreeting => write!(f, "a primeira linha não é uma apresentação"),
            Self::UnsupportedVersion(v) => write!(f, "versão de apresentação não suportada: {v}"),
            Self::UnknownChannel(c) => write!(f, "canal desconhecido: {c}"),
            Self::Io(e) => write!(f, "erro ao ler a apresentação: {e}"),
        }
    }
}

/// Interpreta a linha, sem o `\n`.
///
/// # Errors
/// Quando a linha não é uma apresentação desta versão para um canal conhecido.
pub fn parse(line: &str) -> Result<Greeting, GreetingError> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut parts = line.split(' ');
    let magic = parts.next().unwrap_or_default();
    if magic != MAGIC {
        return Err(if magic.starts_with("PAWNPRO/") {
            GreetingError::UnsupportedVersion(magic.to_string())
        } else {
            GreetingError::NotAGreeting
        });
    }
    let name = parts.next().unwrap_or_default();
    let channel =
        Channel::from_name(name).ok_or_else(|| GreetingError::UnknownChannel(name.to_string()))?;
    let argument = parts.next().filter(|a| !a.is_empty()).map(str::to_string);
    Ok(Greeting { channel, argument })
}

/// Lê a apresentação sem consumir nada além dela.
///
/// Lê byte a byte de propósito: um leitor com buffer levaria junto o começo do
/// protocolo que vem depois, e esses bytes se perderiam na troca de dono.
///
/// # Errors
/// Conexão fechada, prazo estourado, linha longa demais ou inválida.
pub async fn read<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Greeting, GreetingError> {
    let line = tokio::time::timeout(TIMEOUT, read_line(stream))
        .await
        .map_err(|_| GreetingError::TimedOut)??;
    parse(&line)
}

async fn read_line<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, GreetingError> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match stream.read(&mut byte).await {
            Ok(0) => return Err(GreetingError::Closed),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if bytes.len() == MAX_LEN => return Err(GreetingError::TooLong),
            Ok(_) => bytes.push(byte[0]),
            Err(e) => return Err(GreetingError::Io(e.to_string())),
        }
    }
    String::from_utf8(bytes).map_err(|_| GreetingError::NotAGreeting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn parses_each_channel() {
        assert_eq!(
            parse("PAWNPRO/1 lsp"),
            Ok(Greeting {
                channel: Channel::Lsp,
                argument: None
            })
        );
        assert_eq!(parse("PAWNPRO/1 dap").unwrap().channel, Channel::Dap);
        assert_eq!(
            parse("PAWNPRO/1 plugin 42\r"),
            Ok(Greeting {
                channel: Channel::Plugin,
                argument: Some("42".into())
            })
        );
    }

    #[test]
    fn rejects_what_is_not_a_greeting() {
        // O começo de uma mensagem LSP ou DAP sem apresentação.
        assert_eq!(
            parse("Content-Length: 52"),
            Err(GreetingError::NotAGreeting)
        );
        assert_eq!(
            parse("PAWNPRO/2 lsp"),
            Err(GreetingError::UnsupportedVersion("PAWNPRO/2".into()))
        );
        assert_eq!(
            parse("PAWNPRO/1 ftp"),
            Err(GreetingError::UnknownChannel("ftp".into()))
        );
    }

    /// A linha que o plugin gera é a que o núcleo entende: os dois lados vivem
    /// em crates diferentes, e só este teste os amarra.
    #[test]
    fn the_plugin_greeting_is_understood() {
        let line = pawnpro_dbg_protocol::transport::plugin_greeting("42");
        assert_eq!(
            parse(line.strip_suffix('\n').expect("termina a linha")),
            Ok(Greeting {
                channel: Channel::Plugin,
                argument: Some("42".into())
            })
        );
    }

    /// O protocolo começa logo depois da linha: nada dele pode ser consumido
    /// pela leitura da apresentação.
    #[tokio::test]
    async fn reading_stops_exactly_after_the_line() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client
            .write_all(b"PAWNPRO/1 dap\nContent-Length: 2\r\n\r\n{}")
            .await
            .unwrap();
        drop(client);

        assert_eq!(read(&mut server).await.unwrap().channel, Channel::Dap);
        let mut rest = String::new();
        server.read_to_string(&mut rest).await.unwrap();
        assert_eq!(rest, "Content-Length: 2\r\n\r\n{}");
    }

    #[tokio::test]
    async fn a_line_without_end_is_refused() {
        let (mut client, mut server) = tokio::io::duplex(512);
        client.write_all(&[b'a'; MAX_LEN + 1]).await.unwrap();
        assert_eq!(read(&mut server).await, Err(GreetingError::TooLong));
    }

    #[tokio::test]
    async fn a_closed_connection_is_refused() {
        let (client, mut server) = tokio::io::duplex(64);
        drop(client);
        assert_eq!(read(&mut server).await, Err(GreetingError::Closed));
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_connection_times_out() {
        let (_client, mut server) = tokio::io::duplex(64);
        assert_eq!(read(&mut server).await, Err(GreetingError::TimedOut));
    }
}
