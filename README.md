<div align="center">
  <img src="images/logo.png" alt="PawnPro Core" />

  [![CI](https://img.shields.io/github/actions/workflow/status/NullSablex/PawnPro-Core/ci.yml?style=flat-square&label=CI)](https://github.com/NullSablex/PawnPro-Core/actions/workflows/ci.yml)
  [![CodeQL](https://img.shields.io/github/actions/workflow/status/NullSablex/PawnPro-Core/codeql.yml?style=flat-square&logo=github&label=CodeQL)](https://github.com/NullSablex/PawnPro-Core/actions/workflows/codeql.yml)
  [![Docs](https://img.shields.io/github/actions/workflow/status/NullSablex/PawnPro-Core/docs.yml?style=flat-square&label=docs)](https://pawnpro-core.nullsablex.com/)
  [![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/NullSablex/PawnPro-Core/badge?style=flat-square)](https://scorecard.dev/viewer/?uri=github.com/NullSablex/PawnPro-Core)
  [![Release](https://img.shields.io/github/v/release/NullSablex/PawnPro-Core?style=flat-square&logo=github&label=release)](https://github.com/NullSablex/PawnPro-Core/releases)
  [![Downloads](https://img.shields.io/github/downloads/NullSablex/PawnPro-Core/total?style=flat-square&logo=github&label=downloads)](https://github.com/NullSablex/PawnPro-Core/releases)
  [![Stars](https://img.shields.io/github/stars/NullSablex/PawnPro-Core?style=flat-square&logo=github&label=stars)](https://github.com/NullSablex/PawnPro-Core/stargazers)
  [![Issues](https://img.shields.io/github/issues/NullSablex/PawnPro-Core?style=flat-square&logo=github&label=issues)](https://github.com/NullSablex/PawnPro-Core/issues)
  [![License](https://img.shields.io/badge/licença-Source--Available-blue?style=flat-square)](LICENSE.md)

  ![Rust](https://img.shields.io/badge/Rust-edição%202024-000000?style=flat-square&logo=rust&logoColor=white)
  ![Windows x64](https://img.shields.io/badge/Windows-x64-0078D4?style=flat-square&logo=windows11&logoColor=white)
  ![Linux](https://img.shields.io/badge/Linux-x64%20·%20arm64-FCC624?style=flat-square&logo=linux&logoColor=black)
  ![macOS](https://img.shields.io/badge/macOS-x64%20·%20arm64-000000?style=flat-square&logo=apple&logoColor=white)
</div>

**O núcleo nativo do [PawnPro](https://github.com/NullSablex/PawnPro).** Um
binário em Rust que analisa código Pawn, depura o servidor do jogo e comanda
tudo o que depende do sistema operacional. A extensão do editor conversa com
ele e desenha o resultado.

## Por que existe

A extensão em TypeScript comandava processos que não possuía: descobria quem
ocupava uma porta, decidia se um processo era do projeto e o encerrava, tudo por
sinal indireto. Cada uma dessas operações tinha uma implementação por sistema
(`lsof`, `/proc`, `ps`, `netstat`, `taskkill`), escrita à mão e difícil de
testar. E o que a extensão calculava por conta própria — includes, SDK, flags do
compilador — podia discordar do que a análise usava.

O núcleo inverte isso: **quem possui o recurso é quem responde sobre ele**, e há
uma fonte só para cada resposta.

## O que ele faz

- **Análise de Pawn (LSP)** — diagnósticos, autocomplete, hover, assinatura,
  ir para definição, referências, renomeação com escopo e formatação, sobre a
  unidade de compilação do arquivo aberto.
- **Depuração (DAP)** — breakpoints (de linha, condicionais, por contagem,
  logpoints, de função, de dado e em erro de runtime), pilha de chamadas,
  inspeção e edição de variáveis, com um plugin que roda dentro do servidor.
- **Servidor SA-MP / open.mp** — resolve o executável e o log, sobe e encerra o
  processo, descobre quem ocupa a porta, fala RCON e filtra do histórico os
  comandos que carregam senha.
- **Configuração e projeto** — lê e grava os `config.json` global e do projeto,
  resolve includes e SDK, monta a linha de comando do `pawncc` e guarda o estado
  local. Observa os arquivos e avisa a extensão e a engine quando algo muda.

Tudo isso por JSON-RPC 2.0 no stdio, com a engine, o depurador e o plugin num
**soquete local único** (named pipe no Windows).

## Estrutura

| Crate | Responsabilidade |
|---|---|
| `crates/core` | O binário `pawnpro-core`: JSON-RPC com a extensão, supervisor, soquete e tudo que depende do sistema operacional |
| `crates/engine` | Análise de Pawn e LSP, como biblioteca |
| `crates/debugger/adapter` | Adaptador DAP, como biblioteca |
| `crates/debugger/protocol` | O protocolo entre o adaptador e o plugin |
| `crates/debugger/plugin` | O plugin do servidor (`pawnpro_debug.so` / `.dll`), de 32 bits |

Crates separadas, um binário só. A separação não é cosmética: o `Cargo.toml` de
cada uma impede acoplamento acidental, e é o que permite um subsistema cair sem
levar os outros junto. Responsabilidades internas — RCON, processos, portas —
são **módulos** da crate a que pertencem (`core/src/server/`), não crates
próprias: multiplicar crates para cada função criaria fronteiras sem separar
nada.

## Build

```bash
cargo build --release -p pawnpro-core
```

O plugin do servidor é de 32 bits, como o servidor:

```bash
rustup target add i686-unknown-linux-gnu
cargo build --release -p pawnpro-debug-plugin --target i686-unknown-linux-gnu
```

As dependências, o perfil de release e como atualizá-los estão em
[`docs/dependencies.md`](docs/dependencies.md).

## Verificação (igual à do CI)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -W clippy::pedantic -D warnings
cargo test --workspace
```

## Avisos de terceiros

Cada release publica, junto dos binários, os avisos de licença das bibliotecas
compiladas neles. Para gerar localmente:

```bash
cargo install cargo-about --features cli --locked
cargo about generate --fail -m crates/core/Cargo.toml about.hbs -o pawnpro-core-THIRD-PARTY.txt
cargo about generate --fail -m crates/debugger/plugin/Cargo.toml about.hbs -o pawnpro_debug-THIRD-PARTY.txt
```

Uma dependência com licença fora de `about.toml` faz a geração falhar.

## Release

Uma tag `v*.*.*` publica, num release só: o binário do núcleo para Windows,
Linux e macOS (x64 e arm64), o plugin de depuração para Linux e Windows, os
avisos de licença e os checksums. As três crates compartilham a versão — elas
saem juntas, e versioná-las em separado só criaria a pergunta "qual delas é a
que importa".

A extensão baixa o binário da release indicada em `coreVersion`, no
`package.json` dela, e o empacota no VSIX.

## Documentação

<https://pawnpro-core.nullsablex.com/> — fonte em [`docs/`](docs/), em
português e inglês.

| | |
|---|---|
| [Arquitetura](docs/architecture.md) | As peças, o soquete único e por que o desenho é assim |
| [O contrato com a extensão](docs/rpc.md) | Os métodos JSON-RPC e o que é do núcleo |
| [Configuração e projeto](docs/configuration.md) | Escopos, listas de nomes, includes e o compilador |
| [O servidor do jogo](docs/server.md) | Executável, portas, processos, log e RCON |
| [A engine](docs/engine.md) | Unidade de compilação, cache, diagnósticos e formatação |
| [Depuração](docs/debugger.md) | Adaptador, plugin e o que acontece numa sessão |
| [Supervisão](docs/supervision.md) | Como um subsistema cai e volta sem levar o resto |
| [Dependências](docs/dependencies.md) | O que cada uma resolve, o perfil de release e as licenças |

## Contribuindo

As issues e os pull requests desta parte do projeto ficam aqui; o que é da
interface vai para o [repositório da extensão](https://github.com/NullSablex/PawnPro).

O uso de **IA** é permitido: quem contribui é responsável pelo que envia, sem
co-autoria de IA, e sem preconceito quanto ao seu uso.

## Licença

PawnPro-Core License v1.0 — Source-Available (não Open Source).  
Uso pessoal e comercial permitido ✅ · Redistribuição gratuita com atribuição ✅ · Venda proibida ❌ · Detalhes: [LICENSE.md](LICENSE.md)
