//! A engine: o que o core entrega a ela e onde ela atende.
//!
//! A engine é uma biblioteca, não um processo à parte — o core a atende pelo
//! soquete único, a supervisiona e é a única fonte da configuração dela.

mod service;
mod sessions;
mod settings;

pub use service::EngineService;
pub use settings::{build_settings, resolve_sdk_file_path};
