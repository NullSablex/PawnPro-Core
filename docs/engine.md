# A engine

A análise de Pawn é uma biblioteca (`crates/engine`) que o núcleo hospeda numa
thread e serve por LSP no soquete local. Ela não abre configuração nem procura
includes: recebe tudo resolvido por um canal tipado — ver
[Arquitetura](architecture.md).

O que ela oferece ao editor: diagnósticos, autocomplete, hover, ajuda de
assinatura, ir para definição, referências, contador acima das funções,
renomeação, correções rápidas, tokens semânticos e formatação (documento e
seleção).

## Unidade de compilação

A pergunta que organiza tudo é "o que é compilado junto com este arquivo?".

**Programa** é o `.pwn` que nenhum outro arquivo do projeto inclui; um `.pwn`
incluído é trecho, não programa. A unidade de um arquivo é o programa que o
inclui, com tudo o que esse programa inclui — inclusive o `.inc` irmão que o
editor não abriu.

Daí saem duas garantias que o editor precisa:

- um símbolo do gamemode não aparece ao editar um filterscript, mesmo que os
  dois tenham funções de mesmo nome;
- uma `stock` usada só pelo `.pwn` que a inclui não é acusada de não usada.

## O texto que vale é o do editor

A análise lê o documento aberto, não o arquivo em disco: uma `stock` nova num
include ainda não salvo já conta nos outros arquivos da unidade.

Cada publicação de diagnósticos carrega a **versão do documento** que a
originou. Se uma edição chegou enquanto a análise corria, o resultado é
descartado: avisos de um texto que já não existe não aparecem.

## Cache

Os identificadores de cada arquivo ficam em cache, validado pela data de
modificação e, para os abertos, pelo texto do editor. O contador de referências
soma direto do cache, sem reler arquivos, e a busca de declarações lê os
símbolos dele.

Num gamemode com 84 includes, cada edição passou de cerca de 1 s para 0,2 s.

## Diagnósticos

São 19 códigos, de `PP0001` a `PP0019`, e 13 deles têm correção automática. A
lista completa, com severidade e o que cada um cobre, está na
[documentação da extensão](https://pawnpro.nullsablex.com/features/#diagnosticos).

Duas decisões valem para todos:

- **Severidade tem significado.** Erro é o que o compilador recusaria; aviso é o
  que compila e provavelmente está errado; dica é estilo. O assistente de nomes
  (`PP0018`) é sempre dica, e vem desligado.
- **A mensagem é traduzida, o código não.** Cada texto é uma `MsgKey` com uma
  tabela por idioma (`messages/langs/`), e o código `PP####` é o que se procura
  na documentação, igual em qualquer idioma. O mesmo vale para os títulos das
  correções rápidas.

## Formatação

A formatação é guiada pela estrutura do código, não por expressão regular sobre
texto. Duas exigências a governam, e há teste para as duas:

1. **Não danificar.** Comentário no fim da linha, string com espaço, caractere,
   macro com continuação `\` e diretiva recuada continuam como estavam.
2. **Ser estável.** Formatar de novo não muda mais nada.

## Sonda contra projeto real

Testes sintéticos não cobrem um gamemode de verdade. A sonda
(`PAWNPRO_PROBE_PROJECT=/caminho/do/projeto`) passa cada arquivo do projeto pela
análise, pelos tokens semânticos e pela formatação, e cobra que nada entre em
pane, que a formatação não altere código nem comentário e que formatar duas
vezes dê o mesmo resultado. Também mede onde vai o tempo da análise.

```bash
PAWNPRO_PROBE_PROJECT=~/meu-gamemode \
  cargo test --release -p pawnpro-engine probe -- --nocapture
```
