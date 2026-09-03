//! Leitura incremental do log do servidor.
//!
//! O servidor escreve num arquivo sem notificar ninguém: acompanhar é comparar
//! o tamanho de tempos em tempos e ler o que cresceu. Exibir é de quem tem a
//! interface.

use std::path::{Path, PathBuf};

/// Ler `metadata` de um arquivo local é barato, e meio segundo já faz a saída
/// parecer imediata.
pub const POLL_INTERVAL_MS: u64 = 500;

/// Acompanha o crescimento de um arquivo de log.
#[derive(Debug)]
pub struct LogTailer {
    file: PathBuf,
    /// Tamanho do arquivo na última leitura.
    last_size: u64,
    encoding: String,
}

impl LogTailer {
    /// Começa a acompanhar a partir do fim: abrir o painel não deve despejar
    /// o log inteiro de execuções anteriores.
    #[must_use]
    pub fn new(file: &Path, encoding: &str) -> Self {
        let last_size = std::fs::metadata(file).map_or(0, |m| m.len());
        Self {
            file: file.to_path_buf(),
            last_size,
            encoding: if encoding.is_empty() {
                "windows1252".to_string()
            } else {
                encoding.to_lowercase()
            },
        }
    }

    /// O arquivo acompanhado.
    #[must_use]
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Evita reiniciar o tail à toa, o que descartaria a posição e faria o
    /// painel repetir conteúdo.
    #[must_use]
    pub fn is_tailing(&self, path: &Path) -> bool {
        self.file == path
    }

    /// Lê o que o servidor escreveu desde a última chamada.
    ///
    /// Um arquivo que encolheu foi truncado pelo servidor ao reiniciar: a
    /// leitura recomeça do zero em vez de ler de uma posição inexistente.
    pub fn read_new(&mut self) -> Option<String> {
        let size = std::fs::metadata(&self.file).ok()?.len();

        if size < self.last_size {
            // Log truncado: recomeça, mas sem devolver o conteúdo antigo.
            self.last_size = 0;
        }
        if size == self.last_size {
            return None;
        }

        let bytes = read_range(&self.file, self.last_size, size)?;
        self.last_size = size;
        if bytes.is_empty() {
            return None;
        }
        Some(crate::compiler::build::decode_output(
            &bytes,
            &self.encoding,
        ))
    }
}

/// Lê um intervalo de bytes do arquivo.
fn read_range(path: &Path, from: u64, to: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let len = usize::try_from(to.checked_sub(from)?).ok()?;
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
        let mut tailer = LogTailer::new(&tmp.0, "windows1252");
        assert_eq!(tailer.read_new(), None);
    }

    #[test]
    fn reads_only_what_was_appended() {
        let tmp = TempFile::new("append");
        tmp.append(b"antes\n");
        let mut tailer = LogTailer::new(&tmp.0, "windows1252");
        tmp.append(b"depois\n");
        assert_eq!(tailer.read_new().as_deref(), Some("depois\n"));
        // Sem novidade, nada é devolvido de novo.
        assert_eq!(tailer.read_new(), None);
    }

    #[test]
    fn a_truncated_log_restarts_without_repeating() {
        // O servidor reiniciou e recriou o arquivo: ler da posição antiga
        // devolveria lixo, e repetir o conteúdo confundiria quem lê.
        let tmp = TempFile::new("truncate");
        tmp.append(b"conteudo longo anterior\n");
        let mut tailer = LogTailer::new(&tmp.0, "windows1252");
        tmp.truncate(b"novo\n");
        assert_eq!(tailer.read_new().as_deref(), Some("novo\n"));
    }

    #[test]
    fn output_is_decoded_with_the_configured_encoding() {
        // O servidor escreve em windows-1252: lido como UTF-8, o acento viraria
        // lixo no meio da mensagem.
        let tmp = TempFile::new("encoding");
        let mut tailer = LogTailer::new(&tmp.0, "windows1252");
        tmp.append(&[b'a', 0xE7, 0xE3, b'o', b'\n']);
        assert_eq!(tailer.read_new().as_deref(), Some("ação\n"));
    }

    #[test]
    fn a_missing_file_yields_nothing() {
        let tmp = TempFile::new("missing");
        let mut tailer = LogTailer::new(&tmp.0, "windows1252");
        assert_eq!(tailer.read_new(), None);
        // E passa a ler quando o servidor criar o arquivo.
        tmp.append(b"apareceu\n");
        assert_eq!(tailer.read_new().as_deref(), Some("apareceu\n"));
    }

    #[test]
    fn knows_which_file_it_follows() {
        // Evita reiniciar o tail à toa, o que descartaria a posição.
        let tmp = TempFile::new("which");
        let tailer = LogTailer::new(&tmp.0, "");
        assert!(tailer.is_tailing(&tmp.0));
        assert!(!tailer.is_tailing(Path::new("/outro.txt")));
    }

    #[test]
    fn an_empty_encoding_falls_back_to_the_server_default() {
        let tmp = TempFile::new("default-enc");
        let mut tailer = LogTailer::new(&tmp.0, "");
        tmp.append(&[0xE7, b'\n']);
        assert_eq!(tailer.read_new().as_deref(), Some("ç\n"));
    }
}
