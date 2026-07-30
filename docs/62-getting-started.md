# Getting started

From installation to building, testing, and running your first project.
Task-oriented how-tos are in [63-guides.md](63-guides.md), the manifest
reference in [10-manifest.md](10-manifest.md), and the command reference in
[60-cli.md](60-cli.md).

## 1. Installation

There are no binary releases yet; build from source. You need:

| Requirement | Used for |
|---|---|
| A Rust toolchain (`cargo`) | building `dowel` itself |
| A C compiler | compiling your project; the default is `cc` on PATH |
| A C++ compiler | only for projects with C++ sources; the default is `c++` on PATH |
| `ninja` | the default executor; without it, the sequential executor (`--executor=direct`) works |

```sh
git clone https://github.com/sabas0ba/dowel
cd dowel
cargo build --release
export PATH="$PWD/target/release:$PATH"

dowel --version
```

If you need to pin or switch versions, `dowelup` acquires and manages dowel
itself ([61-acquisition.md](61-acquisition.md)).

## 2. Try the working example

[`examples/hello`](../examples/hello) is a two-package setup: a static
library (`libgreet`) and an executable that uses it (`app`).

```sh
cd examples/hello/app
dowel check                      # report diagnostics only; does not build
dowel build                      # generate ninja files and run them
./.dowel/build/*/bin/app

cd ../libgreet
dowel test                       # build and run the test targets
```

## 3. Create a minimal project

```sh
dowel new myapp
cd myapp
dowel build
./.dowel/build/*/bin/myapp
```

`dowel new` scaffolds a working `bin` package (`--lib` for a library, which
comes with a passing `dowel test` target). What it generates is exactly the
minimal form below — two files plus a source. `dowel.toml` holds package
information (strict TOML read and written by machines) and `dowel.build`
holds target definitions (written by humans); the reasons for the split are
in [10-manifest.md](10-manifest.md).

```
myapp/
├── dowel.toml
├── dowel.build
└── src/
    └── main.c
```

`dowel.toml`:

```toml
[package]
name    = "myapp"
version = "0.1.0"
edition = "2026"
```

`dowel.build`:

```
[bin.myapp]
sources = glob("src/*.c")
```

```sh
dowel check
dowel build
./.dowel/build/*/bin/myapp
```

Build outputs and intermediates go under `.dowel/` (add it to your git
ignore). Build directories are separated per configuration (`--config`).
Deleting `.dowel/` is always safe: correctness is never lost, only the cached
speedup (see the store section of [60-cli.md](60-cli.md)).

## 4. Split out a library and depend on it

```sh
dowel add libs/util                       # scaffold a library and declare the dependency
dowel add --git https://github.com/x/y    # declare a git dependency, pinned to HEAD's sha
```

`dowel add` creates a library package in a subdirectory (or, with `--git`,
declares an external repository pinned to a full commit sha) and appends the
`[[dependencies]]` entry to your `dowel.toml`; wiring it into a target
(below) stays your choice, and the command prints the exact line to add.

Dependencies between packages are declared in `dowel.toml` — as a local
`path`, or as a `git` URL pinned to a full commit sha
([11-toml-reference.md](11-toml-reference.md); registry fetching is not
implemented yet). Which target uses a dependency is written in
`dowel.build`.

`app/dowel.toml`:

```toml
[[dependencies]]
name = "libgreet"
path = "../libgreet"
```

`app/dowel.build`:

```
[bin.app.private]
deps = [dep("libgreet")]
```

On the library side, what propagates to dependents (`public`) and what applies
only to the library itself (`private`) are separated by block:

```
[lib.greet]
sources = glob("src/**.c")

[lib.greet.public]
includes = [dir("include")]      # also affects the compilation of app

[lib.greet.private]
includes = [dir("src")]          # affects only this library; invisible to app
```

`dowel why` traces where a propagated value came from:

```sh
dowel why app:app includes
```

## 5. Add tests

```
[test.unit]
sources = glob("tests/*.c")

[test.unit.private]
deps = [target("greet")]
```

`dowel test` builds and runs them, treating exit status 0 as success. This
follows the C convention; no test harness is imposed.

```sh
dowel test
dowel test --nocapture           # pass test output through
dowel test --failed --fail-fast  # only what failed last time, stop at the first failure
```

## 6. The everyday loop

- `dowel check` — on every save. Runs through planning and reports diagnostics
  only, without executing anything, so it is fast
- `dowel build` / `dowel test` — verify for real
- `dowel why <target> <property>` — answers "why does this value look like
  this" with the propagation path
- `DOWEL_LOG=debug dowel build` — answers "why did this rebuild" in the log

If you write manifests in an editor, there is a language server (`dowel lsp`;
[63-guides.md](63-guides.md) section 6). Diagnostics carry locations and
stable codes, and unknown names come with suggestions.

## 7. What to read next

- Task-oriented how-tos (switching configurations, cross execution, CI) —
  [63-guides.md](63-guides.md)
- Pinning and switching versions of dowel itself (`dowelup`) —
  [61-acquisition.md](61-acquisition.md)
- Everything the manifests accept — [11-toml-reference.md](11-toml-reference.md)
  and [12-build-reference.md](12-build-reference.md); how it behaves —
  [13-semantics.md](13-semantics.md)
- Every command and option — [60-cli.md](60-cli.md)
- What works today and what doesn't —
  [91-implementation-status.md](91-implementation-status.md)
