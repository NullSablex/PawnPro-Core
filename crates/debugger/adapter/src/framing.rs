//! Enquadramento de mensagens DAP sobre um stream (igual ao LSP): cada mensagem
//! é `Content-Length: N\r\n\r\n` seguida de `N` bytes de JSON UTF-8.

use std::io::{self, BufRead, Write};

/// Lê uma mensagem DAP do `reader`. `Ok(None)` no fim do stream (EOF limpo).
///
/// # Errors
/// Erro de I/O, cabeçalho malformado ou `Content-Length` ausente/inválido.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // linha em branco encerra os cabeçalhos
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
        // Outros cabeçalhos (ex.: Content-Type) são ignorados.
    }

    let len = content_length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Content-Length ausente"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Escreve uma mensagem DAP (corpo JSON já serializado) no `writer`, com o
/// cabeçalho `Content-Length`, e dá flush.
///
/// # Errors
/// Erro de I/O ao escrever ou dar flush.
pub fn write_message<W: Write>(writer: &mut W, body: &str) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_framed_message() {
        let body = "{\"seq\":1,\"a\":2}"; // 15 bytes
        let raw = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut r = Cursor::new(raw.into_bytes());
        let msg = read_message(&mut r).unwrap().unwrap();
        assert_eq!(msg, body);
    }

    #[test]
    fn eof_returns_none() {
        let mut r = Cursor::new(Vec::new());
        assert!(read_message(&mut r).unwrap().is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let body = "{\"hello\":\"world\"}";
        let mut out = Vec::new();
        write_message(&mut out, body).unwrap();
        let mut r = Cursor::new(out);
        assert_eq!(read_message(&mut r).unwrap().unwrap(), body);
    }
}
