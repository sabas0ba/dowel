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

## `[toolchain]` and `[toolchain.<triple>]`

```toml
[toolchain]                              # applies to host builds
c   = "clang-19"
cxx = "clang++-19"

[toolchain.aarch64-unknown-linux-gnu]    # applies to --target=aarch64-unknown-linux-gnu
c   = "aarch64-linux-gnu-gcc"
cxx = "aarch64-linux-gnu-g++"
```

| Key | Type | Behavior |
|---|---|---|
| `c` | string | the C compiler command, default `cc` for host builds. It must be on PATH at plan time (a value containing a path separator is probed as a path) — toolchain fetching is not implemented. Missing from PATH: `missing-toolchain`. Required in `[toolchain.<triple>]`: missing there is `missing-field` |
| `cxx` | string | the C++ compiler command, default `c++` for host builds. Required — and probed — only when the build contains C++ sources. Missing from PATH: `missing-toolchain` |
| `ar` | string | the archiver command, default `ar`. Required — and probed — only when the build produces a static library. Cross builds should declare it alongside `c` / `cxx` so archives are not created by the host's tool. Missing from PATH: `missing-toolchain` |

Any other key is `unknown-property`, with a suggestion — a misspelled tool
would otherwise silently fall back to its default, which for a cross
archiver means the host's `ar` quietly builds the archives.

The toolchain is selected by the target triple, the same way
`[runner.<triple>]` is (issue #42). The plain `[toolchain]` table is the
declaration for host builds; it never applies to another triple. Passing
`--target=<triple>` for a triple with no `[toolchain.<triple>]` declaration
is refused before building with `missing-toolchain` — building with the
host compiler would silently place host artifacts under that triple's name,
and the mistake would only surface later (a runner's
`Invalid ELF image for this architecture`, or a debugger showing the wrong
architecture). Likewise, a cross build whose sources contain C++ requires
`cxx` in the triple's table; falling back to the host `c++` is refused.

If a dependency package declares a toolchain different from the one the
build uses, planning warns with `toolchain-mismatch` — ABI checking assumes
a single pinned toolchain per build. Only declarations that apply to the
current target triple participate in this comparison.

## `[[dependencies]]`

```toml
[[dependencies]]
name = "libgreet"
path = "../libgreet"

[[dependencies]]
name = "bar"
git  = "https://github.com/example/bar"
rev  = "9f3c0a1e2b7d4856c0f1a93e5d2b8c4770ae6135"

[[dependencies]]
name     = "zlib"
version  = "1.3"        # resolved via the system pkg-config
optional = true
```

Each `[[dependencies]]` entry declares one package this package may use.
Declaring it here creates no edge by itself — a target must also reference
it with `dep("name")` in `dowel.build` ([12-build-reference.md](12-build-reference.md)).

| Key | Type | Behavior |
|---|---|---|
| `name` | string | required. Missing: `missing-field`. The name used by `dep("...")` and, for optional dependencies, by the feature flag that activates them |
| `path` | string | a directory containing another dowel package, relative to this `dowel.toml`. The path must exist and contain a manifest (`missing-manifest` otherwise) |
| `git` | string | a git URL (anything `git` itself accepts, including local paths). Requires `rev`. Fetched once into `.dowel/deps/<name>-<rev12>/`; later runs never touch the network. A failing fetch is `unfetchable-dependency` |
| `rev` | string | required with `git`: a **full 40-digit commit sha**. Branches, tags, and abbreviated shas are refused with `unpinned-dependency` — a name-only reference does not count as pinned. Because the rev pins the content exactly, git dependencies need no lock file |
| `version` | string | a system package, resolved through **pkg-config** ([ADR-0015](adr/0015-version-deps-pkgconfig.md)). `name` is the pkg-config module name; the version is a **minimum** (`--atleast-version`). `--cflags` / `--libs` become the dependency's public flags and link flags. Absent module, too-low version, or missing pkg-config: `unsatisfied-dependency`. Resolutions are recorded in `dowel.lock` (below) |
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

## `dowel.lock`

`path` dependencies are local content and `git` dependencies are pinned by
their rev, so neither needs locking. `version` dependencies resolve against
whatever the system has, so each resolution is recorded in `dowel.lock` at
the workspace root ([ADR-0015](adr/0015-version-deps-pkgconfig.md)):

```toml
[[package]]
name    = "zlib"
version = "1.3.1"
source  = "pkg-config"
```

- A resolution with no entry is **appended**
- A resolution matching its entry is silent
- A resolution differing from its entry warns with `lockfile-drift` and the
  file is **never rewritten silently** — delete the entry (or the file) to
  accept the new resolution

The lock detects drift; it does not restore anything. A system package
cannot be fetched, so the promise is "you will notice a changed
environment", not "you will get the same bits".

## What is deliberately absent

- **No expressions** — enforced, see above
- **No target definitions** — targets live in `dowel.build`
- **No version ranges** — a `version` constraint is a lower bound only;
  comparison is delegated to pkg-config itself
