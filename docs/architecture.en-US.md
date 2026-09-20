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
              │                    │
              │                    ├── engine   (crate, supervised thread)
              │                    ├── adapter  (crate, one thread per session)
              │                    └── server   (module: RCON, processes, ports)
              │                              └──► omp-server ──► plugin
              │                                                     │
              └── LSP and DAP ──► local socket ◄────────────────────┘
                                  (Unix; named pipe on Windows)
```

The core's JSON-RPC travels over stdio; LSP and DAP would not fit in the same
channel, so there is a socket. The extension asks for the address
(`engine.start` for IntelliSense, `debug.start` for a debug session) and points
its client at it. Whoever owns `omp-server` is now who answers for it.

## One socket, three channels

There is a single address, and every connection states its channel on the first
line:

```
PAWNPRO/1 lsp                  ← the editor's LSP client
PAWNPRO/1 dap                  ← the editor's debug session
PAWNPRO/1 plugin <session>     ← the plugin, from inside the game server
```

One socket per subsystem would multiply addresses, directories and cleanup to
solve the same problem three times. The greeting is a single text line, read
byte by byte with a deadline and a maximum length: a connection that does not
introduce itself is dropped, and none of them reaches the wrong subsystem.

The plugin gets the address and the session id from the environment when the
adapter starts the game server. That id is what binds the plugin to the right
session when more than one is open.

**Not TCP on loopback.** LSP authenticates no one, and the engine reads from
disk whatever path the incoming URI points at: on a local port, any process on
the machine — under any user — could connect and ask for the contents of any
file the session owner can read. A Unix socket inside a `0700` directory has the
filesystem refuse that; on Windows the equivalent is a named pipe.

The address is reserved once and survives the engine's restarts: when it falls
and the supervisor brings it back, the extension reconnects to the same place
instead of having to discover it again.

## Who delivers the configuration

The core, and only the core. The engine opens neither `config.json` nor the list
files: the core reads both scopes, resolves includes, SDK, formatting and
naming, and delivers the result over an internal channel — a Rust `struct`, not
a JSON object, because the two compile together and the compiler can enforce the
agreement.

```
config.json (global + project)  ──►  core  ──►  typed channel  ──►  engine
.ban / .allow                        │
                                     └── polls the timestamps
```

The core is also who notices the change: it polls the files' timestamps every
two seconds and re-delivers when one moves. The engine republishes diagnostics
without the editor asking. This used to be the extension's job, watching the
files and sending `workspace/didChangeConfiguration` — the same work the core
now does, from the side that owns the files.

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
