# Configuration and project

PawnPro's configuration has **one owner**: the core. The extension used to read
`config.json` in TypeScript and the engine read it again in Rust — two readers
of the same file are two possible results, and IntelliSense could disagree with
what the build used.

## Scopes and merging

```
defaults  ◄──  ~/.pawnpro/config.json  ◄──  <project>/.pawnpro/config.json
  (code)             (global)                        (wins)
```

Merging happens on the **raw JSON**, before it becomes the typed structure:
afterwards there would be no way to tell "the user wrote `false`" from "the
default is `false`", and the project scope could not override the global one
predictably.

Every field has a default, and whatever is missing is filled in: a
`config.json` with a single key is valid. A value with the wrong type is
ignored on its own — the rest of the file still applies, and the rejected key
is reported to the caller, instead of bringing the whole read down.

`${workspaceFolder}` is resolved by the core, including inside default values:
whoever receives the configuration receives real paths.

## Who gets told

The core checks the timestamps of the configuration files and of the
`.ban`/`.allow` lists every two seconds. When something changes:

- the **extension** gets the `config.changed` notification and refreshes its
  cache without having asked;
- the **engine** gets the already-resolved configuration over a typed channel
  and republishes its diagnostics.

Switching projects moves the watch along.

## Project files

| File | What it holds |
|---|---|
| `~/.pawnpro/config.json` | Global configuration |
| `.pawnpro/config.json` | Project configuration |
| `.pawnpro/state.json` | Local state: the server panel's history and favourites |
| `.pawnpro/*.ban` / `*.allow` | The naming assistant's long lists |
| `.pawnpro/logs/` | Diagnostics log, when enabled |

**State is not configuration.** It is operational data belonging to whoever is
developing — it does not belong in the repository nor to another user of the
machine. That is why `state.json` is written atomically with restricted
permissions, and the core creates a `.pawnpro/.gitignore` that excludes it
without touching the project's own `.gitignore`.

## Name lists

The naming assistant's long lists live in `.ban`/`.allow` files, one term per
line, rather than in the JSON: hundreds of terms inside `config.json` make the
file unreadable and hard to review in a diff.

The core reads the file (up to the configured ceiling) and, if it does not
exist or is empty, falls back to the inline list in the JSON. Migrating from
one to the other is manual, with a backup — migrating on its own would rewrite
the user's file unasked.

## Includes

The include roots come from **a single function**, used by the engine, by the
build and by the editor's include tree. Having three similar calculations was
what made `#include` resolve one way in the analysis and another in the
compiler.

The list is assembled in this order, without repeats and only with directories
that exist: whatever is in `includePaths`, whatever comes from `-i` in
`compiler.args`, and then `qawno/include`, `pawno/include` and `include` at the
project root. If none of those exist at the root, the search walks up from the
open file's directory — a project may hold a subproject with includes of its
own.

The open.mp SDK (`open.mp.inc`) is looked up first where the server installs it
(`qawno/include`) and then in the configured includes. A configured path beats
both, but only if it exists: pointing at a missing file is a user mistake, and
a guess would hide it.

## The compiler

The core builds the `pawncc` command line, with the open project's
configuration; the extension only shows the output.

- **Finding the binary**, from the most explicit to the most generic: the
  `PAWNCC` variable, `compiler.path`, the `PATH`, the project directories
  (`qawno`, `pawno`, `include`, `tools`, `bin`), and the common install paths.
  With auto-detection off, a `compiler.path` that does not work is an error —
  the user wants that compiler, and falling back to another would be worse than
  failing.
- **Asking the binary which flags it accepts.** `pawncc -?` prints its help,
  and the supported set comes from it. A table per version would not cover the
  forks and the open.mp builds, and passing an unknown flag makes the build
  fail for a reason that has nothing to do with the code.
- **A minimal preset** when `compiler.args` is empty: `-d1`, `-O1`, `-(+`,
  `-;+` and `-w239`, each one only if the local build accepts it. Flags it does
  not accept are removed and reported, instead of vanishing silently.
- **Debugging replaces `-d`.** In a build for debugging, any `-d` from the
  configuration becomes `-d3`, just there: `-d0` through `-d2` do not carry the
  symbols breakpoints and inspection need, and the user's configuration is left
  alone.

Running it is background work — see
[The contract with the extension](rpc.md#slow-work-does-not-hold-the-loop) —
and the output is decoded with the configured encoding before it returns.
