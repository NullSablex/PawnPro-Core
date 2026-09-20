# Configuração e projeto

A configuração do PawnPro tem **um dono**: o núcleo. Antes a extensão lia o
`config.json` em TypeScript e a engine lia de novo em Rust — dois leitores do
mesmo arquivo são dois resultados possíveis, e o IntelliSense podia discordar do
que a compilação usava.

## Escopos e mesclagem

```
padrões  ◄──  ~/.pawnpro/config.json  ◄──  <projeto>/.pawnpro/config.json
 (código)            (global)                     (vence)
```

A mesclagem acontece no **JSON bruto**, antes de virar a estrutura tipada:
depois seria impossível distinguir "o usuário escreveu `false`" de "o padrão é
`false`", e o escopo do projeto não teria como sobrescrever o global de forma
previsível.

Cada campo tem um padrão, e o que faltar é preenchido: um `config.json` com uma
chave só é válido. Um valor com o tipo errado é ignorado sozinho — o resto do
arquivo continua valendo, e a chave rejeitada é informada a quem pediu, em vez
de derrubar a leitura inteira.

`${workspaceFolder}` é resolvido pelo núcleo, inclusive nos valores padrão: quem
recebe a configuração já recebe caminho de verdade.

## Quem é avisado

O núcleo confere os carimbos de tempo dos arquivos de configuração e das listas
`.ban`/`.allow` a cada dois segundos. Quando algo muda:

- a **extensão** recebe a notificação `config.changed` e atualiza o cache sem
  ter pedido nada;
- a **engine** recebe a configuração já resolvida por um canal tipado e
  republica os diagnósticos.

Trocar de projeto move a observação junto.

## Arquivos do projeto

| Arquivo | O que guarda |
|---|---|
| `~/.pawnpro/config.json` | Configuração global |
| `.pawnpro/config.json` | Configuração do projeto |
| `.pawnpro/state.json` | Estado local: histórico e favoritos do painel do servidor |
| `.pawnpro/*.ban` / `*.allow` | Listas longas do assistente de nomes |
| `.pawnpro/logs/` | Registro de diagnóstico, quando ligado |

**O estado não é configuração.** São dados de operação de quem desenvolve —
não pertencem ao repositório nem a outro usuário da máquina. Por isso o
`state.json` é gravado com permissão restrita, de forma atômica, e o núcleo
cria um `.pawnpro/.gitignore` que o exclui sem tocar no `.gitignore` do
projeto.

## Listas de nomes

As listas longas do assistente de nomes moram em arquivos `.ban`/`.allow`, um
termo por linha, e não no JSON: uma lista de centenas de termos dentro do
`config.json` torna o arquivo ilegível e difícil de revisar em diff.

O núcleo lê o arquivo (até o teto configurado) e, se ele não existir ou estiver
vazio, cai na lista inline do JSON. A migração de uma para a outra é manual, com
backup — migrar sozinho mexeria no arquivo do usuário sem ele pedir.

## Includes

As raízes de include saem de **uma função só**, usada pela engine, pela
compilação e pela árvore de includes do editor. Ter três cálculos parecidos era
o que fazia o `#include` resolver de um jeito na análise e de outro no
compilador.

A lista é montada nesta ordem, sem repetição e só com pastas que existem: o que
está em `includePaths`, o que vier de `-i` em `compiler.args`, e então
`qawno/include`, `pawno/include` e `include` na raiz do projeto. Se nada disso
existir na raiz, a busca sobe a partir da pasta do arquivo aberto — o projeto
pode ter um subprojeto com include próprio.

O SDK do open.mp (`open.mp.inc`) é procurado primeiro onde o servidor o instala
(`qawno/include`) e depois nos includes configurados. Um caminho configurado
vence os dois, mas só se existir: apontar para um arquivo ausente é engano do
usuário, e um palpite esconderia isso.

## O compilador

Quem monta a linha de comando do `pawncc` é o núcleo, com a configuração do
projeto aberto; a extensão só mostra a saída.

- **Achar o binário**, do mais explícito ao mais genérico: a variável `PAWNCC`,
  `compiler.path`, o `PATH`, as pastas do projeto (`qawno`, `pawno`, `include`,
  `tools`, `bin`) e os caminhos de instalação comuns. Com a detecção automática
  desligada, um `compiler.path` que não serve é erro — o usuário quer aquele
  compilador, e cair noutro seria pior que falhar.
- **Perguntar ao binário quais flags ele aceita.** `pawncc -?` imprime a ajuda,
  e dela sai o conjunto suportado. Manter uma tabela por versão não daria conta
  dos forks e das builds de open.mp, e passar uma flag desconhecida faz a
  compilação falhar por um motivo que não tem nada a ver com o código.
- **Preset mínimo** quando `compiler.args` está vazio: `-d1`, `-O1`, `-(+`,
  `-;+` e `-w239`, cada um só se a build local o aceitar. As flags que ela não
  aceita são removidas e relatadas, em vez de sumirem em silêncio.
- **Depurar troca o `-d`.** Na compilação para depuração, qualquer `-d` da
  configuração vira `-d3`, só ali: `-d0` a `-d2` não dão os símbolos que os
  breakpoints e a inspeção precisam, e a configuração do usuário não é alterada.

A execução é um trabalho em segundo plano — ver
[O contrato com a extensão](rpc.md#trabalho-demorado-nao-prende-o-laco) — e a
saída é decodificada na codificação configurada antes de voltar.
