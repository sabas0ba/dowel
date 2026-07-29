# `dowel.build` reference

The complete syntax of `dowel.build`, and every property that can be
configured. The machine-readable form of everything on this page is
`dowel schema dump` — the language server's hover and the type checker read
the same tables, so this page, the editor, and the diagnostics cannot
disagree silently.

How the declared values behave at build time — propagation, merging, glob
expansion — is in [13-semantics.md](13-semantics.md).

## 1. File structure

A `dowel.build` file is a sequence of tables, like TOML:

```
# comment
[lib.foo]                 # table header: [<kind>.<name>]
sources = glob("src/**.c")

[lib.foo.public]          # a block of the same target
includes = [dir("include")]
```

Inherited from TOML: table headers `[a.b.c]`, `key = value` entries, arrays
`[...]`, inline tables `{ k = v, ... }`, `#` line comments, implicit table
creation. Strings come in three forms — basic `"..."`, literal `'...'`, and
multi-line `"""..."""`. Integers and booleans are as in TOML.

Added on top of TOML, **in value position only**: function calls, `match`,
postfix `when`, and configuration references. There are no variables, no
string concatenation, no arithmetic, no recursion, and (as yet) no
iteration: expressions are pure and total, so evaluation always terminates
([ADR-0004](adr/0004-syntax.md)).

Nesting of containers (arrays, inline tables, calls, `match` arms) is
accepted up to 64 levels; deeper input gets `nesting-too-deep`, and the limit
can be raised with `--max-nesting=<n>` up to 512
([60-cli.md](60-cli.md)). A leading UTF-8 BOM is accepted.

## 2. Table kinds

The `kind` in `[<kind>.<name>]` is a closed vocabulary. An unknown kind
fails type checking with a suggestion.

| kind | Meaning | Artifact | Status |
|---|---|---|---|
| `lib` | static library | `lib<name>.a` | implemented |
| `bin` | executable | `bin/<name>` | implemented |
| `test` | test executable; run by `dowel test`, exit status 0 = pass | `bin/<name>` | implemented |
| `bench` | benchmark target | — | reserved, not implemented |
| `template` | non-recursive reuse unit | — | reserved, not implemented |
| `toolchain` | toolchain description | — | reserved, not implemented |
| `runner` | execution wrapper; the name is a **target triple**, not a target name | none | implemented |

Targets are referenced as `<name>` or `<package>:<name>` on the command
line, and via `target("name")` / `dep("package")` in properties.

## 3. Target properties

A target has three blocks. The root block holds what belongs to the target
itself; `public` and `private` hold the properties that feed compilation and
propagate (or not) to dependents. Whether a property propagates is decided
by the **block**, not the property: both blocks accept the same property
set.

### `[<kind>.<name>]` — root block

| Property | Type | Merge | Meaning |
|---|---|---|---|
| `sources` | `List<Path>` | `append` | sources to compile. Does not propagate. C only today — a C++ extension is rejected with `unsupported-language` |

### `[<kind>.<name>.public]` and `[<kind>.<name>.private]`

`public` affects this target **and** everyone who depends on it. `private`
affects this target only. (The precise formulas are in
[13-semantics.md](13-semantics.md).)

| Property | Type | Merge | Meaning |
|---|---|---|---|
| `includes` | `Set<Path>` | `union` | include search paths (`-I`). Ordered along the dependency graph: your own first, dependencies after |
| `defines` | `Map<Ident, Val>` | `error_on_conflict` | preprocessor definitions (`-D`). Two different values arriving for the same name fail, with both provenances shown |
| `flags` | `List<Str>` | `append` | compile flags, order-preserving |
| `link_flags` | `List<Str>` | `append` | link flags, order-preserving |
| `deps` | `List<DepRef \| TargetRef>` | `append` | edges: `dep("name")` is a package dependency declared in `dowel.toml`; `target("name")` is a target in the same package |
| `abi` | `AbiLabel` | `must_equal` | ABI label. Every target linked together must declare the same value or the build fails (`abi-mismatch`) before linking. Currently a hand-written string; automatic computation is planned |

Unknown properties fail with `unknown-property` and an edit-distance
suggestion; wrong types with `type-mismatch`.

### `[runner.<triple>]` — execution wrappers

Runners launch cross-compiled test artifacts
(`dowel test --target=<triple>`). They produce no artifact and propagate
nothing, so they have their own property set — target properties like
`sources` are type errors here.

| Property | Type | Meaning |
|---|---|---|
| `command` | `Str` | the program that wraps the artifact, e.g. `qemu-riscv64` or `ssh` |
| `args` | `List<Str>` | arguments placed before the artifact path |
| `transfer` | `List<Str>` | a command that copies the artifact before launch, e.g. `["scp", "-q"]`. Source and destination are appended by the implementation — they are not written here ([ADR-0008](adr/0008-runner-transfer.md)) |
| `remote_dir` | `Str` | directory on the target machine that receives the artifact. Specified together with `transfer` |
| `host` | `Str` | host part of the transfer destination, forming `<host>:<path>` |

Runner values may use `match` / `when` like any other property.

## 4. Functions

Callable in value position. There are exactly five; unknown names fail with
a suggestion.

| Function | Signature | Meaning |
|---|---|---|
| `glob(pattern)` | `(Str) -> List<Path>` | files matching the pattern, expanded at plan time (never during evaluation). Patterns: `*` any run without `/`, `**` any run including `/`, `?` one character except `/` |
| `dir(path)` | `(Str) -> Path` | a directory, relative to the root of the package that writes the call |
| `file(path)` | `(Str) -> Path` | a file, relative to the same root |
| `dep(name)` | `(Str) -> DepRef` | reference to a dependency declared in this package's `dowel.toml`. An undeclared name is `undeclared-dependency` |
| `target(name)` | `(Str) -> TargetRef` | reference to another target in the same package |

`Path` is a distinct type from `Str`: paths always carry their base point
(the declaring package's root), and the language has no string concatenation
with which to build one. A plain string where a path is expected is a type
error.

## 5. Configuration references and conditionals

### The configuration vocabulary

Values can branch on the build configuration through a closed, dot-separated
vocabulary. (The vocabulary is provisional — Q1 in
[99-open-questions.md](99-open-questions.md) — but this is what is
implemented; `dowel schema dump` prints the live version.)

| Key | Domain | Values |
|---|---|---|
| `cfg.opt` | finite | `debug`, `release` (selected by `--config`) |
| `cfg.target` | open | the target triple (selected by `--target`); `match` on it requires a `_` arm |
| `host.os` | finite | `linux`, `macos`, `windows` |
| `host.arch` | finite | `x86_64`, `aarch64`, `riscv64` |
| `feature.<name>` | boolean | feature flags declared in `[features]` of `dowel.toml`; undeclared names are diagnosed with a suggestion |
| `tc.c` | open | identifier of the selected C toolchain |

### `match`

```
flags = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}
```

- The scrutinee is a configuration key; the arms map values to expressions
- Patterns are bare words (`debug`) or strings (`"debug"`); `_` is the
  wildcard
- **Exhaustiveness is checked.** A finite-domain key must either cover every
  value or have a `_` arm (`non-exhaustive-match`); a pattern outside the
  key's domain is `unknown-pattern` with a suggestion; open-domain keys
  (`cfg.target`, `tc.c`) always require `_`
- Duplicate arms are `duplicate-arm`
- Arms may nest further `match` / `when` expressions

### Postfix `when`

```
deps  = [dep("zlib") when feature.zlib]        # condition on an element
flags = ["-fsanitize=address"] when feature.asan   # condition on the whole value
```

Two predicate forms:

- `when feature.<name>` — boolean keys only; using it on a non-boolean key
  is `expected-comparison`
- `when <key> == "value"` — string comparison; on finite domains the value
  is checked against the vocabulary (`unknown-pattern` otherwise)

Composition is implicit AND only (chain `when` inside `match` arms for
anything more complex). A `when` binds to the expression before it on the
same line — it does not reach across a newline.

### What conditionals resolve to

A `match`/`when` value has type `Cfg<T>` after evaluation; nothing is
decided yet. Specialization (per `--config`/`--target`/`--features`)
resolves it: `match` picks its arm, a false `when` drops the element (from a
list or map) or the whole value. The chosen arm and dropped elements are
recorded in provenance and shown by `dowel why`
([13-semantics.md](13-semantics.md)).

## 6. Full example

```
[lib.foo]
sources = glob("src/**.c")

[lib.foo.public]
includes = [dir("include")]
defines  = { FOO_API = 1 }
deps     = [dep("bar")]
abi      = "gnu11"

[lib.foo.private]
includes = [dir("src")]
flags    = match cfg.opt {
    debug   => ["-O0", "-g3"],
    release => ["-O2", "-DNDEBUG"],
}
deps     = [dep("zlib") when feature.zlib]

[test.unit]
sources = glob("tests/*.c")

[test.unit.private]
deps = [target("foo")]

[runner.riscv64gc-unknown-linux-gnu]
command = "qemu-riscv64"
args    = ["-L", "/usr/riscv64-linux-gnu"]
```
