//! Como o plugin alcança o núcleo.
//!
//! O núcleo atende num soquete local só (Unix domain socket num diretório
//! privado; named pipe no Windows) e sobe o servidor com o endereço e o id da
//! sessão no ambiente. O plugin conecta nesse endereço e se apresenta com o id
//! antes de qualquer mensagem do protocolo.
//!
//! Os nomes e o formato ficam aqui porque são contrato entre três peças: o
//! núcleo que lê a apresentação, a sessão que sobe o servidor e o plugin que
//! conecta.

/// Variáveis de ambiente que a sessão passa ao servidor e o plugin lê.
pub mod env {
    /// Endereço do núcleo, onde o plugin conecta.
    pub const ENDPOINT: &str = "PAWNPRO_DBG_ENDPOINT";
    /// Com que id o plugin se apresenta.
    pub const SESSION: &str = "PAWNPRO_DBG_SESSION";
    /// O `.amx` em depuração, de onde o plugin lê o bloco de debug.
    pub const AMX_DEBUG: &str = "PAWNPRO_DBG_AMXDBG";
    /// Idioma das mensagens de erro de runtime.
    pub const LOCALE: &str = "PAWNPRO_DBG_LOCALE";
}

/// A linha com que o plugin se apresenta ao núcleo, com o `\n` final.
#[must_use]
pub fn plugin_greeting(session: &str) -> String {
    format!("PAWNPRO/1 plugin {session}\n")
}
