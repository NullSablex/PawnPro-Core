# PawnPro Core

Núcleo em Rust do [PawnPro](https://github.com/NullSablex/PawnPro). Hospeda os
subsistemas que a extensão usava como processos separados — a engine (LSP) e o
depurador (DAP) — e concentra as operações que dependem do sistema operacional.

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
| `crates/core` | O binário `pawnpro-core`: JSON-RPC com a extensão, supervisor, soquete e tudo que depende do sistema operacional |
| `crates/engine` | Análise de Pawn e LSP, como biblioteca |
| `crates/debugger/adapter` | Adaptador DAP, como biblioteca |
| `crates/debugger/protocol` | O protocolo entre o adaptador e o plugin |
| `crates/debugger/plugin` | O plugin do servidor (`pawnpro_debug.so` / `.dll`), de 32 bits |

Uma crate por peça arquitetural. Responsabilidades internas —
RCON, processos, portas — são **módulos** da crate a que pertencem
(`core/src/server/`), não crates próprias: multiplicar crates para cada função
criaria fronteiras sem separar nada.

Crates separados, um binário só. A separação não é cosmética: o `Cargo.toml` de
cada um impede acoplamento acidental, e é o que permite um subsistema cair sem
levar os outros junto.

## Estado

O núcleo concentra o RCON, os processos, as portas, o compilador, a configuração
e o estado do projeto, e hospeda a engine e o adaptador de depuração num soquete
local que ele supervisiona. **A depuração é instável**: funciona no uso descrito
na documentação da extensão, mas ainda pode falhar.

Ver [Arquitetura](architecture.md) e [Supervisão](supervision.md).
