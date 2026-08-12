# ADR-0042: An ABI label is a set of components, so granularity is chosen per declaration instead of once for everyone

**Status**: Accepted

Closes Q2 of [99-open-questions.md](../99-open-questions.md). Extends
[ADR-0019](0019-c-abi-label.md), which stands unchanged.

## Context

Q2 asked what an ABI label should be made of, and framed it as a dilemma
inherited from Conan's `package_id`:

> - **Too coarse** → verification becomes meaningless (the vcpkg triplet limit)
> - **Too fine** → cache hit rates collapse
>
> Candidate components: toolchain ID, C++ standard version, standard library
> implementation, `_GLIBCXX_USE_CXX11_ABI`, MSVC runtime kind, sanitizers,
> LTO, exception model, floating-point model.

The dilemma is real, but it is a consequence of a choice nobody had noticed
making: **the label is one opaque token**. If a label is a single word, then
every declaration in the world has to agree on how much that word encodes,
so the granularity has to be picked once and it is wrong for someone.

What the label is used for today is comparison. Labels reaching a target are
merged with `must_equal`, and a difference is `abi-mismatch` before linking.
That comparison does not need a single granularity. It needs to know when two
declarations *contradict* each other.

The repository's own examples show what an opaque token costs. Three
incompatible spellings are in use: `gnu11` / `gnu++17` (a language dialect,
in ADR-0019), `x86_64-linux-gnu` / `x86_64-linux-musl` (a triple, in the
tests), and `c` (a boundary). All three are defensible, none can be compared
with another, and nothing tells an author which one to write. A label nobody
knows how to write fails on spelling and passes on substance.

## Decision

**An ABI label may be a set of named components, and comparison is per
component.**

```toml
[lib.hashx.public]
abi = { libc = "musl", cxx_stdlib = "libc++" }
```

- Two labels conflict when they name **the same** component with different
  values. That is `abi-mismatch`, and it names the component — printing two
  whole labels leaves the reader to diff them.
- A component only one side names is **not a constraint**. The side that
  knows less says less, and says it without blocking the side that knows
  more.
- The merged label is the **union** of components. A constraint that is
  dropped in the middle of a graph is a constraint the far end never sees.

This is the answer to the granularity dilemma: **stop choosing.** A coarse
declaration constrains little, a fine one constrains more, and they compose
without either having to know what the other decided.

**The component vocabulary is closed** and grows one component per ADR, with
a domain — [ADR-0034](0034-closed-vocabulary.md)'s procedure, for
[ADR-0034](0034-closed-vocabulary.md)'s reason. An open set would make a
misspelled component a component nobody else names, which is exactly the
shape of "not a constraint": the declaration would be accepted, compared
against nothing, and mean nothing. `unknown-abi-component` refuses it with a
suggestion.

It starts with two:

| Component | Meaning | Domain |
|---|---|---|
| `libc` | the C runtime this surface requires | `gnu` / `musl` / `msvc` / `apple` / `none` / `other` |
| `cxx_stdlib` | the C++ standard library this surface requires | `libstdc++` / `libc++` / `msvc-stl` |

`libc` is the one dowel can derive, so it comes with a check the label
system did not have. `target.os` does not answer this axis — `linux-gnu` and
`linux-musl` are the same OS and two runtimes that do not link — so
`target.env` joins the configuration vocabulary, read off the triple the way
[ADR-0026](0026-target-os-arch.md) reads `os` and `arch`.

`cxx_stdlib` cannot be derived. It is included because mixing `libstdc++`
and `libc++` is the best-known ABI break in C++ and because a declaration is
still checkable — against the other declaration. That is what every label
here is: ADR-0019 already recorded that no label is verified against the code
it describes.

**A label written as one word keeps its current meaning.** It is compared
whole, and `c` is still exempt from every comparison (ADR-0019). Nothing
that exists today changes. A word and a component set cannot be compared —
a word cannot be taken apart — so meeting one of each is `abi-mismatch` with
a note saying why.

**A declared `libc` is also checked against the build.** Comparing labels
only asks who requires what; it never asks what this build *is*. A surface
requiring `libc = "musl"` built for a gnu triple has its requirement
unsatisfied, the link succeeds, and the failure is at run time. Only derived
components can be checked this way, and `libc` is the only one.

## Consequences

- The "too fine" horn of the dilemma is not just avoided, it is currently
  unloaded. It is about a **binary cache** — a finer identity means fewer
  hits — and dowel has none; its build directories are already keyed by
  configuration. Until there is one, a finer label costs nothing, so the bias
  should be toward saying more. Revisit when binaries are shared between
  builds.
- Labels are still hand-written. Computing one is Phase 6, and this decision
  shapes what it must produce: a set of components, so a computed label can
  be compared with a declared one component by component instead of having to
  match a string somebody typed.
- Within one build the toolchain and configuration are uniform
  ([ADR-0031](0031-toolchain-is-the-builds.md)), so components describing
  *what a target was built with* would be identical everywhere and their
  comparison vacuous. The components here describe **what a surface
  requires**, which is why they are worth comparing at all. That distinction
  is what keeps the label from turning back into a copy of the configuration
  identity.
- The vocabulary is deliberately short. The other candidates Q2 listed —
  sanitizers, LTO, exception model, `_GLIBCXX_USE_CXX11_ABI`, MSVC runtime
  kind — are real, and each needs its own evidence and its own domain. Adding
  them now would be guessing at both.
- `dowel why` shows a label's components with a provenance chain each, so a
  mismatch can be traced to the package that introduced the requirement.
