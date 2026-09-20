# Avisos de terceiros

Os binários deste projeto embutem bibliotecas Rust de terceiros, cada uma sob a
sua licença. A lista completa, **com o texto de cada licença**, é gerada a cada
release e publicada junto dos binários:

| Arquivo | Cobre |
|---|---|
| `pawnpro-core-THIRD-PARTY.txt` | O binário `pawnpro-core` (núcleo, engine e adaptador de depuração) |
| `pawnpro_debug-THIRD-PARTY.txt` | O plugin de depuração do servidor |

O primeiro acompanha também o binário empacotado no VSIX da extensão, em
`bin/pawnpro-core-THIRD-PARTY.txt`.

## Gerar localmente

```bash
cargo install cargo-about --features cli --locked
cargo about generate --fail -m crates/core/Cargo.toml about.hbs -o pawnpro-core-THIRD-PARTY.txt
cargo about generate --fail -m crates/debugger/plugin/Cargo.toml about.hbs -o pawnpro_debug-THIRD-PARTY.txt
```

## Licenças aceitas

`about.toml` lista as licenças que podem entrar nos binários — hoje MIT,
Apache-2.0 (inclusive com a exceção LLVM), Unicode-3.0, Unlicense, Zlib,
BSD-2-Clause, BSD-3-Clause e 0BSD. Uma dependência sob licença fora dessa lista
**faz o release falhar**: aceitá-la é uma decisão, não um efeito colateral de
`cargo update`.

As crates deste repositório têm `publish = false` e ficam de fora desses
arquivos — elas são cobertas pela [licença do projeto](LICENSE.md).

## Código de terceiros no repositório

Nenhum, hoje. Trecho de terceiro que venha a entrar no código-fonte é listado
aqui, com a licença e a origem.
