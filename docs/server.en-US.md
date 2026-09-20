# The game server

Everything that talks to `omp-server` / `samp03svr` lives in
`core/src/server/`: finding the executable, reading its configuration, probing
the port, knowing who owns the process, killing it, sending RCON commands, and
following the log. They are angles of the same responsibility, and **getting
the boundary between them wrong is where the defects that motivated the core
came from**.

## Finding the server

With `server.path` empty, the executable is looked up in the usual places of
the project — the root, `server/`, `samp/`, `samp-server/`, `samp03/` and
`open.mp/` — under the known names of each platform.

The server's own configuration comes in two incompatible formats, and host,
port and RCON password come from them:

| Server | File | Format |
|---|---|---|
| SA-MP | `server.cfg` | `key value`, one per line |
| open.mp | `config.json` | JSON, with `rcon.enable` and `logging.file` |

The log follows the same origin: `server_log.txt` on SA-MP, and the
`config.json`'s `logging.file` on open.mp (`log.txt` by default).

## Whose process is it

The port comes from the repository's `config.json` — a file the project
controls. A gamemode with `"port": 53` would turn the kill button into a weapon
against system services.

So no destructive operation acts on "whoever is on the port", but on **whoever
is on the port and is this project's server**: same executable, same user. The
filter is applied when listing and checked again at kill time, including when
the request arrives over RPC — the extension cannot bypass the policy by asking
the core directly.

Killing is graceful first and forced after the deadline, so the server gets a
chance to save. A zombie does not count as alive: it has already died and only
occupies the table until its parent reaps it, and treating it as alive made the
kill burn the whole deadline and then report a failure that never happened.

## Following the log

The server writes to a file and notifies no one, so following it means
comparing the size every so often and reading what grew.

The read is **stateless**: whoever follows sends the previous position and gets
back the new text plus the next position. With no position, only the size is
measured — opening the panel must not dump the log of previous runs. A file
that shrank was recreated by the server on restart, and reading starts over
from its beginning.

The text is decoded with the configured encoding (`windows-1252` by default,
the usual one in the Pawn ecosystem) before it leaves here: whoever displays it
gets text, not bytes.

## Commands carrying a password

The panel's history goes to `.pawnpro/state.json`, in the clear and inside the
project: a `login secret123` there would be committed along. The command is
still sent, but it is not recorded when:

1. the name already indicates a credential (`login`, `rcon_password`,
   `password`, `changepass[word]`, `setpass[word]`);
2. an argument announces one (`--senha`, `token`, `key`, `secret`, `auth` and
   the like, with or without `=`);
3. an argument **looks like** one: eight characters or more mixing letters and
   digits. Numbers, IPs and coordinates are deliberately left out — a false
   positive would make the history useless.

A project can add its own commands in `server.history.sensitiveCommands`.

## Before debugging

The debug plugin is checked before the session starts the server: whether it is
in the right place (`components/` on open.mp, or `plugins/`), whether it is
registered when the server requires it, whether it is the official binary, and
whether its architecture matches the server's.

When something is wrong the server refuses the plugin at boot and writes the
reason in the middle of dozens of loading lines, where nobody sees it. Checking
beforehand is what lets the editor say exactly what is missing.

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
