# The manifest

How a dowel project is described. This page is the model overview; the
detailed references are:

| Document | Contents |
|---|---|
| [11-toml-reference.md](11-toml-reference.md) | `dowel.toml`: every table and key that is read, and how it is validated |
| [12-build-reference.md](12-build-reference.md) | `dowel.build`: the complete syntax, and every configurable target/runner property |
| [13-semantics.md](13-semantics.md) | how it functions: evaluation, specialization, propagation and merging, planning, execution |

Everything in these references describes the current implementation. Where a
designed feature is not implemented yet, it is marked in place; the summary
list is in [91-implementation-status.md](91-implementation-status.md).

## Two files

A package is a directory containing two manifest files. The split is
deliberate ([ADR-0003](adr/0003-manifest-split.md)).

| File | Format | Written by | Contents |
|---|---|---|---|
| `dowel.toml` | strict TOML | machines (read and write) | package identity, dependencies, toolchain, feature flags |
| `dowel.build` | TOML-style dialect | humans | target definitions, propagated properties, conditionals |
| `dowel.lock` | generated (not implemented) | machines | resolution results, hashes, transitive dependencies |

`dowel.toml` stays strict TOML — expressions are rejected in value position
(diagnostic `expression-in-strict-toml`) — so third-party tools (SBOM
generators, vulnerability scanners, update bots) can read it without
implementing this language.

`dowel.build` is a superset dialect of TOML that adds expressions in value
position only. Its extension is deliberately not `.toml`, so editors do not
apply TOML mode; completion, highlighting, and diagnostics come from
`dowel lsp` instead.

## The model in one pass

1. Both files are parsed into lossless syntax trees (error-tolerant: a
   syntax error never stops analysis)
2. `dowel.build` is **evaluated** into typed values that carry their source
   location and provenance. Conditionals (`match`, `when`) are *not* resolved
   here
3. The values are **specialized** for one configuration (`--config`,
   `--target`, `--features`): `match` picks an arm, `when` keeps or drops
   elements
4. Properties **propagate** along the dependency graph and are **merged**
   per property under a declared merge rule (`union`, `append`,
   `error_on_conflict`, `must_equal`, `replace`)
5. The **plan** stage expands `glob(...)`, resolves paths, and builds the
   action graph (compile / archive / link), which ninja (or the sequential
   executor) runs

Because step 2 is separate from steps 3–5, switching `--config` or
`--target` does not re-evaluate manifests, and every value can answer
`dowel why <target> <property>` with the exact chain of declarations that
produced it.

## Minimal example

```toml
# dowel.toml
[package]
name    = "libfoo"
version = "0.3.1"
```

```
# dowel.build
[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]      # propagates to dependents

[lib.foo.private]
includes = [dir("src")]          # affects only this target
flags    = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}
```

A complete two-package example, with a dependency and a test, lives at
[`examples/hello`](../examples/hello) and is built by the test suite on every
run.
