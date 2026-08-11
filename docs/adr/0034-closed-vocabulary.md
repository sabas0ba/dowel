# ADR-0034: The configuration vocabulary is closed; a project's own axes are features

**Status**: Accepted

Closes [Q1](../99-open-questions.md). The target's own words were settled
by [ADR-0026](0026-target-os-arch.md) and predicate composition by
[ADR-0032](0032-predicate-composition.md); this decides the last part —
whether the vocabulary is fixed or extensible — and hands the remaining
"which further dimensions" question to Q2, where it belongs.

## Context

`cfg` / `host` / `target` / `tc` are a closed set of keys with declared
domains. The implementation adopted the draft vocabulary "provisionally",
and the diagnostic said so:

```
error[unknown-cfg-key]: unknown configuration key `cfg.sanitizer`
  = note: `cfg` accepts: cfg.opt, cfg.target
  = note: the vocabulary is provisional; see Q1 in docs/99-open-questions.md
```

The open question was whether a toolchain — or a project — should be able
to add keys to it.

The message above is what makes the question urgent rather than academic.
Someone who wants to vary a build on a sanitizer reads it, learns that
their spelling is not allowed, and is told to consult an open-questions
document. **It never says what to write instead**, even though dowel has
had the answer since features existed.

## Decision

**The vocabulary is closed. Nothing extends it — not a toolchain, not a
package, not a command-line flag.**

What holds it closed is what it buys:

- **Exhaustiveness checking.** `match target.os { … }` is checked against
  a finite domain known at type-check time
  ([ADR-0026](0026-target-os-arch.md)). A vocabulary that a toolchain can
  extend has no domain until a toolchain is selected, and selection
  happens after evaluation — so the check would have to be dropped, for
  every key, to admit keys nobody has yet asked for.
- **Misspellings stay findable.** `cfg.taget` is an error with a
  suggestion. If unknown keys might be legitimate extensions, the only
  honest response to an unknown key is to accept it, and every typo
  becomes a predicate that is quietly false.
- **Configuration identity stays computable.** The build directory and the
  stored evaluation are keyed on the configuration. Keys that appear
  depending on which toolchain was picked make the identity depend on the
  answer to a question asked later.

**A project's own axes are features, and that is a second layer, not a
workaround.** The two layers differ in who knows the axis:

| Layer | Who declares it | Domain | Checked how |
|---|---|---|---|
| `cfg` / `host` / `target` / `tc` | dowel | fixed, mostly finite | vocabulary + exhaustiveness |
| `feature.<name>` | the package, in `[features]` | boolean | the name must be declared |

A sanitizer, an LTO mode, a vendored-vs-system choice — these are the
project's axes, and `[features]` is where a project declares its axes.
Where the axis is a choice rather than an addition, `[features]
exclusive` states that ([ADR-0021](0021-exclusive-features.md)), which is
how an enumerated axis is spelled without features stopping being
additive.

**The diagnostic carries the way out.** "Provisional; see Q1" is replaced by
the rule and the alternative:

```
error[unknown-cfg-key]: unknown configuration key `cfg.sanitizer`
  = note: `cfg` accepts: cfg.opt, cfg.target
  = note: the configuration vocabulary is closed: it holds what dowel itself knows about a build (ADR-0034)
  = note: to vary a build on something dowel does not know, declare it in `[features]` of dowel.toml and write `feature.sanitizer`
```

When no vocabulary key is close enough to be a plausible typo, the note
fills the name in: *declare `sanitizer` in `[features]`, then write
`feature.sanitizer`*.

It is a note and not a **fix**, deliberately. A fix is something that,
applied, leaves the file correct; rewriting the key to `feature.sanitizer`
leaves `unknown-feature` behind, because the feature still has to be
declared. dowel's own property test — "applying a suggestion introduces no
other diagnostic" — rejects the first draft of this change, which offered
the rewrite as a fix. The message cannot know whether the feature exists:
`unknown-cfg-key` is raised while evaluating `dowel.build`, which does not
read `[features]`.

`unknown-namespace` carries the same note, since `platform.os` is the same
mistake made one level up.

**Which further dimensions belong in the vocabulary is Q2's question, not
Q1's.** The candidates — C++ standard library, CRT kind,
`_GLIBCXX_USE_CXX11_ABI`, sanitizers, LTO, exception model — are exactly
the candidate components of the ABI label. Adding them here, before
knowing which ones the label needs and at what granularity, would fix the
granularity by accident. Q1 asked whether the vocabulary can grow at all;
the answer is that it grows only by a decision recorded here, one key at a
time, with a domain.

## Consequences

- The vocabulary is now a commitment rather than a placeholder. Adding a
  key is an ADR — which is the intended weight, since each key is a
  dimension every manifest may branch on and a candidate component of the
  ABI label.
- Projects needing an axis dowel does not have are not blocked, and the
  error now says how. What they lose relative to a real key is
  exhaustiveness checking over a value set: `exclusive` states that two
  features conflict, not that one of them is always on.
- `dowel schema dump` continues to be the live source for the vocabulary,
  so a tool never needs this document's copy of the table.
- Not decided: whether `cfg.opt` should hold more than `debug` /
  `release`. It is a finite domain that a project might reasonably want to
  extend (`relwithdebinfo`), and extending a *domain* is a smaller
  question than extending the vocabulary — but it is still a decision
  about what dowel knows, and no one has asked for it yet.
