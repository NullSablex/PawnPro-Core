# PawnPro Core

Núcleo em Rust do [PawnPro](https://github.com/NullSablex/PawnPro): supervisiona
a engine (LSP) e o depurador (DAP), e concentra as operações que dependem do
sistema operacional.

## Estrutura

| Crate | Responsabilidade |
|---|---|
| `crates/core` | Supervisor e tudo que depende do sistema operacional |
| `crates/engine` | Análise de Pawn e LSP *(a migrar)* |
| `crates/debugger` | DAP e o servidor do jogo *(a migrar)* |

Três crates, e só elas: as peças arquiteturais. Responsabilidades internas —
RCON, processos, portas — são **módulos** da crate a que pertencem
(`core/src/server/`), não crates próprias: multiplicar crates para cada função
criaria fronteiras sem separar nada.

## Build

```bash
cargo build --release
```

## Verificação (igual à do CI)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -W clippy::pedantic -D warnings
cargo test --workspace
```

## Documentação

<https://pawnpro-core.nullsablex.com/> — fonte em [`docs/`](docs/).

## Licença

AGPL-3.0-or-later. Ver [LICENSE](LICENSE).
