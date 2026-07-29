# `dowel.toml` reference

Every table and key the implementation reads, and what happens when it is
missing or wrong. Keys not listed here are currently ignored without a
diagnostic (this includes `edition`, `[policy]`, and `[toolchain] sysroot`
from the design examples — reserved, but not yet acted on).

`dowel.toml` must stay strict TOML: function calls, `match`, postfix `when`,
and configuration references are rejected in value position with
`expression-in-strict-toml`. Anything that needs an expression belongs in
`dowel.build`.

## `[package]`

```toml
[package]
name    = "libfoo"
version = "0.3.1"
```

| Key | Type | Required | Behavior |
|---|---|---|---|
| `name` | string | yes | the package name, used in target references (`<package>:<target>`) and diagnostics. Missing: `missing-field`. Default while erroring: the directory name |
| `version` | string | no | recorded; not yet used for resolution. Default `0.0.0` |

A missing `[package]` table is `missing-table`. A `dowel.toml` whose
directory has no `dowel.build` defines no targets but can still be depended
on for its metadata (in practice every package has both).

## `[toolchain]`

```toml
[toolchain]
c = "clang-19"
```

| Key | Type | Behavior |
|---|---|---|
| `c` | string | the C compiler command. It must be on PATH at plan time — toolchain fetching is not implemented. Missing from PATH: `missing-toolchain` |

Without a `[toolchain]` table, the default compiler is `cc` on PATH. If a
dependency package declares a C toolchain different from the one the build
uses, planning fails with `toolchain-mismatch` — ABI checking assumes a
single pinned toolchain per build.

## `[[dependencies]]`

```toml
[[dependencies]]
name = "libgreet"
path = "../libgreet"

[[dependencies]]
name     = "zlib"
version  = "1.3"        # not implemented: fetching
optional = true
```

Each `[[dependencies]]` entry declares one package this package may use.
Declaring it here creates no edge by itself — a target must also reference
it with `dep("name")` in `dowel.build` ([12-build-reference.md](12-build-reference.md)).

| Key | Type | Behavior |
|---|---|---|
| `name` | string | required. Missing: `missing-field`. The name used by `dep("...")` and, for optional dependencies, by the feature flag that activates them |
| `path` | string | a directory containing another dowel package, relative to this `dowel.toml`. **The only implemented source.** The path must exist and contain a manifest (`missing-manifest` otherwise) |
| `git` | string | recognized but not fetchable yet: `unsupported-dependency`. The design requires a full 40-digit `rev`, never a branch or tag |
| `version` | string | registry dependencies; recognized but not fetchable yet: `unsupported-dependency` |
| `optional` | bool | default `false`. An optional dependency participates only when a feature flag with the same name is enabled. When inactive, neither the edge nor the node exists — the package is not even loaded |
| `when` | inline table | reserved for conditional dependencies (`when = { os = "windows" }`). Parsed, but **not yet honored** — the dependency is treated as unconditional |

An entry with none of `path` / `git` / `version` is `incomplete-dependency`.

## `[features]`

```toml
[features]
default = ["zlib"]
zlib    = []
png     = ["zlib"]
```

Each key declares a feature flag; its value is the list of other features it
enables (transitively closed, cycle-safe). Values must be arrays of strings
(`type-mismatch` otherwise).

- `default` is special: it is included unless `--no-default-features` is
  passed. `default` itself is never a feature name
- The set of valid feature names is exactly the keys of this table. An
  unknown name fails with a diagnostic and a suggestion, whether it comes
  from `--features` on the command line or from a `feature.<name>` reference
  in `dowel.build`
- An enabled feature named like an `optional` dependency activates that
  dependency
- Feature selection is fixed before loading; inside `dowel.build`, features
  are read as `feature.<name>` in `when` conditions
  ([12-build-reference.md](12-build-reference.md))

## What is deliberately absent

- **No expressions** — enforced, see above
- **No target definitions** — targets live in `dowel.build`
- **`dowel.lock`** — not generated yet; resolution currently covers `path`
  dependencies only, which need no locking
