//! Tipos das mensagens DAP e a saída para o editor.
//!
//! Mantém-se genérico: `arguments`/`body` ficam como `serde_json::Value`,
//! decodificados por comando conforme necessário — evita modelar todo o
//! protocolo de uma vez.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::framing;

/// Mensagem recebida do cliente (o editor). DAP usa `type: "request"`.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub seq: i64,
    pub command: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Resposta a um request (`type: "response"`).
#[derive(Debug, Serialize)]
pub struct Response {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request_seq: i64,
    pub success: bool,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub body: Value,
}

impl Response {
    #[must_use]
    pub fn ok(seq: i64, req: &Request, body: Value) -> Self {
        Self {
            seq,
            kind: "response",
            request_seq: req.seq,
            success: true,
            command: req.command.clone(),
            message: None,
            body,
        }
    }

    #[must_use]
    pub fn fail(seq: i64, req: &Request, message: impl Into<String>) -> Self {
        Self {
            seq,
            kind: "response",
            request_seq: req.seq,
            success: false,
            command: req.command.clone(),
            message: Some(message.into()),
            body: Value::Null,
        }
    }
}

/// Evento enviado ao cliente (`type: "event"`), ex.: `stopped`, `terminated`.
#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)] // `event` é o nome do campo no protocolo DAP
pub struct Event {
    pub seq: i64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub event: &'static str,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub body: Value,
}

impl Event {
    #[must_use]
    pub const fn new(seq: i64, event: &'static str, body: Value) -> Self {
        Self {
            seq,
            kind: "event",
            event,
            body,
        }
    }
}

/// Saída DAP compartilhada entre o laço da sessão e as threads que recebem
/// eventos do plugin e o console do servidor.
///
/// A trava serializa as mensagens: sem ela, duas threads escrevendo ao mesmo
/// tempo intercalariam cabeçalho e corpo, e o editor perderia o enquadramento.
#[derive(Clone)]
pub struct DapOut {
    inner: Arc<Mutex<DapOutInner>>,
}

struct DapOutInner {
    /// `seq` dos eventos emitidos fora da sessão, numa faixa própria para não
    /// colidir com os que a sessão numera a partir de 1.
    seq: i64,
    writer: Box<dyn Write + Send>,
}

impl DapOut {
    #[must_use]
    pub fn new(writer: Box<dyn Write + Send>, start_seq: i64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DapOutInner {
                seq: start_seq,
                writer,
            })),
        }
    }

    /// Emite um evento DAP (`stopped`, `output`, …) com `seq` próprio.
    pub fn event(&self, event: &'static str, body: Value) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.seq += 1;
        if let Ok(text) = serde_json::to_string(&Event::new(inner.seq, event, body)) {
            let _ = framing::write_message(&mut inner.writer, &text);
        }
    }

    /// Emite uma resposta ou evento já numerado pela sessão.
    pub fn send<T: Serialize>(&self, message: &T) {
        if let Ok(text) = serde_json::to_string(message) {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = framing::write_message(&mut inner.writer, &text);
        }
    }
}

/// Codifica bytes em base64 (alfabeto padrão) — o campo `data` do `readMemory`
/// do DAP é base64. Evita uma dependência externa para algo tão pequeno.
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |n: u32, shift: u32| char::from(ALPHABET[((n >> shift) & 63) as usize]);
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b1 = u32::from(chunk[0]);
        let b2 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b3 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b1 << 16) | (b2 << 8) | b3;
        out.push(symbol(n, 18));
        out.push(symbol(n, 12));
        out.push(if chunk.len() > 1 { symbol(n, 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(n, 0) } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_matches_rfc() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"M"), "TQ==");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }
}
