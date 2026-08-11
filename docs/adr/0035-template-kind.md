# ADR-0035: A template shares manifest text, not a graph edge; it expands into the block it came from

**Status**: Accepted

## Context

Targets that belong together repeat their settings. Three tools in one
package, each wanting the same warning flags, the same include directory,
and the same dependency, write those three lines three times. Nothing ties
the copies together, so they drift.

A library with no sources already covers part of this. Declaring
`[lib.settings]` with a `public` block and depending on it does propagate
includes, defines, and flags — it works today, and it produces an empty
archive as a side effect.

What it cannot do is share a **private** setting. `public` is the only
block that propagates, and a setting placed there reaches not just the
targets that wanted it but everything downstream of them. Warning flags
are the ordinary case: `-Wall -Wextra -Werror` is how *this* code is
compiled, and pushing it onto every consumer of a library is a different
statement, usually a wrong one.

So the gap is not "no way to share settings". It is that the only way to
share them is to publish them.

## Decision

**`[template.<name>]` holds `public` and `private` blocks, and a target
takes one with `use`.**

```toml
[template.tool]

[template.tool.public]
includes = [dir("include")]

[template.tool.private]
flags = ["-Wall", "-Wextra", "-Werror"]
deps  = [target("core")]

[bin.probe]
sources = [file("src/probe.c")]
use     = [template("tool")]

[bin.trace]
sources = [file("src/trace.c")]
use     = [template("tool")]
```

**A template expands into the block it came from.** Its `private` becomes
the target's `private`; its `public` becomes the target's `public`. That
is the whole difference from the library trick, and the reason the kind
exists: sharing a setting and publishing it become separate acts again.

**Expansion happens before merging, using the ordinary merge rules.** A
template contributes as if its lines had been written in the target, ahead
of the target's own. `append` keeps that order, `replace` lets the target
win, and `error_on_conflict` still fails on a genuine conflict — a
template is not a special case in the merge algebra, which is what keeps
`dowel why` able to explain the result.

**A template is not a target.** It produces no artifact, appears in no
graph, and cannot be named by `deps` or on the command line. Nothing about
the build changes because a template exists; only what a target's blocks
contain.

**A template holds settings only.** No `sources`, no `targets`, no
`linkage`, no `exports` — the root-block properties say *what a target
is*, and sharing those makes it unclear what is being built. Writing one
is `unknown-property`, pointing at the block that does accept it.

**Templates do not use templates.** One level, the same as
[ADR-0033](0033-shared-toolchain-file.md): no cycle to detect, no order to
explain, and the answer to "where did this flag come from" is one hop.

**`use` is a root property**, not something inside `public` / `private` —
it says what this target is assembled from, and putting it inside a block
would suggest a template could be taken publicly, which is not a thing:
the template already decides which of its own settings are public.

## Consequences

- The library-with-no-sources idiom keeps working and is no longer the
  only option. It remains the right shape when the shared thing genuinely
  is a public interface with a real artifact behind it; a template is the
  right shape when it is text.
- `deps` inside a template is allowed and expands like anything else. The
  graph edge belongs to the target that used the template, which is
  already how a `deps` line behaves wherever it is written.
- A template that no target uses is inert. Nothing warns, because a
  package that declares one for a target it has not written yet is not
  making a mistake.
- Provenance keeps working through expansion — `dowel why` names the
  template's line, since expansion carries each value's site with it. This
  is the property that made expansion (rather than a separate lookup
  layer) the right implementation.
- Not addressed: parameters. A template takes no arguments, so "the same
  settings but with a different `LOG_LEVEL`" still needs two templates or
  a `defines` line at the use site. Parameters would make templates a
  small language, and the cases that need one are better served by a
  configuration key or a feature.
