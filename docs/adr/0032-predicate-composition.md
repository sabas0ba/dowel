# ADR-0032: `when` predicates compose with `and` / `or` / `not`; `match` stays the way to choose

**Status**: Accepted

Decides part of [Q1](../99-open-questions.md); the rest of the `cfg`
vocabulary question stays open.

## Context

A `when` clause held exactly one predicate — a boolean key, or a key
compared against a string. Anything else had to be written by repetition:

```toml
# "on Linux or macOS" — two lines that must be kept identical
flags = ["-pthread"] when target.os == "linux",
flags = ["-pthread"] when target.os == "macos",
```

The duplication is not merely verbose. The two lines are one intent, and
nothing ties them together: editing one and not the other is silent, and
the reader has to notice that the right-hand sides match to know it was
meant as a disjunction.

`match` covers the case where the alternatives are exclusive and the value
differs per arm. It does not cover "this same value, under any of these
conditions" — writing that as a `match` means repeating the value in each
arm and adding a `_` arm producing nothing, which is worse than the
repetition it replaces.

Negation had no spelling at all. "Everywhere except Windows" could only be
written by listing the other values, which stops being correct the moment
the vocabulary grows — and `target.os` is a finite vocabulary that is
expected to grow ([ADR-0026](0026-target-os-arch.md)).

## Decision

`when` takes an expression over predicates:

```
predicate := disjunction
disjunction := conjunction ("or" conjunction)*
conjunction := unary ("and" unary)*
unary       := "not" unary | atom
atom        := <key> | <key> "==" <string> | "(" disjunction ")"
```

```toml
flags = ["-pthread"] when target.os == "linux" or target.os == "macos"
flags = ["-fPIC"]    when not target.os == "windows"
deps  = [dep("zlib") when feature.zlib and not feature.minimal]
```

**Words, not symbols.** `and` / `or` / `not` rather than `&&` / `||` /
`!`. The surrounding language is a declarative manifest whose other
operators are words (`when`, `match`, `glob`, `dep`), and `when a && b`
reads as if a different language had been spliced in. The words are
already reserved by being keywords in this position — a bare `and` is not
a namespace reference, so nothing that parsed before parses differently
now.

**Precedence is `not` > `and` > `or`**, the same as every language that
spells these as words, with parentheses to override. Precedence is a place
where being unusual has no upside.

**`match` remains the way to choose between alternatives.** `or` makes one
value reachable under several conditions; it does not make two values
mutually exclusive. The guidance that stacked `when`s are the wrong tool
for switching implementations ([12-build-reference.md](../12-build-reference.md)
section 5) is unchanged, and gains a sharper form: if the predicates you
are writing are the negations of each other, you want `match`.

**Exhaustiveness checking is unaffected.** It belongs to `match`, whose
arms are patterns over one scrutinee's domain, and `when` produces a value
or produces nothing — it has no arms to be exhaustive over. This is why
`or` can be admitted without a decision about exhaustiveness: the two
mechanisms were already separate, and this ADR does not join them.

**Domain checking reaches every leaf.** `when target.os == "windwos"` was
already `unknown-pattern`; it stays so inside `and`, `or`, and `not`. A
composed predicate that is wrong in one leaf is as wrong as a simple one,
and finding out at the leaf is what makes the message point at the typo
rather than at the clause.

## Consequences

- `Pred` becomes a tree. The places that walked it — specialization,
  digesting, serialization, hover — recurse. Only serialization has a
  compatibility dimension: three tags are added to the `when` encoding,
  which older records never used, so records written before this change
  still read correctly and the store's format version does not move.
- `Pred::key()` returned the single key a predicate reads and had no
  callers. It becomes `keys()`, returning all of them, because "the keys
  this predicate depends on" is the question a reader of a composed
  predicate actually has.
- A predicate can now be written that is always false (`when
  target.os == "linux" and target.os == "macos"`). Nothing detects it.
  Detecting it means reasoning about the domains jointly, which is a
  solver, and the same expression is unremarkable when the two keys
  differ. `dowel why` already shows what a value specialized to, which is
  where an always-false predicate becomes visible.
- Not decided here: whether the `cfg` vocabulary is fixed or extensible,
  and which further dimensions belong in it. Those are the rest of Q1.
