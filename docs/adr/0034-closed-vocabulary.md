# ADR-0034: The configuration vocabulary is closed; a project's own axes are features

**Status**: Accepted

Closes [Q1](../99-open-questions.md), whose other parts were settled by
[ADR-0026](0026-target-os-arch.md) and
[ADR-0032](0032-predicate-composition.md).

## Context

`cfg` / `host` / `target` / `tc` is a closed set of keys with declared
domains. The open question was whether anything should be able to add to
it.

The diagnostic is what made the question urgent. Someone wanting to vary a
build on a sanitizer was told:

```
error[unknown-cfg-key]: unknown configuration key `cfg.sanitizer`
  = note: the vocabulary is provisional; see Q1 in docs/99-open-questions.md
```

Their spelling is refused and they are sent to an open-questions document.
**It never says what to write instead** — though the answer, `[features]`,
has existed all along.

## Decision

**The vocabulary is closed.** Nothing extends it: not a toolchain, not a
package, not a flag. Three things depend on that:

- **Exhaustiveness checking.** `match target.os { … }` is checked against a
  domain known at type-check time. An extensible vocabulary has no domain
  until a toolchain is selected, and that happens after evaluation — so the
  check would have to go, for every key.
- **Findable misspellings.** If unknown keys might be legitimate
  extensions, the only honest response is to accept them, and `cfg.taget`
  becomes a predicate that is quietly false.
- **Computable configuration identity.** The build directory and the stored
  evaluation are keyed on the configuration. Keys that appear depending on
  the toolchain make identity depend on a later answer.

**A project's own axes are features** — a second layer, not a workaround.
The two differ in who knows the axis:

| Layer | Declared by | Domain |
|---|---|---|
| `cfg` / `host` / `target` / `tc` | dowel | fixed, mostly finite |
| `feature.<name>` | the package, in `[features]` | boolean |

Sanitizers, LTO modes, vendored-vs-system: these are the project's axes.
Where the axis is a choice rather than an addition, `[features] exclusive`
says so ([ADR-0021](0021-exclusive-features.md)).

**The diagnostic names the alternative**, filling in the key that was
written:

```
  = note: the vocabulary is closed: it holds what dowel knows about a build (ADR-0034)
  = note: for your own axes, declare `sanitizer` in `[features]` and write `feature.sanitizer`
```

It is a note, not a fix. A fix leaves the file correct; rewriting the key
leaves `unknown-feature` behind, since the feature still has to be
declared. dowel's own property test — *applying a suggestion introduces no
other diagnostic* — rejected the first draft, which offered it as one.

**Which further dimensions belong in the vocabulary is Q2's question.** The
candidates (standard library, CRT kind, sanitizers, LTO, exception model)
are the candidate components of the ABI label. Adding them before knowing
what the label needs would fix its granularity by accident.

## Consequences

- The vocabulary is a commitment, not a placeholder. Adding a key is an
  ADR — the intended weight, since each key is a dimension every manifest
  may branch on.
- Projects needing an axis dowel lacks are not blocked, and the error says
  how. What they lose is exhaustiveness over a value set: `exclusive` says
  two features conflict, not that one is always on.
- `dowel schema dump` remains the live source, so no tool needs this
  document's copy of the table.
- Left open: whether `cfg.opt` should hold more than `debug` / `release`.
  Widening a *domain* is a smaller question than widening the vocabulary,
  and no one has asked.
