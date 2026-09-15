# PawnPro Core

Rust core of [PawnPro](https://github.com/NullSablex/PawnPro). It supervises the
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
| `crates/core` | Supervisor, and everything that depends on the OS |
| `crates/engine` | Pawn analysis and LSP |
| `crates/debugger` | DAP and the game server *(to migrate)* |

Three crates, and only those: the architectural pieces. Internal
responsibilities — RCON, processes, ports — are **modules** of the crate they
belong to (`core/src/server/`), not crates of their own: one crate per function
would multiply boundaries without separating anything.

Separate crates, a single binary. The split is not cosmetic: each `Cargo.toml`
prevents accidental coupling, and it is what lets one subsystem fail without
taking the others down.

## Status

Under construction. The core already owns RCON, processes and configuration,
and hosts the engine on a local socket it supervises. The debugger is still
missing — see [Architecture](architecture.md) and [Supervision](supervision.md).
