# ADR-0033: A build can name a toolchain file it shares; the unit of override is one tool

**Status**: Accepted

Completes the part of [issue #125](https://github.com/sabas0ba/dowel/issues/125)
that [ADR-0031](0031-toolchain-is-the-builds.md) deliberately left standing.

## Context

ADR-0031 settled *why* a dependency's `[toolchain]` does not apply to a
build: the tool's name comes from the machine doing the build, not from
the library. It said so in the diagnostic, which removed the time spent
discovering the rule — and it explicitly left the copying cost in place:

> If the copying later proves worse than the ambiguity, the thing to add
> is not "inherit from a dependency" but a way for a build to name a
> toolchain **file** it shares with others — the environment's own unit,
> which is where Cargo ended up too.

The cost is real and grows multiplicatively. A tree with one algorithm
built for four triples and three consumers, each supporting a different
subset, writes the same `[toolchain.<triple>]` table once per
(consumer, triple) pair. Adding a triple is not one line but one line per
consumer, and a consumer that updates a compiler and leaves the others
behind gets `toolchain-mismatch` — a warning, and a build that succeeds.

## Decision

**`[package] toolchains` names a file of toolchain declarations.**

```toml
# cli/dowel.toml
[package]
name       = "cli"
version    = "0.1.0"
toolchains = "../toolchains.toml"
```

```toml
# toolchains.toml — one table per triple, the same spelling as in dowel.toml
[toolchain.aarch64-unknown-linux-gnu]
c  = "aarch64-linux-gnu-gcc"
ar = "aarch64-linux-gnu-ar"

[toolchain.thumbv7em-none-eabihf]
c       = "arm-none-eabi-gcc"
ar      = "arm-none-eabi-ar"
objcopy = "arm-none-eabi-objcopy"
```

**One file holds many triples.** The thing being shared is the mapping
from triple to tools, and that mapping is what the tree has one of. A file
per triple would trade table copies for path copies.

**The spelling inside the file is the spelling inside `dowel.toml`.** Not
a new schema — the same `[toolchain]` / `[toolchain.<triple>]` tables,
read by the same code. A file that holds anything else is refused
(`unknown-table`) rather than silently ignored, because "I wrote it in the
wrong file" is otherwise indistinguishable from "it had no effect".

**The unit of override is one tool, and the local declaration wins.**

```toml
[package]
toolchains = "../toolchains.toml"

[toolchain.thumbv7em-none-eabihf]
c = "/opt/gcc-13/bin/arm-none-eabi-gcc"   # this machine only; `ar` and
                                          # `objcopy` still come from the file
```

Per-triple override would mean rewriting the whole table to change one
tool, which is the cost this ADR exists to remove. Per-tool keeps the
shared file as the base and the local declaration as the exception, which
is the shape the situation actually has: the table is right, one machine
differs.

**Reading is not transitive.** A toolchain file cannot name another one.
With one level there is no cycle to detect, no order to explain, and the
answer to "where did this compiler come from" is always one of two files.
A tree that outgrows this wants a generated file, which it can already
write.

**A dependency's `toolchains` is not read**, exactly as its `[toolchain]`
is not. Nothing about ADR-0031's reasoning changes: this ADR gives the
consumer a place to put the table once, not a way to inherit one.

**The file is read through the query engine**, like every other input, so
that it is *recorded* as one. A one-shot `dowel build` re-reads everything
anyway and would survive a shortcut here; a session that stays alive would
not. The language server holds a `Session` and reloads it, and a toolchain
file read outside the engine is a file the reload does not know it read —
the editor would keep answering with the previous compiler until something
else happened to touch `dowel.toml`. Recording it is also what lets the
store key the evaluation on the file's own contents rather than on the
manifest that names it.

## Consequences

- The copying cost that ADR-0031 accepted is gone for trees that want it,
  without weakening "one pinned toolchain per build": the file supplies
  declarations to *one* package's build, the same as writing them inline.
  `toolchain-mismatch` still compares against what the build actually
  uses.
- The drift case in issue #125 — a consumer updating a compiler and the
  library keeping the old name — is now avoidable by construction, since
  there is one table to update rather than one per consumer. It is not
  *prevented*: a tree can still write tables inline in every package, and
  nothing requires the shared file.
- Where the file lives is the tree's business. dowel resolves the path
  relative to the `dowel.toml` that names it and does not search for a
  conventional name, because a convention would have to be either
  repo-root-relative (dowel has no notion of a repo root) or ancestor-
  searched (which makes a build's inputs depend on directories above it).
- Not addressed: fetching or pinning toolchains, which is Phase 5. This
  decides where a declaration lives, not where the compiler comes from.
