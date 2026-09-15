# PawnPro Core

Núcleo em Rust do [PawnPro](https://github.com/NullSablex/PawnPro). Supervisiona
os subsistemas que a extensão usava como processos separados — a engine (LSP) e
o depurador (DAP) — e concentra as operações que dependem do sistema
operacional.

## Por que existe

A extensão em TypeScript comandava processos que não possuía: descobria quem
ocupava uma porta, decidia se um processo era do projeto e o encerrava, tudo por
sinais indiretos. Cada operação dessas tinha uma implementação por sistema
(`lsof`, `/proc`, `ps`, `netstat`, `taskkill`), escrita à mão e difícil de
testar.

O core inverte isso: **quem possui o processo é quem responde sobre ele**.

## Estrutura

| Crate | Responsabilidade |
|---|---|
| `crates/core` | Supervisor e tudo que depende do sistema operacional |
| `crates/engine` | Análise de Pawn e LSP |
| `crates/debugger` | DAP e o servidor do jogo *(a migrar)* |

Três crates, e só elas: as peças arquiteturais. Responsabilidades internas —
RCON, processos, portas — são **módulos** da crate a que pertencem
(`core/src/server/`), não crates próprias: multiplicar crates para cada função
criaria fronteiras sem separar nada.

Crates separados, um binário só. A separação não é cosmética: o `Cargo.toml` de
cada um impede acoplamento acidental, e é o que permite um subsistema cair sem
levar os outros junto.

## Estado

Em construção. O core já concentra o RCON, os processos e a configuração, e
hospeda a engine num soquete local que ele supervisiona. Falta o
depurador — ver [Arquitetura](architecture.md) e [Supervisão](supervision.md).
