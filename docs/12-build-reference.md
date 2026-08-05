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
| `sources` | `List<Path>` | `append` | sources to compile. Does not propagate. C and C++ may mix in one target; the language — and so the compiler — is chosen per file by extension (C++: `.cc` `.cp` `.cpp` `.cxx` `.c++` `.CPP` `.C`, everything else compiles as C) |

### `[<kind>.<name>.public]` and `[<kind>.<name>.private]`

`public` affects this target **and** everyone who depends on it. `private`
affects this target only. (The precise formulas are in
[13-semantics.md](13-semantics.md).)

| Property | Type | Merge | Meaning |
|---|---|---|---|
| `includes` | `Set<Path>` | `union` | include search paths (`-I`). Ordered along the dependency graph: your own first, dependencies after |
| `defines` | `Map<Ident, Val>` | `error_on_conflict` | preprocessor definitions (`-D`). Two different values arriving for the same name fail, with both provenances shown |
| `flags` | `List<Str>` | `append` | compile flags for every language, order-preserving |
| `c_flags` | `List<Str>` | `append` | compile flags for C sources only, placed after `flags` |
| `cxx_flags` | `List<Str>` | `append` | compile flags for C++ sources only, placed after `flags` |
| `c_std` | `Str` | `max` | the C standard: `c89` `c99` `c11` `c17` `c23`. Becomes `-std=` for C sources |
| `cxx_std` | `Str` | `max` | the C++ standard: `c++98` `c++03` `c++11` `c++14` `c++17` `c++20` `c++23` `c++26`. Becomes `-std=` for C++ sources |
| `link_flags` | `List<Str>` | `append` | link flags, order-preserving. Unlike the translation properties, these follow the **link closure** even across `private` edges — a static archive cannot carry its own link requirements ([13-semantics.md](13-semantics.md)) |
| `deps` | `List<DepRef \| TargetRef>` | `append` | edges: `dep("name")` is a package dependency declared in `dowel.toml`; `target("name")` is a target in the same package |
| `abi` | `AbiLabel` | `must_equal` | ABI label. Every target linked together must declare the same value or the build fails (`abi-mismatch`) before linking. Currently a hand-written string; automatic computation is planned |

Unknown properties fail with `unknown-property` and an edit-distance
suggestion; wrong types with `type-mismatch`. `c_std` / `cxx_std` also have
a closed vocabulary: a value outside it is `unknown-standard`, checked where
it is written — every `match` arm and `when` branch included — so a
misspelling does not wait for the configuration that selects it.

**`max` is why a standard is not a flag.** The highest standard reached
along the closure wins ([ADR-0016](adr/0016-language-standard-property.md)):
a library requiring `c++17` used by a `c++20` binary compiles fine, and a
library requiring `c++20` raises a consumer that asked for less — which is
what its public headers need. Written as `cxx_flags = ["-std=..."]` the two
would simply concatenate and the last one would silently win.

The generated `-std=` is placed **before** `c_flags` / `cxx_flags`, so an
explicitly written flag still overrides it. That is the escape hatch for GNU
dialects (`cxx_flags = ["-std=gnu++20"]`), which are deliberately outside
the vocabulary — a dialect is a different axis from a standard version and
cannot be placed in one order.

### `[<kind>.<name>.artifacts]` — deriving files from the artifact

Embedded work needs a step after linking: the ELF is turned into a raw
image, an Intel HEX file, or a stripped copy. Declaring it here puts that
step **inside** the build graph, so it is produced by `dowel build`, skipped
when its input has not changed, and performed by the tool the toolchain
selects for the target triple.

```toml
[bin.firmware]
sources = glob("src/*.c")

[bin.firmware.artifacts]
bin = { tool = "objcopy", args = ["-O", "binary"] }
hex = { tool = "objcopy", args = ["-O", "ihex"] }
```

Each key names the **extension of the produced file**: the output is the
target's artifact with its extension replaced, so `firmware` yields
`firmware.bin` and `firmware.hex` next to it in the build directory.

A derived file is produced whenever its target's artifact is, including when
that target is only reached as someone else's dependency: a library's
`.stripped` keeps appearing after a binary that links it is added (issue
#64). Whether a derived file exists is decided by the declaration, never by
how the target happened to be reached.

| Property | Type | Meaning |
|---|---|---|
| `tool` | `Str` | required. The **name** of a toolchain tool (`objcopy`), not a command. The concrete command comes from `[toolchain]` / `[toolchain.<triple>]`, so a cross build uses `arm-none-eabi-objcopy` without the manifest repeating it. A name outside the tool table is `unknown-tool`; a missing `tool` is `missing-field` |
| `args` | `List<Str>` | arguments placed before the paths |

The command run is `<tool> <args...> <input> <output>` — the input and
output are appended positionally and never written in the manifest, the same
rule runner transfers follow ([ADR-0008](adr/0008-runner-transfer.md)). A
tool whose invocation does not fit that shape cannot be expressed here; for
a stripped copy, use `objcopy` with `--strip-all` rather than `strip`.

The tool is probed at plan time only when a declaration uses it — a build
with no `artifacts` block never requires `objcopy` to exist. Because the
tool's command is part of the action's command line, changing the
declaration rebuilds the derived file.

### `[<kind>.<name>.inspect]` — reporting on the artifact

The counterpart of `artifacts`: tools that report rather than produce.
`size` for the flash and RAM budget, `nm` for symbols, `objdump -d` to read
what the optimizer did, `readelf -S` to check a linker script's answer.

```toml
[bin.firmware.inspect]
sections = { tool = "size", args = ["-A"] }
symbols  = { tool = "nm", args = ["--size-sort"] }
```

| Property | Type | Meaning |
|---|---|---|
| `tool` | `Str` | required. A toolchain tool's **name**, exactly as in `artifacts` — the command comes from `[toolchain]`, so a cross build reports with `arm-none-eabi-size` |
| `args` | `List<Str>` | arguments placed before the artifact path |

The command run is `<tool> <args...> <artifact>`; the artifact's path is
appended positionally, never written in the manifest.

An inspection produces **no file**, so there is nothing to be up to date
about: it is not part of the build graph, not a `dowel build` default, and
not incremental. It runs when asked:

```sh
dowel inspect                    # every target that declares an inspection
dowel inspect firmware           # one target
dowel inspect --message-format=json
```

`dowel inspect` builds first, then runs each declared tool and passes its
output through — dowel does not parse it. A tool exiting nonzero fails the
run, which is what makes a budget check expressible today as a wrapper
script. Interpreting a tool's output inside dowel (a `max_flash = ...`
declaration) needs a decision about per-tool output formats and is not part
of this.

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
| `tc.cxx` | open | identifier of the selected C++ toolchain |
| `tc.ar` | open | identifier of the selected archiver |

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
