# ADR-0031: The toolchain is a property of the build, not of a package; the diagnostic says so

**Status**: Accepted

## Context

A library that supports several triples has nowhere to put the compilers
those triples need. `[toolchain.<triple>]` is read from a dependency
package — planning even reports what it says, through
`toolchain-mismatch` — but it does not apply to the build. The consumer
must write the same table again.

The observed output made this worse rather than merely inconvenient
(issue #125):

```console
$ dowel -C app build --target=aarch64-unknown-linux-gnu
error[missing-toolchain]: no toolchain is declared for target `aarch64-unknown-linux-gnu`
  = note: declare one, for example `[toolchain.aarch64-unknown-linux-gnu]` with `c = "..."` in dowel.toml
warning[toolchain-mismatch]: package `mylib` asks for `c = "aarch64-linux-gnu-gcc"` but the build uses `cc`
```

dowel says the declaration is missing and, two lines later, reads out its
contents. The advice offers a generic example when the specific answer is
in hand. Whatever the design position is, this output does not state it.

The cost of copying is real. A tree with a core algorithm built for four
triples and three consumers, each supporting a different subset, ends up
with the table written **supported triples × consumers** times. Worse, a
consumer that changes `aarch64-linux-gnu-gcc` to `aarch64-linux-gnu-gcc-12`
and forgets the library gets `toolchain-mismatch` — a warning — and a build
that succeeds.

## Decision

**A dependency's `[toolchain]` does not apply to the build, and this is
deliberate.** The toolchain is a property of the build, not of a package.

The reason is that the tool *name* is not knowledge the library has:

- The same aarch64 target is `aarch64-linux-gnu-gcc` on Debian,
  `aarch64-none-linux-gnu-gcc` in an Arm release, and something entirely
  different inside a Yocto SDK — the name comes from what is installed on
  the machine doing the build.
- A library that ships a name is right only where its author's machine is
  reproduced. Everywhere else the name must be overridden, so the
  declaration becomes something to work around rather than something to
  use.
- ABI label verification assumes one pinned toolchain per build
  ([ADR-0012](0012-store-contents.md), and the `toolchain-mismatch`
  warning that already exists). Letting dependencies supply toolchains
  means a build can be handed several, and the conflict resolution — first
  wins? deepest wins? — would be a rule with no principled answer.

Cargo draws the line in the same place: the toolchain lives in the
environment (`rust-toolchain.toml`, `rustup`), not in a package's
manifest.

What a library *can* express is which triples it supports. That is
knowledge it genuinely has, and `targets` — on the package
([issue #71](https://github.com/sabas0ba/dowel/issues/71)) and now on the
target ([issue #126](https://github.com/sabas0ba/dowel/issues/126)) —
carries it.

**The diagnostic states the position instead of leaving it to be
inferred.** When a dependency declares a toolchain for the requested
triple, `missing-toolchain` reads out what it says and why it does not
apply:

```
error[missing-toolchain]: no toolchain is declared for target `aarch64-unknown-linux-gnu`
  = note: building with the host toolchain would produce artifacts for the wrong architecture under this target's name
  = note: dependency `mylib` declares one for this triple (c = "aarch64-linux-gnu-gcc", ar = "aarch64-linux-gnu-ar")
  = note: a dependency's toolchain does not apply to this build: it is a property of the build, not of the package (ADR-0031). declare it here to use it
```

The generic "declare one, for example …" advice stays for the case where
nothing is in hand. When something is, the specific answer replaces it —
the reader can copy the line rather than go looking for it.

## Consequences

- The copying cost stands. A consumer supporting a triple writes that
  triple's table. What this ADR removes is the time spent discovering
  *why*, which was the larger part of it.
- The drift the issue describes — a consumer updating a compiler and the
  library keeping the old name — is not fixed here and is not made worse.
  `toolchain-mismatch` stays a warning, because the build genuinely can
  proceed and the consumer's declaration is the one that governs.
- If the copying later proves worse than the ambiguity, the thing to add
  is not "inherit from a dependency" but a way for a build to name a
  toolchain **file** it shares with others — the environment's own unit,
  which is where Cargo ended up too. That would be a new decision, not a
  reversal of this one.
- `docs/63-guides.md` states the rule where a reader meets it, rather than
  leaving it to be deduced from a warning's wording.
