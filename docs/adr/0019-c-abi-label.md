# ADR-0019: `abi = "c"` names a boundary, not a language

**Status**: Accepted

## Context

`abi` is merged with `must_equal`: the labels reaching a target must all be
the same string, and a difference is `abi-mismatch` before linking. The
purpose is stated in [13-semantics.md](../13-semantics.md) — turn a
would-be runtime ODR breakage into a build failure.

A C library and a C++ consumer, both written honestly, produce **different**
labels:

```toml
# the library
[lib.hashx.public]
abi = "gnu11"

# the consumer, in its own language
[bin.hashcxx.private]
abi = "gnu++17"
```

The build is refused. It is not wrong about the strings; it is wrong about
what it is checking. An ODR violation is one entity in C++ having more than
one definition, and it does not arise across an `extern "C"` boundary: a C
function has no overloading, no templates, no inline instantiation, and no
name mangling. Both sides may be built by different languages and still
agree on everything that call actually depends on.

The workaround available today is worse than the failure. The consumer
copies the library's label:

```toml
abi = "gnu11"       # not this program's ABI; the library's
```

Then it builds and runs. But the label has stopped describing an ABI and
started naming "the set of things that use hashx". A design that puts ABI
checking at its center cannot afford labels that mean that (issue #78).

For the library author it is worse still. A library is written without
knowing its consumers, so fixing one label forces it on **every** consumer.
The asymmetry — the author does not know the language, the consumer wants to
state its own — is what makes this show up when distributing a library and
not when building a single tree, where the same person writes both sides and
naturally makes the labels agree.

## Decision

An ABI label may name a **boundary** instead of a language. One such label
exists: `c`.

```toml
[lib.hashx.public]
abi = "c"          # this surface is the C ABI; the consumer's language is its own business
```

A `c` label is compatible with every label. In a `must_equal` merge over
`abi`:

- `c` values do not participate in the comparison
- the merged value is the first non-`c` label, so a real constraint is never
  hidden by one
- when every label is `c`, the result is `c`

Everything else is unchanged. Two labels that both name a language must
still be equal, and the diagnostic is the same `abi-mismatch`.

The exemption belongs to the ABI label vocabulary, not to `must_equal`.
`must_equal` on any other property still means equality; `c` is a
distinguished value of `Type::AbiLabel`, which is the type the rule is being
applied to.

## Consequences

- A C library can be distributed without deciding its consumers' language.
  This is the case the label system was failing at, and it is the majority
  of why a C library is distributed at all
- The label goes back to describing what it says it describes. `gnu11` on a
  C++ target means that target really is `gnu11`, not that it uses a
  particular library
- `c` never weakens a check it was not asked to weaken. It declines to add
  a constraint; it does not remove one. A tree whose dependency declares
  `gnu11` still propagates `gnu11` through a `c` surface, because that
  constraint is real and reaching further
- Declaring `abi = "c"` is a claim about the headers, and nothing verifies
  it. That is true of every label today — they are hand-written
  ([91-implementation-status.md](../91-implementation-status.md)) — but the
  claim `c` makes is narrower and more checkable than a language label, and
  is the natural thing for an IDL or a header scan to confirm later
- When labels are computed rather than written (Phase 6, and Q2 on label
  composition), "this surface is the C ABI" survives as a fact the
  computation has to produce. A computed `gnu11` for a C library would
  reintroduce exactly this failure, so the distinction has to exist below
  the label, not only in what an author types
- The remaining `abi-mismatch` says nothing about `c`. Pointing a mismatched
  pair at this label is a separate, easy change; it is deliberately not part
  of this decision, which is about what the label system can express
