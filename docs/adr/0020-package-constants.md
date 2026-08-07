# ADR-0020: `pkg.name` / `pkg.version` are package constants, readable in value position

**Status**: Accepted

## Context

A library's version exists twice: once in `dowel.toml` as `[package]
version`, and once in the header it publishes, because consumers read it
with `#if` and the library answers it at run time.

```toml
[package]
version = "0.4.0"
```

```c
#define HASHX_VERSION "0.4.0"      /* copied by hand */
```

Nothing connects them. Moving the manifest's version alone changes nothing
and produces no diagnostic; the artifact keeps reporting the header's value,
so a consumer reading the manifest and a consumer calling the function get
different answers (issue #80). The same fact is recorded in two places and
nobody compares them, which is exactly what
[00-overview.md](../00-overview.md) section 2 sets out to avoid.

The obvious way to close it is to let a define read the manifest:

```toml
defines = { HASHX_VERSION = pkg.version }
```

Two things stood in the way. There was no `pkg` namespace, and a namespace
reference was refused in value position (`unexpected-reference`) — the
configuration namespaces are only legal as a `match` scrutinee or a `when`
predicate. String concatenation is deliberately absent
([ADR-0004](0004-syntax.md)), so `"0." + minor` is not a way around it
either.

This shows up when distributing a library and not when building one tree.
`[package] version` is already used for *other people's* versions — a
pkg-config minimum, a `dowel.lock` entry — where it is always an input to
resolution. "My own version, in my own artifact" only arises on the side
that publishes.

## Decision

A new namespace, `pkg`, holds constants of the package whose manifest
declares them:

| Reference | Type | Value |
|---|---|---|
| `pkg.name` | `Str` | `[package] name` |
| `pkg.version` | `Str` | `[package] version` |

It is readable **in value position**, which no other namespace is:

```toml
[lib.hashx.private]
defines = { HASHX_VERSION = pkg.version, HASHX_NAME = pkg.name }
```

And it is refused in a `match` scrutinee or a `when` predicate, which every
other namespace requires. A package's own version is not an axis a build
varies along; accepting `match pkg.version` would say it is.

The reference resolves at **specialization**, not at evaluation. Evaluation
is per file and its result is stored keyed by that file's content, and a
`dowel.build` file does not change when `dowel.toml`'s version does.
Substituting during evaluation would put a stale version in the store and
reintroduce the bug from the other side. Specialization already runs per
package (`Config::for_package`, [ADR-0017](0017-feature-forwarding.md)),
which is where the package a value belongs to is known.

Only a whole value may be read. There is still no concatenation, so a
composite string like `"hashx/0.4.0"` is not expressible — that is
[ADR-0004](0004-syntax.md) unchanged, not an oversight.

## Consequences

- The version is recorded once. Moving `[package] version` rebuilds the
  sources that read it and changes the artifact, so the manifest and the
  binary cannot disagree
- Editing `dowel.toml` now invalidates compiles in that package. That is
  the point — the value is a real input — and it costs a rebuild of the
  files that name it, not of the tree
- `pkg` sits outside the `cfg` vocabulary (`cfg` / `host` / `feature` /
  `tc`) and outside Q1. Those keys describe the configuration a build runs
  in and have domains, exhaustiveness rules, and a place in the
  configuration identity. A package constant has none of that, and folding
  it in would have made the ABI-label question harder for no gain
- A reference in value position is a shape the language did not have.
  It is confined to `pkg`: the configuration namespaces still belong to
  `match` and `when`, where the deferred resolution has a reason
  (switching `--config` must not re-evaluate manifests). Should another
  constant namespace ever be wanted, this is the pattern it follows
- Generating a header (`configure_file`) stays absent. Adding one vocabulary
  entry is smaller than adding a generation mechanism, and the reference
  list in [11-toml-reference.md](../11-toml-reference.md) is unchanged
