//! A engine: o que o core entrega a ela e onde ela atende.
//!
//! A engine é uma biblioteca, não um processo à parte — o core a hospeda num
//! soquete local, a supervisiona e é a única fonte da configuração dela.

mod host;
mod service;
mod settings;

pub use service::EngineService;
pub use settings::{build_settings, resolve_sdk_file_path};
