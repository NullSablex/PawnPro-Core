# Supervision

## Subsystems

The engine and each debug session run on threads of their own, with
`catch_unwind` at the edge: a panic becomes that subsystem's crash, and the
others never notice. The supervisor brings back whatever fell.

```
crash ──► wait 500 ms ──► start again
   │
   ├─ 5 restarts that don't  → give up on the next crash (`failed`)
   │     hold
   └─ 30 s up without one    → the counter resets
```

Resetting the counter matters: a crash today and another an hour from now are
not the same problem, and treating them as one would make the subsystem give up
by accumulation. The extension follows this through the `core.subsystemStatus`
notification and tells the user — how many times the engine came back, or that
it gave up.

This is why the release profile does **not** use `panic = "abort"`: without
unwinding there is no `catch_unwind`, and any panic would kill the whole
process — the opposite of what the supervisor exists for.

### The game server is the session's child

The debug session, not the extension, starts `omp-server`. That is what keeps
it from outliving the session:

- the session's `Drop` kills the process and reaps it;
- on Linux the child gets `PR_SET_PDEATHSIG`, so it dies along with the thread
  that spawned it if that thread disappears without running `Drop`.

The extension tracks no debug PID: it asks for a stop, and whoever owns the
process ends it.

## RCON

The first migrated subsystem, chosen because it was the one failing silently
most often.

### The defects the TypeScript version had

**A command sent to a stopped server was reported as success.** The protocol is
UDP with no retransmission: with the server down the datagram vanishes without
any error, and the UI answered "sent (this command returns no text)".

**Output came out of order.** The reply arrives after a silence that closes the
burst of datagrams. Two quick commands had their replies swapped, because
nothing correlated reply and command.

**A non-IPv4 host produced a packet with the wrong address.** The protocol
header carries 4 bytes of IP, and the previous version fell back to a fixed
`127.0.0.1` for any host that was not a numeric IPv4 — sending the packet with
an address that was not the destination's.

### How the Rust design rules them out

| Defect | What prevents a recurrence |
|---|---|
| Pretending to send | `send` probes the port first and returns `RconError::ServerDown`. No path yields an `RconReply` without a server. |
| Out-of-order output | `RconReply` carries the `command` that produced it. Correlation is in the type, not in arrival order. |
| Wrong address | `ipv4_octets` returns `Option`: a host that does not fit the header is refused, never replaced by a guess. |
| Generic "failed" | `RconError` is an enum per condition. Consumers need an exhaustive `match`. |

### Order of checks

Deliberate — the cheap, conclusive ones first, and only then the one that costs
I/O:

```
send(command)
  ├─ RCON disabled in config.json?      → Disabled
  ├─ host outside loopback?             → RemoteBlocked
  ├─ password missing or default?       → InvalidPassword
  ├─ port not answering?                → ServerDown      ← the only I/O one
  └─ send, read the burst until silence → RconReply
```

Blocking non-loopback hosts is not cosmetic: RCON sends the password **in the
clear**. `is_loopback_host` errs on the safe side — `0.0.0.0` is the "all
interfaces" wildcard and does **not** count as local.
