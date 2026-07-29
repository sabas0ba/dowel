# Manifest reference

What `dowel.toml` and `dowel.build` accept, and the types and merge semantics
behind them. The introduction to writing manifests is in
[62-getting-started.md](62-getting-started.md). This document includes the
full design, so parts of it are not implemented yet; those parts are marked
in place, and the current state is listed in
[91-implementation-status.md](91-implementation-status.md).

The manifest is split into two files. The rationale is
[ADR-0003](adr/0003-manifest-split.md).

| File | Format | Written by | Contents |
|---|---|---|---|
| `dowel.toml` | strict TOML | machines (read and write) | package information, dependencies, toolchain, policy |
| `dowel.build` | TOML-style dialect | humans | target definitions, propagated properties, conditionals |
| `dowel.lock` | generated (not implemented) | machines | resolution results, hashes, transitive dependencies |

## 1. `dowel.toml`

```toml
[package]
name    = "libfoo"
version = "0.3.1"
edition = "2026"

[toolchain]
c       = "clang-19"
sysroot = "x86_64-linux-gnu-glibc2.35"

[policy]
cooldown = "7d"
licenses = ["MIT", "Apache-2.0", "BSD-3-Clause"]

[[dependencies]]
name     = "zlib"
version  = "1.3"
optional = true

[[dependencies]]
name = "bar"
git  = "https://github.com/example/bar"
rev  = "9f3c0a1e2b7d4856c0f1a93e5d2b8c4770ae6135"

[[dependencies]]
name = "mylib"
path = "../mylib"

[[dependencies]]
name    = "winsock-shim"
version = "0.2"
when    = { os = "windows" }

[features]
default = ["zlib"]
```

### Rules

- Kept as strict TOML; expressions are not allowed in value position. This
  guarantees external tools (SBOM generators, vulnerability scanners, update
  bots) can read it without a custom parser
- Dependencies come in 4 forms: registry name / git / https tarball / local
  path. **Only `path` is implemented**; the three fetching forms and
  `dowel.lock` generation are not
  ([91-implementation-status.md](91-implementation-status.md))
- git dependencies may not resolve through branches or tags; a full 40-digit
  immutable object reference is required
- Conditions are **structs over a closed vocabulary**, as in
  `when = { os = "windows" }`. String-embedded mini-languages like Cargo's
  `[target.'cfg(windows)'.dependencies]` are not adopted (they fail the same
  way CMake generator expressions do)

## 2. `dowel.build`

```
# libfoo/dowel.build

[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
deps     = [dep("bar"), dep("mylib")]

[lib.foo.private]
includes = [dir("src")]
defines  = { FOO_BUILDING = 1 }
deps     = [dep("zlib") when feature.zlib]
flags    = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}

[test.unit]
sources = glob("tests/*.c")
deps    = [target("foo")]
```

### Syntax inherited from TOML

Table headers `[a.b.c]`, key = value, arrays, inline tables, basic and
multi-line strings, `#` line comments, implicit table creation.

### Elements added in value position only

| Element | Notation | Borrowed from |
|---|---|---|
| Function calls | `glob(...)`, `dir(...)`, `dep(...)`, `target(...)` | common |
| Exhaustive branching | `match cfg.opt { debug => …, release => … }` | Rust |
| Conditional elements | `dep("zlib") when feature.zlib` | original (postfix) |
| Namespace references | `cfg.opt`, `feature.zlib`, `host.os` | common |

Expressions are **pure and total**: no side effects, no variable bindings,
iteration only as comprehensions over finite lists, no recursion. Termination
is thereby guaranteed as part of the language specification
([ADR-0004](adr/0004-syntax.md)).

### Table kinds

The `kind` in `[<kind>.<name>]` is a closed vocabulary, each with its own
schema. An unknown `kind` fails type checking.

| kind | Meaning | Status |
|---|---|---|
| `lib` / `bin` / `test` | targets | implemented |
| `bench` | benchmark targets | not implemented |
| `template` | reuse unit (non-recursive) | not implemented |
| `toolchain` | toolchain description | not implemented |
| `runner` | execution wrapper (qemu etc.); in `[runner.<triple>]` the name is a target triple | implemented |

`runner` is the one kind whose name is a target triple rather than a target
name, and whose property set differs from the others (`command` and `args`).
It produces no artifacts and propagates nothing, so giving it the target
vocabulary would let meaningless declarations pass type checking.

### `public` / `private`

The counterpart of CMake's `INTERFACE` / `PRIVATE`, but separated by block
rather than qualified per property name. What propagates and what does not
are distinguished syntactically.

## 3. Types and merge semantics

The substance of the language is here: each property **declares its merge
rule as part of its type**.

```
schema {
  includes : Set<Path>        merge = union,  order = topological
  defines  : Map<Ident, Val>  merge = error_on_conflict
  flags    : List<Flag>       merge = append
  abi      : AbiLabel         merge = must_equal
}
```

| Merge rule | Behavior |
|---|---|
| `union` | set union, in topological order (dependents before dependencies — the order include search and linking expect) |
| `append` | concatenation, preserving order |
| `error_on_conflict` | if different values arrive, fail and present the provenance of both |
| `must_equal` | fail unless equal. ABI label verification is expressed this way (automatic label computation is not implemented; today a hand-written `abi` string is verified) |
| `replace` | the later-arriving value wins |

Because the merge rule belongs to the type, adding a property does not
require writing new verification code.

### Principal types

- **`Path`** — a distinct type from `string`. The base point (project root /
  build directory / sysroot) is part of the type, and the language provides
  no string concatenation for building paths. Much of CMake's accident
  surface originates here
- **`List<T>` / `Set<T>`** — there is no semicolon-separated-string
  representation
- **`Cfg<T>`** — a `T` parameterized by configuration; the result of `match`
  has this type. It corresponds to a generator expression, but as an ordinary
  type rather than a string-embedded mini-language. Configurations are
  substituted at action-generation time, so switching `--release` or
  `--target` does not re-run manifest evaluation

## 4. Abstraction (not implemented)

```
[template.cli_tool]
params = ["name", "srcs"]

[template.cli_tool.bin]
sources = srcs
deps    = [dep("cli-common")]
```

- Templates are non-recursive; a cycle in the call graph is detected
  statically and fails
- Iteration is limited to comprehensions over finite lists

## 5. Displaying provenance

```
$ dowel why target:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

The provenance chain is a walk of the query graph's subtree as-is; with an
incremental evaluation engine in place it requires no additional data
structure.

## 6. Guarding against confusion with TOML

`dowel.build` is a superset dialect of TOML, and existing TOML tools fail at
value position.

- The extension is not `.toml`, so editors do not apply TOML mode
- Input that is valid TOML but invalid here gets a diagnostic saying so
  explicitly
- Completion, highlighting, and diagnostics come from our own language server

## 7. Configuration vocabulary (provisional)

The vocabulary of the `cfg` / `feature` / `host` / `tc` namespaces is not
finalized ([99-open-questions.md](99-open-questions.md) Q1). Until it is, the
implementation carries the following as a **closed vocabulary**. The live
version is available from `dowel schema dump`.

| Namespace | Key | Domain |
|---|---|---|
| `cfg` | `opt` | `debug` / `release` |
| `cfg` | `target` | target triple (free-form string; `match` requires a `_` arm) |
| `host` | `os` / `arch` | build host values |
| `feature` | `<name>` | boolean; only names declared in `[features]` of `dowel.toml` |
| `tc` | `c` | identifier of the selected C toolchain |

Predicate composition in `when` is implicit AND only. Exhaustiveness checking
of `match` applies to keys with finite domains.
