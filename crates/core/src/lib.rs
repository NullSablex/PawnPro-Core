//! Núcleo do PawnPro.
//!
//! Supervisiona os subsistemas que a extensão usava como processos separados —
//! a engine (LSP) e o depurador (DAP) — e concentra as operações que dependem
//! do sistema operacional.
//!
//! A lógica vive aqui, e não no binário, para poder ser testada sem subir o
//! processo inteiro. O `main` é só o ponto de entrada que a consome.
//!
//! Ver `docs/architecture.md`.

pub mod compiler;
pub mod includes;
pub mod server;
pub mod state;
pub mod types;
pub mod ui;
