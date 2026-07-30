# ADR-0015: Resolve `version` dependencies through pkg-config, record them in `dowel.lock`

**Status**: Accepted

## Context

`dowel.toml` accepts three dependency forms: `path`, `git`, and `version`.
The first two were implemented; `version = "..."` was refused with a
diagnostic. The C/C++ world has no single canonical registry
(docs/00-overview.md), and [ADR-0001](0001-toolchain-vs-supply.md) already
decided that dowel owns the toolchain but **delegates dependency supply**.
Building a native registry client (index protocol, tarball fetching,
checksums, a version-constraint solver) would contradict that delegation and
carry a large maintenance surface for little gain: on every practical target
the system already has a working resolver for installed C/C++ libraries —
pkg-config.

Unlike `path` (the content is local) and `git` (the rev pins the content),
a system package is whatever the environment happens to have. That makes the
resolution non-reproducible by nature, which calls for a record of what was
resolved so a changed environment is noticed rather than silently used.

## Decision

`version = "..."` dependencies are resolved by delegating to the system
`pkg-config`:

- The dependency name is the pkg-config module name. Existence is checked
  with `--modversion`; the declared version is a **minimum**, checked with
  `--atleast-version=<v>`. dowel implements no version comparison of its own
- `--cflags` and `--libs` become the public `flags` and `link_flags` of a
  synthetic external `lib` node, so consumers inherit them through the usual
  public-property propagation — the same shape as any other dependency
- Failure (module absent, version too low, pkg-config unavailable) is the
  error `unsatisfied-dependency`, with the remedy stated: install the
  package, lower the constraint, or declare the dependency as `path`/`git`

Each resolution is reconciled against `dowel.lock` at the workspace root:

- No entry for the package: the resolved name/version/source is **appended**
- Entry matches: nothing happens
- Entry differs: the warning `lockfile-drift` is emitted and the lock is
  **never rewritten silently**. Accepting the new resolution means deleting
  the entry (or the file)

Editor sessions (`load_for_editor`) start no external processes: `version`
dependencies are left unresolved in the LSP, and `unsatisfied-dependency` /
`lockfile-drift` never originate from the language server.

## Consequences

- The lock records drift; it does not promise restoration. A system package
  cannot be fetched, so `dowel.lock` guarantees only "you will notice",
  not "you will get the same bits". This is weaker than Cargo's lockfile
  and is stated as such in the file's header comment
- Version constraints are lower bounds only. Ranges, exact pins, and
  exclusions are delegated to pkg-config's own capabilities and are not
  expressible in `dowel.toml`
- Platforms without pkg-config cannot use `version` dependencies; `path`
  and `git` remain available there
- A native registry or tarball-based supply, if ever wanted, is a separate
  future decision; nothing here precludes it, and `source = "pkg-config"`
  in the lock leaves room for other sources
