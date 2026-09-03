//! Configuração do PawnPro: tipos, leitura e mesclagem.

pub mod manager;
pub mod naming_lists;
pub mod types;

pub use manager::{ConfigError, ConfigManager, PAWNPRO_DIR, Scope};
pub use types::PawnProConfig;
