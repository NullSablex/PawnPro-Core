# PawnPro Core

Núcleo em Rust do [PawnPro](https://github.com/NullSablex/PawnPro): hospeda a
engine (LSP) e o depurador (DAP) num soquete local único, e concentra as
operações que dependem do sistema operacional. Um repositório, um binário, uma
versão.

## Estrutura

| Crate | Responsabilidade |
|---|---|
| `crates/core` | O binário `pawnpro-core`: JSON-RPC com a extensão, supervisor, soquete e tudo que depende do sistema operacional |
| `crates/engine` | Análise de Pawn e LSP, como biblioteca |
| `crates/debugger/adapter` | Adaptador DAP, como biblioteca |
| `crates/debugger/protocol` | O protocolo entre adaptador e plugin |
| `crates/debugger/plugin` | O plugin do servidor (`pawnpro_debug.so`/`.dll`) |

Responsabilidades internas — RCON, processos, portas — são **módulos** da crate
a que pertencem (`core/src/server/`), não crates próprias: multiplicar crates
para cada função criaria fronteiras sem separar nada.

## Build

```bash
cargo build --release
```

As dependências, o perfil de release e como atualizá-los estão em
[`docs/dependencies.md`](docs/dependencies.md).

## Avisos de terceiros

Cada release publica, junto dos binários, os avisos de licença das bibliotecas
compiladas neles. Para gerar localmente:

```bash
cargo install cargo-about --features cli --locked
cargo about generate --fail -m crates/core/Cargo.toml about.hbs -o pawnpro-core-THIRD-PARTY.txt
cargo about generate --fail -m crates/debugger/plugin/Cargo.toml about.hbs -o pawnpro_debug-THIRD-PARTY.txt
```

Uma dependência com licença fora de `about.toml` faz a geração falhar.

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
