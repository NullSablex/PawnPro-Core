# Debugging, from the inside

!!! warning "Unstable"

    Debugging works for the usage described in the
    [extension's guide](https://pawnpro.nullsablex.com/debugging/), but it can
    still fail. This page describes how it is built.

Two pieces and one socket:

```
editor ──DAP──► adapter (in the core) ──spawn──► omp-server
                    ▲                               │ loads
                    └────────── socket ──────────── plugin
```

The **adapter** (`crates/debugger/adapter`) translates the editor's DAP into
the commands the **plugin** (`crates/debugger/plugin`) runs inside the Pawn
virtual machine. What travels between them is `crates/debugger/protocol` — an
enum of commands and one of events, serialised as JSON lines.

## How the plugin finds the session

The session starts the game server with four environment variables, which the
plugin reads when it loads:

| Variable | For what |
|---|---|
| `PAWNPRO_DBG_ENDPOINT` | The core's socket address |
| `PAWNPRO_DBG_SESSION` | The id it introduces itself with (`PAWNPRO/1 plugin <id>`) |
| `PAWNPRO_DBG_AMXDBG` | The `.amx` being debugged, where it reads the debug block from |
| `PAWNPRO_DBG_LOCALE` | The language of the runtime error messages |

Without those variables the plugin does nothing: loaded into a server started
by hand, it stays quiet instead of trying to connect somewhere.

## Only the debugged program's VM

The server loads several virtual machines — the gamemode and each filterscript
— and the plugin gets them all. Code addresses start at zero in each one: a
breakpoint applied to the wrong VM would stop inside another script, shown with
the lines and variables of the debugged program.

The plugin recognises the right VM **by its contents**, comparing it with the
`.amx` the session pointed at: the header, the publics table and the name
table, which the server does not alter while loading. Left out are the code,
which is relocated at load time, and the natives table, which receives
addresses as plugins register functions.

That is what stops a filterscript from firing the gamemode's breakpoints.

## Breakpoints with the source already edited

The breakpoint the editor sends is a line of the file **as it is now**; what
runs is the `.amx` compiled earlier. Editing during the session misaligns the
two, and the previous version stopped in the wrong place.

The session keeps each file's text as of the build and compares it with the
current text through a line diff:

```
line in the editor ──diff──► line in the build ──debug block──► address
```

A line that did not exist at build time has no address, and the breakpoint
stays **unverified** instead of stopping somewhere else — the editor shows it
greyed out, and the console explains that a restart is needed to rebuild.

## Restarting and rebuilding

The adapter decides about rebuilding, in `restart`: every restart goes through
it, whether from the button, the key or the palette. Compiling, however,
belongs to the extension — the compiler and its flags are project
configuration.

```
restart ──► source newer than the .amx, or .amx without a debug block?
              ├─ no  → start the server again
              └─ yes → `pawnproRebuild` event ──► the extension compiles
                                                   └─► restart again
```

If the build fails, the extension does not resend the `restart`: that is what
keeps the server from coming up with the stale binary.

## Stopping

`terminate` brings the server down and keeps the session alive; `disconnect`
ends the session. They are different things in DAP, and treating them as
synonyms is what left the editor's progress bar waiting for an end that had
already happened.

## Inspection

- **Arrays** are addressed the way the compiler lays them out: the adapter
  describes the path to the element (`grid[1][2]` is a two-index path), and the
  plugin dereferences what is a reference (`iREFERENCE`, `iREFARRAY`) and adds
  the indirection offsets. Without that, a multidimensional array showed the
  value of another position in memory.
- **Editing** a variable is a command with an id; the plugin confirms with a
  correlated event, and only then does the editor show the new value. An
  optimistic "written" would hide a write that never happened.
- **Data breakpoints** work for globals, locals and array elements. A local's
  expires when the owning function returns — the cell then belongs to something
  else.
- **Runtime errors** (division by zero, index out of bounds, stack/heap
  collision, heap underflow, invalid memory access) pause with the message in
  the configured language, translated in `protocol`.

## While paused

The pause happens inside the VM: no callback, timer or Pawn command runs until
it continues. The server's networking stays up — it keeps answering the status
query — and whatever arrived during the pause is processed at once when it
continues, including the late timers.

That is why debugging targets a local development server, not one with players.
