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
