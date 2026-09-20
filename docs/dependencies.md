# Dependências e build

O que cada dependência resolve, e por que o perfil de release é assim. Fica
aqui, e não em comentário no `Cargo.toml`: manifesto é para declarar versões, e
o motivo de uma escolha envelhece em ritmo diferente do número dela.

## O que cada uma faz

| Crate | Onde | Por quê |
|---|---|---|
| `serde`, `serde_json` | núcleo, engine | O JSON-RPC com a extensão e a leitura do `config.json`. |
| `regex` | núcleo, engine | Varredura de fontes Pawn e dos arquivos de configuração do servidor. |
| `encoding_rs` | núcleo | A saída do `pawncc` vem em windows-1252 na maioria das builds. É o equivalente ao `iconv-lite` que a extensão usava. |
| `sysinfo` | núcleo | Inspeção de processos multiplataforma. Substitui os caminhos por sistema que a extensão mantinha à mão — `/proc`, `ps`, `Get-Process`, `taskkill`. |
| `tokio` | núcleo, engine | O runtime da engine e o soquete em que ela atende. O núcleo pede só `rt-multi-thread`, `net`, `time` e `io-util`: o prazo do `accept` é o que deixa o encerramento chegar. |
| `tokio-util` | núcleo | Liga os fluxos assíncronos do soquete ao adaptador de depuração, que é síncrono. |
| `interprocess` | núcleo | O named pipe do Windows, equivalente ao soquete Unix. |
| `similar` | depurador | O diff de linhas entre o fonte compilado e o texto atual, que leva o breakpoint à linha certa. Sem `default-features`: só o algoritmo de texto. |
| `rust-samp-sdk` | depurador | A interface com a máquina virtual do Pawn, usada pelo plugin do servidor. |
| `tower-lsp` | engine | O protocolo LSP. |
| `walkdir`, `dashmap`, `futures` | engine | Varredura do workspace, cache de documentos abertos e composição das análises. |

`dashmap` fica na 6 de propósito: a 7 ainda é *release candidate*.

## Perfil de release

```toml
lto = true
codegen-units = 1
strip = true
```

O binário é distribuído dentro do VSIX, então tamanho importa mais que tempo de
build — as três opções custam compilação, não execução.

**Não há `panic = "abort"`, e isso é deliberado.** O núcleo supervisiona a
engine e o depurador, e um panic num deles precisa derrubar só aquele
subsistema. Com `abort` não existe unwind, o `catch_unwind` da borda não pega
nada, e o processo inteiro morre — o oposto do que o supervisor existe para
fazer. Ver [Supervisão](supervision.md).

## Uma versão para tudo

As crates usam `version.workspace = true`. Elas saem juntas, num binário só;
versioná-las em separado só criaria a pergunta "qual delas é a que importa".

## Atualizar

```bash
cargo update                                  # dentro do semver, mexe no Cargo.lock
cargo search <crate> --limit 20               # ver se há versão maior
cargo test --workspace                        # antes de commitar o lock
```

## Licenças das dependências

Os binários redistribuem as bibliotecas compiladas neles, e as licenças delas
exigem levar os avisos junto. O release gera `pawnpro-core-THIRD-PARTY.txt` e
`pawnpro_debug-THIRD-PARTY.txt` com o `cargo-about`, e a extensão empacota o
primeiro ao lado do binário.

`about.toml` lista as licenças aceitas. Uma dependência nova com licença fora da
lista derruba o release: aceitá-la é uma decisão, não um efeito colateral de
`cargo update`. As crates do próprio workspace têm `publish = false` e ficam de
fora — são cobertas pela licença PawnPro-Core.
