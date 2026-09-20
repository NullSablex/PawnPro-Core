# O servidor do jogo

Tudo que fala com o `omp-server` / `samp03svr` mora em `core/src/server/`:
descobrir o executável, ler a configuração dele, sondar a porta, saber de quem
é o processo, encerrá-lo, mandar comandos por RCON e acompanhar o log. São
ângulos da mesma responsabilidade, e **errar a fronteira entre eles foi a
origem dos defeitos que motivaram o núcleo**.

## Descobrir o servidor

Com `server.path` vazio, o executável é procurado nos lugares usuais do
projeto — a raiz, `server/`, `samp/`, `samp-server/`, `samp03/` e `open.mp/` —
pelos nomes conhecidos de cada plataforma.

A configuração do servidor vem de dois formatos incompatíveis, e deles saem
host, porta e senha de RCON:

| Servidor | Arquivo | Formato |
|---|---|---|
| SA-MP | `server.cfg` | `chave valor`, uma por linha |
| open.mp | `config.json` | JSON, com `rcon.enable` e `logging.file` |

O log segue a mesma origem: `server_log.txt` no SA-MP, e o `logging.file` do
`config.json` no open.mp (padrão `log.txt`).

## De quem é o processo

A porta vem do `config.json` do repositório — um arquivo que o projeto controla.
Um gamemode com `"port": 53` transformaria o botão de encerrar numa arma contra
serviços do sistema.

Por isso nenhuma operação destrutiva age sobre "quem está na porta", e sim sobre
**quem está na porta e é o servidor deste projeto**: mesmo executável, mesmo
usuário. O filtro é aplicado ao listar e conferido de novo na hora de encerrar,
inclusive quando o pedido vem por RPC — a extensão não contorna a política
pedindo direto.

Encerrar é gracioso primeiro e forçado depois do prazo, para o servidor ter a
chance de salvar. Um processo zumbi não conta como vivo: ele já morreu e só
ocupa a tabela até o pai colhê-lo, e tratá-lo como vivo fazia o encerramento
gastar o prazo inteiro para depois relatar uma falha que não houve.

## Acompanhar o log

O servidor escreve num arquivo e não avisa ninguém, então acompanhar é comparar
o tamanho de tempos em tempos e ler o que cresceu.

A leitura **não guarda estado**: quem acompanha manda a posição anterior e
recebe o texto novo mais a posição seguinte. Sem posição, só o tamanho é
medido — abrir o painel não deve despejar o log de execuções anteriores. Um
arquivo que encolheu foi recriado pelo servidor ao reiniciar, e a leitura
recomeça do início dele.

O texto é decodificado na codificação configurada (`windows-1252` por padrão, a
usual do ecossistema Pawn) antes de sair daqui: quem exibe recebe texto, não
bytes.

## Comandos com senha

O histórico do painel vai para `.pawnpro/state.json`, em texto claro e dentro do
projeto: um `login senha123` ali seria commitado junto. O comando é enviado
normalmente, mas não é registrado quando:

1. o nome já indica credencial (`login`, `rcon_password`, `password`,
   `changepass[word]`, `setpass[word]`);
2. um argumento se anuncia como tal (`--senha`, `token`, `key`, `secret`, `auth`
   e afins, com ou sem `=`);
3. um argumento **parece** credencial: oito caracteres ou mais, misturando
   letras e dígitos. Números, IPs e coordenadas ficam de fora, de propósito — um
   falso positivo faria o histórico deixar de servir.

O projeto pode acrescentar os comandos próprios em
`server.history.sensitiveCommands`.

## Antes de depurar

O plugin de depuração é conferido antes de a sessão subir o servidor: se está
no lugar certo (`components/` no open.mp, ou `plugins/`), se está registrado
quando o servidor exige, se é o binário oficial e se a arquitetura casa com a
do servidor.

Quando algo está errado, o servidor recusa o plugin no boot e escreve o motivo
no meio de dezenas de linhas de carga, onde ninguém vê. Conferir antes é o que
permite ao editor dizer exatamente o que falta.

## RCON

Primeiro subsistema migrado, escolhido por ser o que mais falhava em silêncio.

### Os defeitos que a versão em TypeScript tinha

**Comando enviado a servidor parado era reportado como sucesso.** O protocolo é
UDP sem retransmissão: com o servidor fora do ar, o datagrama some sem erro
nenhum, e a interface respondia "enviado (este comando não devolve texto)".

**A saída saía fora de ordem.** A resposta chega depois de um silêncio que fecha
a rajada de datagramas. Dois comandos rápidos tinham suas respostas trocadas,
porque nada correlacionava resposta e comando.

**Host não-IPv4 gerava pacote com endereço errado.** O cabeçalho do protocolo
tem 4 bytes de IP, e a versão anterior caía num `127.0.0.1` fixo para qualquer
host que não fosse IPv4 numérico — mandando o pacote com um endereço que não era
o do destino.

### Como o desenho em Rust os elimina

| Defeito | O que impede a recorrência |
|---|---|
| Fingir envio | `send` sonda a porta antes e devolve `RconError::ServerDown`. Não existe caminho que produza um `RconReply` sem servidor. |
| Saída fora de ordem | `RconReply` carrega o `command` que a originou. A correlação é do tipo, não da ordem de chegada. |
| Endereço errado | `ipv4_octets` devolve `Option`: um host que não cabe no cabeçalho é recusado, nunca substituído por um palpite. |
| "Falhou" genérico | `RconError` é um `enum` por condição. Quem consome precisa de um `match` exaustivo. |

### Ordem das checagens

Deliberada — as baratas e conclusivas primeiro, e só então a que custa I/O:

```
send(command)
  ├─ RCON desligado no config.json?     → Disabled
  ├─ host fora do loopback?             → RemoteBlocked
  ├─ senha ausente ou padrão?           → InvalidPassword
  ├─ porta não responde?                → ServerDown      ← única com I/O
  └─ envia, lê a rajada até o silêncio  → RconReply
```

O bloqueio fora do loopback não é cosmético: o RCON manda a senha **em texto
claro**. `is_loopback_host` erra para o lado seguro — `0.0.0.0` é o curinga
"todas as interfaces" e **não** conta como local.
