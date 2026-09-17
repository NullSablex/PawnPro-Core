//! O ponto de acesso local: soquete Unix num diretório privado, ou named pipe.
//!
//! O stdio do processo já é do JSON-RPC da extensão, então o resto precisa de
//! um canal próprio. Não é TCP em loopback: LSP, DAP e o protocolo do plugin
//! não autenticam ninguém, e a engine lê do disco o arquivo que a URI recebida
//! apontar. Numa porta local, qualquer processo da máquina — de qualquer
//! usuário — conectaria. Um soquete Unix dentro de um diretório `0700` faz o
//! sistema de arquivos recusar isso.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

/// De quanto em quanto tempo o laço volta a olhar o sinalizador de parada.
///
/// Sem isto o `accept` esperaria para sempre por um cliente que não vem, e o
/// encerramento só chegaria na próxima conexão.
const ACCEPT_POLL: Duration = Duration::from_millis(200);

/// Uma conexão aceita, qualquer que seja o transporte.
pub trait Connection: AsyncRead + AsyncWrite + Send + Unpin + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Connection for T {}

/// Uma conexão aceita, com o tipo do transporte apagado.
pub type Stream = Box<dyn Connection>;

#[cfg(unix)]
pub use unix::Endpoint;
#[cfg(windows)]
pub use windows::Endpoint;

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use tokio::time::timeout;

    use super::{ACCEPT_POLL, Stream};

    /// Distingue reservas do mesmo processo.
    ///
    /// O PID sozinho não basta: dois núcleos no mesmo processo (nos testes)
    /// cairiam no mesmo diretório, e o `Drop` de um apagaria o soquete do outro.
    static RESERVATION: AtomicU32 = AtomicU32::new(0);

    /// Permissão do diretório que guarda o soquete: só o dono entra.
    ///
    /// É o controle de acesso. O soquete em si herdaria a `umask`, e numa
    /// `/tmp` de modo `1777` isso deixaria qualquer usuário conectar.
    const OWNER_ONLY: u32 = 0o700;

    /// O soquete Unix e o diretório privado que o contém.
    pub struct Endpoint {
        dir: PathBuf,
        path: String,
        listener: UnixListener,
    }

    impl Endpoint {
        /// Cria o diretório privado e o soquete dentro dele.
        ///
        /// # Errors
        /// Falha do sistema ao criar o diretório ou o soquete.
        pub fn open() -> io::Result<Self> {
            // `XDG_RUNTIME_DIR` já é privado e some no logout; a temporária é
            // o recurso quando ele não existe.
            let base =
                std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let reservation = RESERVATION.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!("pawnpro-core-{}-{reservation}", std::process::id()));

            // Um núcleo anterior com o mesmo PID pode ter deixado o diretório
            // para trás — dois processos vivos nunca compartilham um PID, então
            // o que estiver aqui é sobra. O soquete velho impediria o `bind`.
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
            // O modo vai no próprio `mkdir`: criar e depois ajustar deixaria
            // uma janela com o diretório aberto.
            fs::DirBuilder::new().mode(OWNER_ONLY).create(&dir)?;

            let path = dir.join("core.sock");
            let listener = UnixListener::bind(&path)?;
            // O laço usa `accept` com prazo; bloqueante, o prazo não existiria.
            listener.set_nonblocking(true)?;

            Ok(Self {
                dir,
                path: path.to_string_lossy().into_owned(),
                listener,
            })
        }

        /// O caminho do soquete.
        #[must_use]
        pub fn address(&self) -> &str {
            &self.path
        }

        /// Apaga o diretório privado, com o soquete dentro.
        pub fn release(&self) {
            let _ = fs::remove_dir_all(&self.dir);
        }

        /// Aceita conexões enquanto o sinalizador estiver ligado.
        ///
        /// Precisa rodar dentro do runtime que vai servir as conexões: é nele
        /// que o soquete aceito se registra.
        pub async fn accept_loop(&self, running: &AtomicBool, mut deliver: impl FnMut(Stream)) {
            // O clone é do descritor: o `Endpoint` continua dono do soquete, e
            // o reinício reaproveita o mesmo.
            let Ok(cloned) = self.listener.try_clone() else {
                return;
            };
            let Ok(listener) = tokio::net::UnixListener::from_std(cloned) else {
                return;
            };
            while running.load(Ordering::Relaxed) {
                // O prazo estourado é o que devolve o controle ao laço quando
                // não há conexão, e uma que morreu antes de completar não é
                // motivo para parar.
                if let Ok(Ok((stream, _))) = timeout(ACCEPT_POLL, listener.accept()).await {
                    deliver(Box::new(stream));
                }
            }
        }
    }

    impl Drop for Endpoint {
        /// Rede de segurança para quem não chamou `release`.
        fn drop(&mut self) {
            self.release();
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::time::timeout;

    use super::{ACCEPT_POLL, Stream};

    /// Distingue reservas do mesmo processo, como no Unix.
    static RESERVATION: AtomicU32 = AtomicU32::new(0);

    /// O named pipe onde o núcleo atende.
    pub struct Endpoint {
        name: String,
    }

    impl Endpoint {
        /// Reserva o nome. A instância só nasce no laço.
        ///
        /// # Errors
        /// Nunca falha aqui: o erro do sistema aparece ao criar a instância.
        pub fn open() -> io::Result<Self> {
            let reservation = RESERVATION.fetch_add(1, Ordering::Relaxed);
            Ok(Self {
                name: format!(
                    r"\\.\pipe\pawnpro-core-{}-{reservation}",
                    std::process::id()
                ),
            })
        }

        /// O nome do pipe, para quem vai conectar.
        #[must_use]
        pub fn address(&self) -> &str {
            &self.name
        }

        /// Nada a apagar: o pipe morre com a última instância aberta.
        pub const fn release(&self) {}

        /// Aceita conexões enquanto o sinalizador estiver ligado.
        ///
        /// Um named pipe não é um ouvinte que se reaproveita: cada instância
        /// atende um cliente, e a seguinte precisa existir antes.
        pub async fn accept_loop(&self, running: &AtomicBool, mut deliver: impl FnMut(Stream)) {
            let Ok(mut server) = create(self.address(), true) else {
                return;
            };
            while running.load(Ordering::Relaxed) {
                if let Ok(Ok(())) = timeout(ACCEPT_POLL, server.connect()).await {
                    let Ok(next) = create(self.address(), false) else {
                        return;
                    };
                    deliver(Box::new(std::mem::replace(&mut server, next)));
                }
            }
        }
    }

    /// Cria uma instância do pipe.
    ///
    /// A primeira é marcada como tal: sem isso, outro processo poderia criar
    /// uma instância com o mesmo nome e receber conexões destinadas ao núcleo.
    fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new().first_pipe_instance(first).create(name)
    }
}
