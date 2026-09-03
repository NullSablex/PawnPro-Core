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
                                  │
                                  ├── engine    (crate, task supervisionada)
                                  ├── debugger  (crate, task supervisionada)
                                  └── rcon      (crate)
                                          └──► omp-server
```

O core expõe LSP e DAP por socket local, e a extensão conecta seus clientes
neles. Quem possui o `omp-server` passa a ser quem responde sobre ele.

## Princípios

Herdados da frente que motivou esta migração, e válidos aqui igualmente:

1. **A porta é a única prova.** Terminal aberto, evento recebido, comando
   despachado — nada disso significa que o servidor está no ar.
2. **Pedir não é concluir.** Um `send` UDP que retorna `Ok` não diz que alguém
   recebeu. É por isso que [`RconClient::send`](supervision.md#rcon) sonda antes.
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
