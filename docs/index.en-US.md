# PawnPro Core

Rust core of [PawnPro](https://github.com/NullSablex/PawnPro). It hosts the
subsystems the extension used to run as separate processes — the engine (LSP)
and the debugger (DAP) — and owns every operation that depends on the operating
system.

## Why it exists

The TypeScript extension commanded processes it did not own: it discovered who
held a port, decided whether a process belonged to the project, and killed it,
all through indirect signals. Each of those operations had a per-system
implementation (`lsof`, `/proc`, `ps`, `netstat`, `taskkill`), hand-written and
hard to test.

The core inverts that: **whoever owns the process answers for it**.

## Layout

| Crate | Responsibility |
|---|---|
| `crates/core` | The `pawnpro-core` binary: JSON-RPC with the extension, supervisor, socket, and everything that depends on the OS |
| `crates/engine` | Pawn analysis and LSP, as a library |
| `crates/debugger/adapter` | DAP adapter, as a library |
| `crates/debugger/protocol` | The protocol between adapter and plugin |
| `crates/debugger/plugin` | The server plugin (`pawnpro_debug.so` / `.dll`), 32-bit |

One crate per architectural piece. Internal
responsibilities — RCON, processes, ports — are **modules** of the crate they
belong to (`core/src/server/`), not crates of their own: one crate per function
would multiply boundaries without separating anything.

Separate crates, a single binary. The split is not cosmetic: each `Cargo.toml`
prevents accidental coupling, and it is what lets one subsystem fail without
taking the others down.

## Status

The core owns RCON, processes, ports, the compiler, configuration and project
state, and hosts both the engine and the debug adapter on a local socket it
supervises. **Debugging is unstable**: it works for the usage described in the
extension's documentation, but it can still fail.

See [Architecture](architecture.md) and [Supervision](supervision.md).
