# Architecture

## The problem behind the core

Three processes, with the extension coordinating all of them from outside:

```
editor ──► extension (TS) ──► engine     (LSP, stdio)
                         └──► adapter    (DAP, stdio) ──► omp-server
```

The extension made decisions about `omp-server` without owning it. To know
whether it was up, it probed a UDP port with no retransmission; to know whether
it could kill it, it read `/proc` or shelled out to `lsof`. Every decision came
from an indirect signal, and every signal had a different shape per OS.

## The design

```
editor ──► extension (TS) ──► pawnpro-core (single binary)
                                   │
                                   ├── engine    (crate, supervised task)
                                   ├── debugger  (crate, supervised task)
                                   └── rcon      (crate)
                                           └──► omp-server
```

The core exposes LSP and DAP over local sockets, and the extension points its
clients at them. Whoever owns `omp-server` is now who answers for it.

## Principles

Inherited from the work that motivated this migration, and equally valid here:

1. **The port is the only proof.** An open terminal, a received event, a
   dispatched command — none of them mean the server is up.
2. **Asking is not finishing.** A UDP `send` returning `Ok` does not mean anyone
   received it. That is why [`RconClient::send`](supervision.md#rcon) probes first.
3. **Expiring state does not drive control flow.** Loss tolerance is for
   display, never for deciding what to do.
4. **Errors are enums, not strings.** Every condition the UI must tell apart
   becomes a variant — and the exhaustive `match` forces it to be handled.

## Why separate crates, not one

A single crate would lose the boundary that provides isolation. With crates,
`debugger`'s `Cargo.toml` does not declare `engine`: using one from the other is
a compile error. That boundary is what lets a panic in the engine leave
debugging alive.

For the same reason the release profile does **not** set `panic = "abort"`:
without unwinding there is no `catch_unwind` at each subsystem's edge, and any
panic would kill the whole process.
