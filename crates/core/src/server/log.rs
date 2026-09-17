//! Leitura incremental do log do servidor.
//!
//! O servidor escreve num arquivo sem notificar ninguém: acompanhar é comparar
//! o tamanho de tempos em tempos e ler o que cresceu. A leitura não guarda
//! estado — quem acompanha guarda a posição e a devolve na chamada seguinte —,
//! e exibir é de quem tem a interface.

use std::path::Path;

use serde::Serialize;

/// O que cresceu no log desde a última leitura.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogChunk {
    /// Tamanho do arquivo agora: a posição para a próxima leitura.
    pub size: u64,
    /// O texto novo, já decodificado. Vazio sem novidade.
    pub text: String,
}

/// Lê o que o servidor escreveu desde `from`.
///
/// Sem `from`, só mede o arquivo: abrir o painel não deve despejar o log
/// inteiro de execuções anteriores. Um arquivo que encolheu foi recriado pelo
/// servidor ao reiniciar, e a leitura recomeça do início dele — o que está lá é
/// o log novo. Arquivo ausente é tamanho zero: o servidor ainda não o criou.
#[must_use]
pub fn read_since(path: &Path, from: Option<u64>, encoding: &str) -> LogChunk {
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    let Some(from) = from else {
        return LogChunk {
            size,
            text: String::new(),
        };
    };
    let start = if size < from { 0 } else { from };
    let text = read_range(path, start, size)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| crate::compiler::build::decode_output(&bytes, encoding))
        .unwrap_or_default();
    LogChunk { size, text }
}

/// Lê um intervalo de bytes do arquivo.
fn read_range(path: &Path, from: u64, to: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let len = usize::try_from(to.checked_sub(from)?).ok()?;
    if len == 0 {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = vec![0u8; len];
    // `read` parcial é normal: o arquivo pode ter crescido entre a medição e
    // a leitura.
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("relógio")
                .as_nanos();
            p.push(format!("pawnpro-log-{tag}-{nanos}.txt"));
            Self(p)
        }
        fn append(&self, bytes: &[u8]) {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.0)
                .expect("abrir");
            f.write_all(bytes).expect("escrever");
        }
        fn truncate(&self, body: &[u8]) {
            std::fs::write(&self.0, body).expect("truncar");
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn starts_from_the_end_of_an_existing_file() {
        // Abrir o painel não deve despejar o log inteiro de execuções
        // anteriores.
        let tmp = TempFile::new("tail-end");
        tmp.append(b"linha antiga\n");
        let first = read_since(&tmp.0, None, "windows1252");
        assert_eq!(first.text, "");
        assert_eq!(first.size, 13);
    }

    #[test]
    fn reads_only_what_was_appended() {
        let tmp = TempFile::new("append");
        tmp.append(b"antes\n");
        let start = read_since(&tmp.0, None, "windows1252").size;
        tmp.append(b"depois\n");
        let chunk = read_since(&tmp.0, Some(start), "windows1252");
        assert_eq!(chunk.text, "depois\n");
        // Sem novidade, nada é devolvido de novo.
        assert_eq!(read_since(&tmp.0, Some(chunk.size), "windows1252").text, "");
    }

    #[test]
    fn a_truncated_log_restarts_from_the_new_file() {
        // O servidor reiniciou e recriou o arquivo: ler da posição antiga
        // devolveria lixo, e pular o começo perderia o que o servidor novo já
        // escreveu.
        let tmp = TempFile::new("truncate");
        tmp.append(b"conteudo longo anterior\n");
        let start = read_since(&tmp.0, None, "windows1252").size;
        tmp.truncate(b"novo\n");
        assert_eq!(
            read_since(&tmp.0, Some(start), "windows1252").text,
            "novo\n"
        );
    }

    #[test]
    fn output_is_decoded_with_the_configured_encoding() {
        // O servidor escreve em windows-1252: lido como UTF-8, o acento viraria
        // lixo no meio da mensagem.
        let tmp = TempFile::new("encoding");
        tmp.append(&[b'a', 0xE7, 0xE3, b'o', b'\n']);
        assert_eq!(read_since(&tmp.0, Some(0), "windows1252").text, "ação\n");
    }

    #[test]
    fn a_missing_file_yields_nothing_until_it_appears() {
        let tmp = TempFile::new("missing");
        let start = read_since(&tmp.0, None, "windows1252");
        assert_eq!((start.size, start.text.as_str()), (0, ""));
        tmp.append(b"apareceu\n");
        assert_eq!(
            read_since(&tmp.0, Some(start.size), "windows1252").text,
            "apareceu\n"
        );
    }

    #[test]
    fn an_empty_encoding_falls_back_to_the_server_default() {
        let tmp = TempFile::new("default-enc");
        tmp.append(&[0xE7, b'\n']);
        assert_eq!(read_since(&tmp.0, Some(0), "").text, "ç\n");
    }
}
