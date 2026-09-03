//! Tudo que fala com o servidor Pawn (`omp-server` / `samp03svr`).
//!
//! Reúne o que a extensão fazia por sinais indiretos: enviar comandos por RCON,
//! descobrir quem ocupa a porta, decidir se um processo é do projeto e
//! encerrá-lo. Fica sob um módulo só porque são a mesma responsabilidade vista
//! por ângulos diferentes — e porque errar a fronteira entre elas foi a origem
//! dos defeitos que motivaram esta migração.

pub mod rcon;
