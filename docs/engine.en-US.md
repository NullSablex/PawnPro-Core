# The engine

Pawn analysis is a library (`crates/engine`) that the core hosts on a thread
and serves over LSP on the local socket. It does not open configuration files
nor look for includes: it receives everything resolved over a typed channel —
see [Architecture](architecture.md).

What it offers the editor: diagnostics, completion, hover, signature help, go
to definition, references, the reference count above functions, renaming, quick
fixes, semantic tokens, and formatting (document and selection).

## Compilation unit

The question that organises everything is "what is compiled together with this
file?".

A **program** is a `.pwn` that no other file in the project includes; an
included `.pwn` is a fragment, not a program. A file's unit is the program that
includes it, along with everything that program includes — including the
sibling `.inc` the editor never opened.

Two guarantees the editor needs follow from that:

- a gamemode symbol does not show up while editing a filterscript, even when
  both have functions with the same name;
- a `stock` used only by the `.pwn` that includes it is not reported as unused.

## The text that counts is the editor's

The analysis reads the open document, not the file on disk: a new `stock` in an
unsaved include already counts in the other files of the unit.

Every diagnostics publication carries the **document version** that produced
it. If an edit arrived while the analysis was running, the result is discarded:
warnings about text that no longer exists never show up.

## Cache

Each file's identifiers are cached, validated by modification time and, for
open files, by the editor's text. The reference count adds up straight from the
cache, without rereading files, and the declaration lookup reads its symbols.

In a gamemode with 84 includes, each edit went from around 1 s to 0.2 s.

## Diagnostics

There are 19 codes, `PP0001` through `PP0019`, and 13 of them have an automatic
fix. The full list, with severity and what each one covers, is in the
[extension's documentation](https://pawnpro.nullsablex.com/features/#diagnosticos).

Two decisions hold for all of them:

- **Severity means something.** An error is what the compiler would refuse; a
  warning is what compiles and is probably wrong; a hint is style. The naming
  assistant (`PP0018`) is always a hint, and ships disabled.
- **The message is translated, the code is not.** Each text is a `MsgKey` with
  one table per language (`messages/langs/`), and the `PP####` code is what you
  search the documentation for, the same in every language. The same holds for
  quick-fix titles.

## Formatting

Formatting is driven by the structure of the code, not by regular expressions
over text. Two requirements govern it, and both are tested:

1. **Do no damage.** End-of-line comments, strings with spaces, characters,
   macros with a `\` continuation and indented directives stay as they were.
2. **Be stable.** Formatting again changes nothing.

## Probe against a real project

Synthetic tests do not cover a real gamemode. The probe
(`PAWNPRO_PROBE_PROJECT=/path/to/project`) runs every file of the project
through the analysis, the semantic tokens and the formatter, and demands that
nothing panics, that formatting alters neither code nor comments, and that
formatting twice yields the same result. It also measures where the analysis
time goes.

```bash
PAWNPRO_PROBE_PROJECT=~/my-gamemode \
  cargo test --release -p pawnpro-engine probe -- --nocapture
```
