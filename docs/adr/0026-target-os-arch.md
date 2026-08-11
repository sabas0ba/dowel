# ADR-0026: The target's OS and architecture are vocabulary of their own, derived from the triple

**Status**: Accepted

Decides part of [Q1](../99-open-questions.md); the rest of Q1 (predicate
composition, whether the vocabulary is extensible) stays open.

## Context

The provisional vocabulary gave the build host two words (`host.os`,
`host.arch`) and the target one (`cfg.target`, the triple as a free-form
string). Writing a manifest whose implementation differs per operating
system — the ordinary `plat_win.c` / `plat_posix.c` shape — showed that
asymmetry to be a hole with two edges (issue #115).

**A misreading passes silently.** `host.os` reads like "the OS", so the
obvious spelling is `match host.os { windows => file("src/plat_win.c"), … }`.
Building for Windows from a Linux machine, that selects the POSIX file. The
documentation is accurate — `host.os` says *build host* — but when it is the
only word available, the wrong reading compiles. If the POSIX file happens
not to include a header the target lacks, it links, and only the answers
are wrong.

**The correct spelling is expensive.** Reaching the target means enumerating
triples: `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc`, `i686-…`,
`aarch64-…`. There is no way to say "any Windows". `cfg.target` has an
unbounded domain, so the `_` arm is mandatory and a triple nobody thought of
falls quietly into the POSIX side — exactly the failure mode a closed
vocabulary exists to prevent, and the asymmetry Q1 already flagged.

## Decision

`target.os` and `target.arch` join the vocabulary, **derived from the target
triple**. No new input: `--target` already carries it, and reading `windows`
out of `x86_64-pc-windows-gnu` is a matter of spelling.

```toml
sources = [
    file("src/text.c"),
    match target.os {
        windows => file("src/plat_win.c"),
        _       => file("src/plat_posix.c"),
    },
]
```

- **`host.*` stays.** Wanting the build host is real — whether the artifact
  can be run here, whether a tool exists on this machine. The pair was
  missing a half; nothing is being replaced.
- **Both domains are finite**, so `match` exhaustiveness applies and a
  manifest that covers every case needs no `_`. When a new target appears,
  the manifest fails and says so, which is the whole point.
  - `target.os`: `linux`, `macos`, `windows`, `none` (bare metal), `other`
  - `target.arch`: `x86_64`, `x86`, `aarch64`, `arm`, `riscv64`, `other`
- **`other` is what makes finite possible.** `--target` takes a free-form
  string, so some triple always lands outside any list we write
  (`x86_64-unknown-freebsd`). Without a landing place the domain cannot be
  closed, and an unclosed domain brings back the `_` arm.
- The spellings are the vocabulary's, not the triple's: `macos`, not
  `darwin`. Reading the same value under the same name as `host.os` is what
  makes them a pair.
- Derivation scans the triple's components rather than indexing them. A
  triple has three parts or four (`thumbv7em-none-eabihf` has no vendor), so
  position decides nothing. Bare metal is recognised from `none`, an `eabi*`
  component, or `elf`.
- 32-bit ARM collapses into one word. The spellings are many (`armv7`,
  `thumbv7em`, `armebv7r`) and they form one family; splitting them would
  only mean every writer enumerates them again.

## Consequences

- The `plat_win.c` / `plat_posix.c` shape is one arm, and adding a Windows
  triple changes nothing in the manifest.
- `target.os` is what decides the executable's spelling (`bin/app.exe` on
  Windows, issue #112). The derivation has a second reader immediately,
  which is the check on whether it belongs in the vocabulary at all.
- Q2 (ABI label composition) gains two components that are easier to handle
  than a triple string: an ABI label is a property of the target, and the
  target is now spelled in parts.
- `other` will accumulate meaning as targets are tried. A project that needs
  FreeBSD specifically still has `cfg.target` and its exact triple; the
  finite words are for the distinctions that recur.
- The derivation is a table of prefixes, not a database. It has to be
  extended as targets are added — but a target dowel has never seen already
  needs its `[toolchain.<triple>]` and `[runner.<triple>]` written, so this
  is not a new class of work.
