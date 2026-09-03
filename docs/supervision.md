# Supervisão

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
