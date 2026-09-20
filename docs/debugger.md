# Depuração por dentro

!!! warning "Instável"

    A depuração funciona no uso descrito no
    [guia da extensão](https://pawnpro.nullsablex.com/debugging/), mas ainda
    pode falhar. Esta página descreve como ela é feita.

São duas peças e um soquete:

```
editor ──DAP──► adaptador (no núcleo) ──spawn──► omp-server
                     ▲                              │ carrega
                     └───────── soquete ─────────── plugin
```

O **adaptador** (`crates/debugger/adapter`) traduz o DAP do editor para os
comandos que o **plugin** (`crates/debugger/plugin`) executa dentro da máquina
virtual do Pawn. O que passa entre os dois é o `crates/debugger/protocol` — um
enum de comandos e um de eventos, serializados em linhas JSON.

## Como o plugin encontra a sessão

A sessão sobe o servidor do jogo com quatro variáveis de ambiente, e o plugin as
lê ao carregar:

| Variável | Para quê |
|---|---|
| `PAWNPRO_DBG_ENDPOINT` | O endereço do soquete do núcleo |
| `PAWNPRO_DBG_SESSION` | Com que id ele se apresenta (`PAWNPRO/1 plugin <id>`) |
| `PAWNPRO_DBG_AMXDBG` | O `.amx` em depuração, de onde ele lê o bloco de debug |
| `PAWNPRO_DBG_LOCALE` | O idioma das mensagens de erro de runtime |

Sem essas variáveis o plugin não faz nada: carregado num servidor iniciado à
mão, ele fica quieto em vez de tentar conectar em algum lugar.

## Só a VM do programa depurado

O servidor carrega várias máquinas virtuais — o gamemode e cada filterscript —,
e o plugin recebe todas. Os endereços de código começam em zero em cada uma: um
breakpoint aplicado à VM errada pararia em código de outro script, mostrado com
as linhas e as variáveis do programa depurado.

O plugin reconhece a VM certa **pelo conteúdo**, comparando com o `.amx` que a
sessão indicou: o cabeçalho, a tabela de publics e a tabela de nomes, que o
servidor não altera ao carregar. Ficam de fora o código, que é relocado na
carga, e a tabela de natives, que recebe endereços quando os plugins registram
funções.

É isso que faz um filterscript deixar de disparar os breakpoints do gamemode.

## Breakpoints com o fonte já editado

O breakpoint que o editor manda é uma linha do arquivo **como está agora**; o
que roda é o `.amx` compilado antes. Editar durante a sessão desalinha os dois,
e a versão anterior parava no lugar errado.

A sessão guarda o texto de cada arquivo no momento da compilação e compara com
o texto atual por diff de linhas:

```
linha no editor ──diff──► linha na compilação ──bloco de debug──► endereço
```

Uma linha que não existia na compilação não tem endereço, e o breakpoint fica
**não verificado** em vez de parar em outro lugar — o editor a mostra apagada, e
o console explica que é preciso reiniciar para recompilar.

## Reiniciar e recompilar

Quem decide recompilar é o adaptador, no `restart`: é por onde todo reinício
passa, venha do botão, da tecla ou da paleta. Compilar, porém, é da extensão —
o compilador e as flags são configuração do projeto.

```
restart ──► fonte mais novo que o .amx, ou .amx sem bloco de debug?
              ├─ não → sobe o servidor de novo
              └─ sim → evento `pawnproRebuild` ──► a extensão compila
                                                    └─► restart de novo
```

Se a compilação falhar, a extensão não reenvia o `restart`: é o que impede o
servidor de subir com o binário velho.

## Parar

`terminate` derruba o servidor e mantém a sessão viva; `disconnect` encerra a
sessão. São coisas diferentes no DAP, e tratá-las como sinônimo é o que fazia a
barra de progresso do editor ficar presa esperando um fim que já tinha
acontecido.

## Inspeção

- **Arrays** são endereçados como o compilador os gera: o adaptador descreve o
  caminho até o elemento (`grid[1][2]` é um caminho de dois índices), e o plugin
  dereferencia o que for referência (`iREFERENCE`, `iREFARRAY`) e soma os
  deslocamentos de indireção. Sem isso, um array de várias dimensões mostrava o
  valor de outra posição da memória.
- **Editar** uma variável é um comando com id; o plugin confirma com um evento
  correlacionado, e só então o editor mostra o valor novo. Um "escrevi" otimista
  esconderia a escrita que não aconteceu.
- **Data breakpoint** vale para global, local e elemento de array. O de uma
  local expira quando a função dona retorna — a célula passa a ser de outra
  coisa.
- **Erros de runtime** (divisão por zero, índice fora do limite, colisão entre
  pilha e heap, underflow de heap, acesso inválido) pausam com a mensagem no
  idioma configurado, traduzida no `protocol`.

## Enquanto está pausado

A pausa acontece dentro da VM: nenhum callback, timer ou comando do Pawn
executa até continuar. A rede do servidor segue de pé — ele continua
respondendo à consulta de status —, e o que chegou durante a pausa é processado
de uma vez ao continuar, inclusive os timers atrasados.

Por isso a depuração é para servidor local de desenvolvimento, não para um
servidor com jogadores.
