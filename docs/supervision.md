# Supervisão

## Subsistemas

A engine e cada sessão de depuração rodam em threads próprias, com
`catch_unwind` na borda: um panic vira queda daquele subsistema, e os outros
nem ficam sabendo. O supervisor levanta o que caiu.

```
queda ──► espera 500 ms ──► sobe de novo
   │
   ├─ 5 reinícios sem durar   → desiste na queda seguinte (`failed`)
   └─ 30 s de pé sem cair     → o contador zera
```

O contador zerar importa: uma queda hoje e outra daqui a uma hora não são o
mesmo problema, e tratá-las como se fossem faria o subsistema desistir por
acúmulo. A extensão acompanha isso pela notificação `core.subsystemStatus` e
avisa o usuário — quantas vezes a engine voltou, ou que ela desistiu.

Por isso o perfil de release **não** usa `panic = "abort"`: sem unwind não há
`catch_unwind`, e qualquer panic mataria o processo inteiro, que é o oposto do
que o supervisor existe para fazer.

### O servidor do jogo é filho da sessão

Quem sobe o `omp-server` é a sessão de depuração, e não a extensão. Isso é o que
garante que ele não sobreviva a ela:

- o `Drop` da sessão mata o processo e o colhe;
- no Linux, o filho recebe `PR_SET_PDEATHSIG`, então morre junto se a thread que
  o criou desaparecer sem passar pelo `Drop`.

A extensão não rastreia PID nenhum da depuração: ela pede para parar, e quem
possui o processo o encerra.

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
