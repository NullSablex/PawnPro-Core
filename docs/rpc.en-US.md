# The contract with the extension

The extension talks to the core over **JSON-RPC 2.0 on stdio**, one message per
line. There is no `Content-Length` like in LSP: with no binary payload and no
streaming, the header would only add work on both ends.

```
extension ──► {"jsonrpc":"2.0","id":1,"method":"server.resolve","params":{…}}
extension ◄── {"jsonrpc":"2.0","id":1,"result":{…}}
extension ◄── {"jsonrpc":"2.0","method":"config.changed","params":{…}}
```

`core.version` returns the version and **the list of methods the running
version answers**. That is how the extension discovers what it may ask of an
older core, instead of trying and handling the error. A test guarantees every
name in that list is dispatchable: a method listed but not implemented would
break the contract silently.

## The method groups

| Group | Methods | For what |
|---|---|---|
| Core | `core.version` | Version and available methods |
| Configuration | `config.set`, `config.delete`, `config.reload`, `config.open`, `config.ensureNamingFiles`, `config.inlineNamingLists`, `config.migrateNaming`, `config.backupNaming` | Read, write and migrate the `config.json` files and the `.ban`/`.allow` lists |
| State | `state.get`, `state.updateServer` | Panel history and favourites, in `.pawnpro/state.json` |
| Compiler | `compiler.detect`, `compiler.buildArgs`, `compiler.run` | Find `pawncc`, build the command line, and run it |
| Includes | `includes.paths`, `includes.listFiles`, `includes.listNatives`, `includes.resolveSdk` | The roots, the `.inc` scan, and the SDK |
| Server | `server.resolve`, `server.loadConfig`, `server.ping`, `server.pidsOnPort`, `server.projectServersOnPort`, `server.kill`, `server.readLog`, `server.sensitiveCommands`, `rcon.send` | Executable, log, ports, processes and RCON |
| Debugging | `debug.preflight`, `debug.start` | Check the plugin and open a session |
| Engine | `engine.start` | Start the engine and return the socket address |
| Logging | `log.configure`, `log.write`, `log.clear` | Diagnostics in `.pawnpro/logs/` |
| Project | `project.changelogSection` | The changelog section for "What's New" |

Notifications go the other way, without an `id`:

| Notification | When |
|---|---|
| `config.changed` | A `config.json` or a list changed on disk — the extension refreshes its cache without asking |
| `core.subsystemStatus` | A subsystem started, crashed and came back, or gave up (see [Supervision](supervision.md)) |

## What belongs to the core, and why

There is a single rule: **whoever owns the resource answers for it**. The rest
follows from it.

- **Configuration has one owner.** The extension used to read `config.json` and
  so did the engine; two readers of the same file are two possible answers. The
  core reads it, merges the scopes, resolves includes and SDK, and hands the
  result over — to the extension over RPC, to the engine over a typed channel.
- **Processes and ports** have a single implementation, instead of one per
  operating system scattered across `lsof`, `/proc`, `ps` and `taskkill`.
- **The ownership policy also holds over RPC.** `server.kill` refuses a PID that
  is not this project's server: the extension cannot work around the rule by
  asking the core directly.

## Slow work does not hold the loop

The message loop handles one request at a time, which is good for ordering and
bad for anything slow. Compiling takes seconds, and a `compiler.run` inside the
loop would leave the editor with no IntelliSense, no panel and no configuration
until `pawncc` finished.

That is why `compiler.run` is a **job**: it runs on a thread of its own and
answers when it is done, with the request's `id`. The loop keeps serving
everything else, and the order of the replies stops being the order of the
requests — which JSON-RPC already allows.

## Errors

The codes are JSON-RPC's (`-32600` and friends). A condition the interface has
to tell apart does not become prose: RCON, for instance, returns the named
failure (`serverDown`, `disabled`, `invalidPassword`, `remoteBlocked`,
`timeout`, `io`), and the extension picks the message. Error text cannot be
compared; an enum variant can.
