# PawnPro Core

Núcleo em Rust do [PawnPro](https://github.com/NullSablex/PawnPro): supervisiona
a engine (LSP) e o depurador (DAP), e concentra as operações que dependem do
sistema operacional.

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

## Build

```bash
cargo build --release
```

As dependências, o perfil de release e como atualizá-los estão em
[`docs/dependencies.md`](docs/dependencies.md).

## Verificação (igual à do CI)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -W clippy::pedantic -D warnings
cargo test --workspace
```

## Documentação

<https://pawnpro-core.nullsablex.com/> — fonte em [`docs/`](docs/).

## Licença

PawnPro-Core License v1.1 — Source-Available (não Open Source).  
Uso pessoal e comercial permitido ✅ · Redistribuição gratuita com atribuição ✅ · Venda proibida ❌ · Detalhes: [LICENSE.md](LICENSE.md)
