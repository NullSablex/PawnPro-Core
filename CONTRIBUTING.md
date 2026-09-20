# Contribuindo com o PawnPro Core

Obrigado pelo interesse! Este repositório é o **núcleo nativo** do PawnPro: o
motor de análise, o depurador e tudo que depende do sistema operacional. O que
é da interface do editor fica no [repositório da
extensão](https://github.com/NullSablex/PawnPro).

## Antes de começar

- Veja se já existe uma [issue](https://github.com/NullSablex/PawnPro-Core/issues) aberta para o assunto.
- Para mudanças significativas, abra uma issue primeiro e combine a abordagem.
- Ao contribuir, você concorda que seu código será licenciado nos termos da [licença do projeto](LICENSE.md).

## Ambiente

**Pré-requisitos:** Rust estável (via [rustup](https://rustup.rs)) e, para mexer
no plugin do servidor, o alvo de 32 bits:

```bash
rustup target add i686-unknown-linux-gnu   # Linux
```

```bash
cargo build --release -p pawnpro-core                                        # o binário
cargo build --release -p pawnpro-debug-plugin --target i686-unknown-linux-gnu # o plugin
```

Para rodar com a extensão sem esperar um release, copie o binário para a pasta
`bin/` dela — o passo a passo está no `CLAUDE.md` da extensão.

## Verificação (a mesma do CI)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -W clippy::pedantic -D warnings
cargo test --workspace
```

Vale rodar também a sonda contra um projeto Pawn de verdade, que é o que pega
o que teste sintético não pega:

```bash
PAWNPRO_PROBE_PROJECT=~/meu-gamemode \
  cargo test --release -p pawnpro-engine probe -- --nocapture
```

## Estrutura

```
crates/core/               o binário: RPC, supervisor, soquete, servidor, configuração
crates/engine/             análise de Pawn e LSP
crates/debugger/adapter/   adaptador DAP
crates/debugger/protocol/  o protocolo entre adaptador e plugin
crates/debugger/plugin/    o plugin que roda dentro do servidor
docs/                      a documentação publicada (pt-BR e en-US)
```

A documentação interna explica o desenho e o porquê de cada peça:
<https://pawnpro-core.nullsablex.com/>.

## Regras de código

- **Identificadores em inglês, comentários em português.** Vale para funções,
  tipos, variáveis e **nomes de teste**.
- **Comentário só para o porquê** — restrição oculta, invariante, armadilha.
  O que o código já diz não se repete em comentário.
- **Erro é `enum`, não string.** Cada condição que o outro lado precisa
  distinguir vira uma variante, e o `match` exaustivo obriga a tratá-la.
- **Nada de `unwrap`/`expect` em caminho de produção** quando a falha é
  possível; num teste, tudo bem.
- **Quem possui o recurso responde sobre ele.** Se a resposta depende de
  adivinhar o estado de outro processo, provavelmente está no lugar errado.
- **Teste que prova.** Um teste que passa com o defeito de volta não serve:
  desfaça a correção e confira que ele falha.
- Toda mudança visível ao usuário entra no `CHANGELOG.md`.
- Texto que chega ao usuário é traduzido nos cinco idiomas
  (`messages/langs/`), e o código do diagnóstico nunca muda.

## Abrindo uma Pull Request

1. Crie um branch a partir de `master`: `git checkout -b fix/o-que-muda`.
2. Faça as três verificações acima passarem.
3. Descreva **o que muda e por quê**; se corrige algo, diga como reproduzir.
4. Mudou dependência? O `Cargo.lock` vai junto no commit.

## Uso de IA

Permitido e bem-vindo, com as mesmas regras da extensão: quem envia é o
responsável, sem co-autoria de IA e sem preconceito quanto ao uso. Detalhes em
[AI-POLICY.md](AI-POLICY.md).

## Reportando bugs

Inclua a versão do núcleo (a página **Ajuda e informações** da extensão mostra),
o sistema operacional, os passos para reproduzir e, se possível, o
`.pawnpro/logs/pawnpro.log` com o diagnóstico ligado — revise-o antes, porque
ele contém caminhos do seu projeto.

**Vulnerabilidades não vão em issue pública**: veja o [SECURITY.md](SECURITY.md).
