# Dependencies and build

What each dependency solves, and why the release profile looks like this. It
lives here, not in a `Cargo.toml` comment: a manifest is for declaring versions,
and the reason behind a choice ages at a different pace than its number.

## What each one does

| Crate | Where | Why |
|---|---|---|
| `serde`, `serde_json` | core, engine | The JSON-RPC with the extension and reading `config.json`. |
| `regex` | core, engine | Scanning Pawn sources and the server's configuration files. |
| `encoding_rs` | core | `pawncc` output comes in windows-1252 on most builds. It is the counterpart of the `iconv-lite` the extension used. |
| `sysinfo` | core | Cross-platform process inspection. Replaces the per-OS paths the extension kept by hand — `/proc`, `ps`, `Get-Process`, `taskkill`. |
| `tokio` | core, engine | The engine's runtime and the socket it listens on. The core asks only for `rt-multi-thread`, `net` and `time`: the `accept` deadline is what lets shutdown get through. |
| `tower-lsp` | engine | The LSP protocol. |
| `walkdir`, `dashmap`, `futures` | engine | Workspace scanning, the open-document cache, and composing the analyses. |

`dashmap` stays on 6 on purpose: 7 is still a release candidate.

## Release profile

```toml
lto = true
codegen-units = 1
strip = true
```

The binary ships inside the VSIX, so size matters more than build time — all
three options cost compilation, not execution.

**There is no `panic = "abort"`, and that is deliberate.** The core supervises
the engine and the debugger, and a panic in one of them must take down only that
subsystem. With `abort` there is no unwind, the `catch_unwind` at the boundary
catches nothing, and the whole process dies — the opposite of what the
supervisor exists for. See [Supervision](supervision.md).

## One version for everything

The crates use `version.workspace = true`. They ship together, in a single
binary; versioning them separately would only raise the question "which one is
the one that matters".

## Updating

```bash
cargo update                                  # within semver, touches Cargo.lock
cargo search <crate> --limit 20               # check for a higher version
cargo test --workspace                        # before committing the lock
```

## Dependency licenses

The binaries redistribute the libraries compiled into them, and their licenses
require shipping the notices along. The release generates
`pawnpro-core-THIRD-PARTY.txt` and `pawnpro_debug-THIRD-PARTY.txt` with
`cargo-about`, and the extension packages the former next to the binary.

`about.toml` lists the accepted licenses. A new dependency under a license
outside the list breaks the release: accepting it is a decision, not a side
effect of `cargo update`. The workspace's own crates have `publish = false` and
are left out — they are covered by the PawnPro-Core license.
