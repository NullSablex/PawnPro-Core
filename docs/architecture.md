# Arquitetura

## O problema que originou o core

Três processos, e a extensão coordenando todos por fora:

```
editor ──► extensão (TS) ──► engine     (LSP, stdio)
                        └──► adaptador  (DAP, stdio) ──► omp-server
```

A extensão decidia sobre o `omp-server` sem possuí-lo. Para saber se ele estava
no ar, sondava uma porta UDP sem retransmissão; para saber se podia encerrá-lo,
lia `/proc` ou chamava `lsof`. Cada decisão vinha de um sinal indireto, e cada
sinal tinha um jeito diferente por sistema operacional.

## O desenho

```
editor ──► extensão (TS) ──► pawnpro-core (um binário)
              │                   │
              │                   ├── engine    (crate, thread supervisionada)
              │                   ├── adaptador (crate, uma thread por sessão)
              │                   └── server    (módulo: RCON, processos, portas)
              │                             └──► omp-server ──► plugin
              │                                                    │
              └── LSP e DAP ──► soquete local ◄────────────────────┘
                                (Unix; named pipe no Windows)
```

O JSON-RPC do core viaja no stdio; LSP e DAP não caberiam no mesmo canal, então
há um soquete. A extensão pergunta o endereço (`engine.start` para o
IntelliSense, `debug.start` para uma sessão de depuração) e liga o cliente nele.
Quem possui o `omp-server` passa a ser quem responde sobre ele.

## Um soquete, três canais

O endereço é um só, e cada conexão diz na primeira linha a que canal pertence:

```
PAWNPRO/1 lsp                  ← o cliente LSP do editor
PAWNPRO/1 dap                  ← a sessão de depuração do editor
PAWNPRO/1 plugin <sessão>      ← o plugin, de dentro do servidor do jogo
```

Um soquete por subsistema multiplicaria endereços, diretórios e limpeza para
resolver o mesmo problema três vezes. A saudação é uma linha de texto, lida byte
a byte com prazo e tamanho máximo: uma conexão que não se apresente é
descartada, e nenhuma delas chega ao subsistema errado.

O plugin recebe o endereço e o id da sessão pelo ambiente, quando o adaptador
sobe o servidor do jogo. É o id que liga o plugin à sessão certa quando há mais
de uma aberta.

**Não é TCP em loopback.** O LSP não autentica ninguém, e a engine lê do disco
o arquivo que a URI recebida apontar: numa porta local, qualquer processo da
máquina — de qualquer usuário — conectaria e pediria o conteúdo de qualquer
arquivo legível pelo dono da sessão. Um soquete Unix dentro de um diretório
`0700` faz o sistema de arquivos recusar isso; no Windows o equivalente é um
named pipe.

O endereço é reservado uma vez e sobrevive aos reinícios da engine: quando ela
cai e o supervisor a levanta de novo, a extensão reconecta no mesmo lugar em
vez de ter de descobri-lo outra vez.

## Quem entrega a configuração

O core, e só ele. A engine não abre `config.json` nem os arquivos de lista: o
core lê os dois escopos, resolve includes, SDK, formatação e nomenclatura, e
entrega o resultado por um canal interno — uma `struct` Rust, não um objeto
JSON, porque as duas compilam juntas e o compilador pode garantir o acordo.

```
config.json (global + projeto)  ──►  core  ──►  canal tipado  ──►  engine
.ban / .allow                        │
                                     └── confere os carimbos de tempo
```

O core também é quem percebe a mudança: confere os carimbos de tempo dos
arquivos a cada dois segundos e reentrega quando algum muda. A engine republica
os diagnósticos sem o editor pedir. Antes isso era papel da extensão, que
observava os arquivos e mandava `workspace/didChangeConfiguration` — o mesmo
trabalho que o core faz agora, do lado que possui os arquivos.

## Princípios

Herdados da frente que motivou esta migração, e válidos aqui igualmente:

1. **A porta é a única prova.** Terminal aberto, evento recebido, comando
   despachado — nada disso significa que o servidor está no ar.
2. **Pedir não é concluir.** Um `send` UDP que retorna `Ok` não diz que alguém
   recebeu. É por isso que [`RconClient::send`](server.md#rcon) sonda antes.
3. **Estado que caduca não decide fluxo.** Tolerância a perdas serve para
   exibição, nunca para escolher o que fazer.
4. **Erro é `enum`, não string.** Cada condição que a interface precisa
   distinguir vira uma variante — e o `match` exaustivo obriga a tratá-la.

## Por que crates separados, e não um só

Um crate único perderia a fronteira que garante o isolamento. Com crates, o
`Cargo.toml` de `debugger` não declara `engine`: se alguém tentar usar uma da
outra, o compilador barra. É essa fronteira que permite um panic na engine não
derrubar a depuração.

Pelo mesmo motivo o perfil de release **não** usa `panic = "abort"`: sem unwind
não há `catch_unwind` na borda de cada subsistema, e qualquer panic mataria o
processo inteiro.
