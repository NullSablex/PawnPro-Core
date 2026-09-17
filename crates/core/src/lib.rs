//! Núcleo do PawnPro: supervisiona os subsistemas e concentra o que depende do
//! sistema operacional.
//!
//! Ver `docs/architecture.md`.

pub mod compiler;
pub mod config;
pub mod diagnostics;
pub mod gateway;
pub mod project;
pub mod rpc;
pub mod server;
pub mod supervisor;
