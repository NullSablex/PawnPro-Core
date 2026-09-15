//! Onde a engine atende: um soquete local com permissão de arquivo.
//!
//! O stdio do processo já é do JSON-RPC da extensão, então o LSP precisa de um
//! canal próprio. Não é TCP em loopback: o LSP não autentica ninguém, e a
//! engine lê do disco o arquivo que a URI recebida apontar. Numa porta local,
//! qualquer processo da máquina — de qualquer usuário — conectaria e pediria o
//! conteúdo de qualquer arquivo legível pelo dono da sessão. Um soquete Unix
//! dentro de um diretório `0700` faz o sistema de arquivos recusar isso.
//!
//! No Windows o equivalente é um named pipe.
//!
//! O endereço nasce fora do laço supervisionado e sobrevive aos reinícios: a
//! engine cai e volta no mesmo lugar, sem a extensão ter de perguntar de novo.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pawnpro_engine::SettingsReceiver;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::runtime::Builder;
use tokio::task::JoinHandle;

/// De quanto em quanto tempo o laço volta a olhar o sinalizador de parada.
///
/// Sem isto o `accept` esperaria para sempre por um cliente que não vem, e o
/// encerramento só chegaria na próxima conexão.
const ACCEPT_POLL: Duration = Duration::from_millis(200);

/// A engine hospedada.
///
/// Interno ao módulo: quem usa a engine passa pelo [`super::EngineService`],
/// que é quem sabe quando ela pode subir.
pub struct EngineHost {
    endpoint: Endpoint,
    /// O que o core entrega às sessões. Uma entrega nova alcança as que já
    /// estão abertas.
    settings: SettingsReceiver,
}

impl EngineHost {
    /// Reserva o endereço e passa a poder atender.
    ///
    /// # Errors
    /// Falha do sistema ao criar o diretório privado ou o soquete.
    pub fn bind(settings: SettingsReceiver) -> io::Result<Self> {
        Ok(Self {
            endpoint: Endpoint::open()?,
            settings,
        })
    }

    /// Onde a extensão deve conectar.
    #[must_use]
    pub fn address(&self) -> &str {
        self.endpoint.address()
    }

    /// Apaga o que o endereço deixou no sistema de arquivos.
    ///
    /// Não pode ficar só no `Drop`: a thread supervisionada também segura o
    /// host, e quando o core encerra ela pode não ter saído ainda — o soquete
    /// sobreviveria ao processo. Chamar duas vezes não faz mal.
    pub fn release(&self) {
        self.endpoint.release();
    }

    /// Atende sessões LSP enquanto o sinalizador estiver ligado.
    ///
    /// É o trabalho que o supervisor executa: retornar com o sinalizador ainda
    /// ligado conta como queda e provoca o reinício.
    ///
    /// # Panics
    /// Repassa o panic de uma sessão, para o supervisor tratá-lo como queda.
    /// Sem isso a engine ficaria de pé sem atender.
    pub fn serve(&self, running: &AtomicBool) {
        // Um runtime por volta do supervisor: reiniciar a engine não pode
        // herdar as tarefas da tentativa anterior.
        let Ok(runtime) = Builder::new_multi_thread().enable_all().build() else {
            return;
        };
        runtime.block_on(self.endpoint.accept_loop(running, &self.settings));
    }
}

/// As sessões abertas e o aviso de que alguma caiu.
struct Sessions {
    open: Vec<JoinHandle<()>>,
    crashed: Arc<AtomicBool>,
    settings: SettingsReceiver,
}

impl Sessions {
    fn new(settings: &SettingsReceiver) -> Self {
        Self {
            open: Vec::new(),
            crashed: Arc::new(AtomicBool::new(false)),
            settings: settings.clone(),
        }
    }

    /// Entrega a conexão à engine numa tarefa própria.
    fn accept<S>(&mut self, stream: S)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (input, output) = tokio::io::split(stream);
        let guard = SessionGuard(Arc::clone(&self.crashed));
        let settings = self.settings.clone();
        self.open.push(tokio::spawn(async move {
            pawnpro_engine::serve(input, output, settings).await;
            drop(guard);
        }));
    }

    /// Descarta as terminadas e repassa o panic de qualquer uma delas.
    ///
    /// # Panics
    /// Quando uma sessão caiu: é o que faz o supervisor reiniciar a engine, no
    /// mesmo endereço.
    fn check(&mut self) {
        self.open.retain(|session| !session.is_finished());
        assert!(
            !self.crashed.load(Ordering::Relaxed),
            "uma sessão da engine caiu em panic"
        );
    }

    /// Encerra o que ainda estiver aberto.
    fn close(self) {
        for session in self.open {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use pawnpro_engine::SettingsReceiver;
    use tokio::time::timeout;

    use super::{ACCEPT_POLL, Sessions};

    /// Distingue reservas do mesmo processo.
    ///
    /// O PID sozinho não basta: dois `EngineService` no mesmo core cairiam no
    /// mesmo diretório, e o `Drop` de um apagaria o soquete do outro.
    static RESERVATION: AtomicU32 = AtomicU32::new(0);

    /// Permissão do diretório que guarda o soquete: só o dono entra.
    ///
    /// É o controle de acesso da sessão. O soquete em si herdaria a `umask`, e
    /// numa `/tmp` de modo `1777` isso deixaria qualquer usuário conectar.
    const OWNER_ONLY: u32 = 0o700;

    /// O soquete Unix e o diretório privado que o contém.
    pub struct Endpoint {
        dir: PathBuf,
        path: String,
        listener: Arc<UnixListener>,
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

            // Um core anterior com o mesmo PID pode ter deixado o diretório
            // para trás — dois processos vivos nunca compartilham um PID, então
            // o que estiver aqui é sobra. O soquete velho impediria o `bind`.
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
            // O modo vai no próprio `mkdir`: criar e depois ajustar deixaria
            // uma janela com o diretório aberto.
            fs::DirBuilder::new().mode(OWNER_ONLY).create(&dir)?;

            let path = dir.join("engine.sock");
            let listener = UnixListener::bind(&path)?;
            // O laço usa `accept` com prazo; bloqueante, o prazo não existiria.
            listener.set_nonblocking(true)?;

            Ok(Self {
                dir,
                path: path.to_string_lossy().into_owned(),
                listener: Arc::new(listener),
            })
        }

        /// O caminho do soquete.
        pub fn address(&self) -> &str {
            &self.path
        }

        /// Apaga o diretório privado, com o soquete dentro.
        pub fn release(&self) {
            let _ = fs::remove_dir_all(&self.dir);
        }

        /// Aceita conexões e entrega cada uma à engine.
        pub async fn accept_loop(&self, running: &AtomicBool, settings: &SettingsReceiver) {
            // O clone é do descritor: o `Endpoint` continua dono do soquete, e
            // o reinício reaproveita o mesmo.
            let Ok(cloned) = self.listener.try_clone() else {
                return;
            };
            let Ok(listener) = tokio::net::UnixListener::from_std(cloned) else {
                return;
            };

            let mut sessions = Sessions::new(settings);
            while running.load(Ordering::Relaxed) {
                // Duas condições sem reação própria: o prazo estourado é o que
                // devolve o controle ao laço quando não há conexão, e uma que
                // morreu antes de completar não é motivo para parar.
                if let Ok(Ok((stream, _))) = timeout(ACCEPT_POLL, listener.accept()).await {
                    sessions.accept(stream);
                }
                sessions.check();
            }
            sessions.close();
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use pawnpro_engine::SettingsReceiver;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::time::timeout;

    use super::{ACCEPT_POLL, Sessions};

    /// O named pipe onde a engine atende.
    pub struct Endpoint {
        name: String,
    }

    impl Endpoint {
        /// Reserva o nome. A instância só nasce no laço.
        ///
        /// # Errors
        /// Nunca falha aqui: o erro do sistema aparece ao criar a instância.
        pub fn open() -> io::Result<Self> {
            Ok(Self {
                name: format!(r"\\.\pipe\pawnpro-core-{}-engine", std::process::id()),
            })
        }

        /// O nome do pipe, para quem vai conectar.
        pub fn address(&self) -> &str {
            &self.name
        }

        /// Nada a apagar: o pipe morre com a última instância aberta.
        pub const fn release(&self) {}

        /// Aceita conexões e entrega cada uma à engine.
        ///
        /// Um named pipe não é um ouvinte que se reaproveita: cada instância
        /// atende um cliente, e a seguinte precisa existir antes.
        pub async fn accept_loop(&self, running: &AtomicBool, settings: &SettingsReceiver) {
            let Ok(mut server) = create(self.address(), true) else {
                return;
            };

            let mut sessions = Sessions::new(settings);
            while running.load(Ordering::Relaxed) {
                // O prazo é o que devolve o controle ao laço quando não há
                // cliente.
                if let Ok(Ok(())) = timeout(ACCEPT_POLL, server.connect()).await {
                    let Ok(next) = create(self.address(), false) else {
                        return;
                    };
                    sessions.accept(std::mem::replace(&mut server, next));
                }
                sessions.check();
            }
            sessions.close();
        }
    }

    /// Cria uma instância do pipe.
    ///
    /// A primeira é marcada como tal: sem isso, outro processo poderia criar
    /// uma instância com o mesmo nome e receber conexões destinadas à engine.
    fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new().first_pipe_instance(first).create(name)
    }
}
