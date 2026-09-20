# O contrato com a extensão

A extensão fala com o núcleo por **JSON-RPC 2.0 no stdio**, uma mensagem por
linha. Não há `Content-Length` como no LSP: sem corpo binário nem streaming,
o cabeçalho só acrescentaria trabalho aos dois lados.

```
extensão ──► {"jsonrpc":"2.0","id":1,"method":"server.resolve","params":{…}}
extensão ◄── {"jsonrpc":"2.0","id":1,"result":{…}}
extensão ◄── {"jsonrpc":"2.0","method":"config.changed","params":{…}}
```

`core.version` devolve a versão e **a lista de métodos que a versão em execução
atende**. É assim que a extensão descobre o que pode pedir a um núcleo mais
antigo, em vez de tentar e tratar o erro. Um teste garante que todo nome dessa
lista é despachável: um método listado e não implementado quebraria o contrato
em silêncio.

## Os grupos de métodos

| Grupo | Métodos | Para quê |
|---|---|---|
| Núcleo | `core.version` | Versão e métodos disponíveis |
| Configuração | `config.set`, `config.delete`, `config.reload`, `config.open`, `config.ensureNamingFiles`, `config.inlineNamingLists`, `config.migrateNaming`, `config.backupNaming` | Ler, gravar e migrar os `config.json` e as listas `.ban`/`.allow` |
| Estado | `state.get`, `state.updateServer` | Histórico e favoritos do painel, em `.pawnpro/state.json` |
| Compilador | `compiler.detect`, `compiler.buildArgs`, `compiler.run` | Achar o `pawncc`, montar a linha de comando e executar |
| Includes | `includes.paths`, `includes.listFiles`, `includes.listNatives`, `includes.resolveSdk` | As raízes, a varredura de `.inc` e o SDK |
| Servidor | `server.resolve`, `server.loadConfig`, `server.ping`, `server.pidsOnPort`, `server.projectServersOnPort`, `server.kill`, `server.readLog`, `server.sensitiveCommands`, `rcon.send` | Executável, log, portas, processos e RCON |
| Depuração | `debug.preflight`, `debug.start` | Conferir o plugin e abrir uma sessão |
| Engine | `engine.start` | Subir a engine e devolver o endereço do soquete |
| Registro | `log.configure`, `log.write`, `log.clear` | O diagnóstico em `.pawnpro/logs/` |
| Projeto | `project.changelogSection` | A seção do changelog para "O que há de novo" |

As notificações vão no sentido contrário, sem `id`:

| Notificação | Quando |
|---|---|
| `config.changed` | O `config.json` ou uma lista mudou no disco — a extensão atualiza o cache sem perguntar |
| `core.subsystemStatus` | Um subsistema subiu, caiu e voltou, ou desistiu (ver [Supervisão](supervision.md)) |

## O que é do núcleo, e por quê

A regra é uma só: **quem possui o recurso responde sobre ele**. Disso decorre o
resto.

- **A configuração tem um dono.** Antes a extensão lia o `config.json` e a
  engine também; dois leitores do mesmo arquivo são dois resultados possíveis. O
  núcleo lê, mescla os escopos, resolve includes e SDK, e entrega pronto —
  para a extensão por RPC, para a engine por um canal tipado.
- **Processos e portas** têm uma implementação só, em vez de uma por sistema
  operacional espalhada em `lsof`, `/proc`, `ps` e `taskkill`.
- **A política de dono vale também por RPC.** `server.kill` recusa um PID que
  não seja o servidor do projeto: a extensão não contorna a regra pedindo
  direto ao núcleo.

## Trabalho demorado não prende o laço

O laço de mensagens trata um pedido por vez, o que é bom para a ordem e ruim
para o que demora. Compilar leva segundos, e um `compiler.run` no laço deixaria
o editor sem IntelliSense, sem painel e sem configuração até o `pawncc`
terminar.

Por isso `compiler.run` é um **job**: roda numa thread própria e responde quando
acaba, com o `id` do pedido. O laço segue atendendo o resto, e a ordem das
respostas deixa de ser a ordem dos pedidos — o que o JSON-RPC já prevê.

## Erros

Os códigos são os do JSON-RPC (`-32600` e seguintes). Uma condição que a
interface precisa distinguir não vira texto: o RCON, por exemplo, devolve a
falha nomeada (`serverDown`, `disabled`, `invalidPassword`, `remoteBlocked`,
`timeout`, `io`), e a extensão escolhe a mensagem. Texto de erro não se
compara; variante de enum, sim.
