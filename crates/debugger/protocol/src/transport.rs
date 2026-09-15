//! Transporte local cross-platform do IPC plugin↔adaptador: socket local
//! (Unix domain socket no Linux/macOS, named pipe no Windows) via `interprocess`.
//!
//! Substitui o TCP-localhost: sem porta, sem firewall, e o SO já isola o socket
//! ao usuário. Ambos os lados concordam no **nome** derivado de um `id` (string
//! curta, ex.: a porta/sessão), produzido por [`socket_name`].

use std::io;

use interprocess::local_socket::{
    GenericNamespaced, Listener, ListenerOptions, Stream, ToNsName, traits::Stream as _,
};

/// Nome do socket a partir de um identificador de sessão. Namespaced para o
/// `interprocess` escolher o caminho certo por plataforma (em geral
/// `/tmp/...sock`-equivalente abstrato no Unix, `\\.\pipe\...` no Windows).
#[must_use]
pub fn socket_name(id: &str) -> String {
    format!("pawnpro-dbg-{id}")
}

/// Cria o listener (lado do plugin) para o `id` dado.
///
/// # Errors
/// Erro de I/O se o nome for inválido ou o socket já estiver em uso.
pub fn listen(id: &str) -> io::Result<Listener> {
    let name = socket_name(id);
    let ns = name.as_str().to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(ns).create_sync()
}

/// Conecta no socket (lado do adaptador) do `id` dado.
///
/// # Errors
/// Erro de I/O se o socket não existir ou a conexão falhar.
pub fn connect(id: &str) -> io::Result<Stream> {
    let name = socket_name(id);
    let ns = name.as_str().to_ns_name::<GenericNamespaced>()?;
    Stream::connect(ns)
}

// Re-exporta os tipos que os consumidores precisam nomear, para não dependerem
// diretamente do `interprocess`.
pub use interprocess::local_socket::{Listener as LocalListener, Stream as LocalStream};
