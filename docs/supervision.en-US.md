# Supervision

## Subsystems

The engine and each debug session run on threads of their own, with
`catch_unwind` at the edge: a panic becomes that subsystem's crash, and the
others never notice. The supervisor brings back whatever fell.

```
crash ──► wait 500 ms ──► start again
   │
   ├─ 5 restarts that don't hold  → give up on the next crash (`failed`)
   └─ 30 s up without a crash     → the counter resets
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
