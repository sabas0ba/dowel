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
| `lib` | library; static by default, shared with `linkage = "shared"` ([ADR-0030](adr/0030-shared-libraries.md)) | `lib<name>.a`; shared: `lib<name>.so` / `lib<name>.dylib` / `<name>.dll` | implemented |
| `bin` | executable | `bin/<name>`, `bin/<name>.exe` on Windows | implemented |
| `test` | test executable; run by `dowel test`, exit status 0 = pass | `bin/<name>`, `.exe` on Windows | implemented |
| `bench` | benchmark executable; measured by `dowel bench` ([ADR-0025](adr/0025-bench-wall-clock.md)) | `bin/<name>`, `.exe` on Windows | implemented |
| `template` | shared settings, expanded into the targets that `use` it ([ADR-0035](adr/0035-template-kind.md)) | none | implemented |
| `toolchain` | toolchain description | — | reserved, not implemented |
| `runner` | execution wrapper; the name is a **target triple**, not a target name | none | implemented |

Targets are referenced as `<name>` or `<package>:<name>` on the command
line, and via `target("name")` / `dep("package")` in properties.

The executable's spelling follows `target.os`: a Windows target produces
`bin/<name>.exe`, because that is what the compiler driver writes. The
spelling is decided in one place, so the runner, `artifacts`, `inspect`,
`dowel debug`, the `built:` line, and the freshness fingerprint all read the
same value. When they did not, the build looked fine and everything
afterwards was handed a path that did not exist (issue #112).

**A target's name is unique within its package**, across kinds: a package
cannot hold both `[lib.foo]` and `[bin.foo]`. The second declaration is
refused with `duplicate-target`, naming both sites. The name is what
`target("...")` resolves, what the `<package>:<target>` label spells, and
what the object directory is keyed on. Allowing two would mean qualifying
all three by kind — a wide change for what it buys, since the artifact
spellings (`libfoo.a` and `foo`) were the only thing that did not collide
(issue #114). A library and its CLI want
`[lib.foo]` with `[bin.foo-cli]`, or the `plot-core` / `plot` shape.

## 3. Target properties

A target has three blocks. The root block holds what belongs to the target
itself; `public` and `private` hold the properties that feed compilation and
propagate (or not) to dependents. Whether a property propagates is decided
by the **block**, not the property: both blocks accept the same property
set.

### `[<kind>.<name>]` — root block

| Property | Type | Merge | Meaning |
|---|---|---|---|
| `sources` | `List<Path>` | `append` | sources to compile. Does not propagate. C, C++, and assembly may mix in one target; the language is chosen per file by extension (C++: `.cc` `.cp` `.cpp` `.cxx` `.c++` `.CPP` `.C`; assembly: `.s` `.S` ([ADR-0048](adr/0048-assembly.md)); everything else compiles as C) |
| `use` | `List<TemplateRef>` | `append` | templates to expand into this target's blocks ([ADR-0035](adr/0035-template-kind.md)) |
| `targets` | `List<Str>` | `append` | triples this target is built for. Empty means every triple. Same spelling as `[package] targets`, narrower reach |
| `linkage` | `Str` | `replace` | how a `lib` is linked: `static` (the default) or `shared`. Ignored by other kinds |
| `exports` | `List<Str>` | `append` | the symbols a shared library exports. Required when `linkage = "shared"` |
| `soversion` | `Int` | `replace` | the ABI generation of a shared library. Enters the file name and the soname ([ADR-0040](adr/0040-shared-library-version.md)). Absent means the library carries no version |

#### Sharing settings between targets

`[template.<name>]` holds `public` and `private` blocks; a target takes one
with `use` ([ADR-0035](adr/0035-template-kind.md)):

```
[template.tool]

[template.tool.public]
includes = [dir("include")]

[template.tool.private]
flags = ["-Wall", "-Wextra", "-Werror"]
deps  = [target("core")]

[bin.probe]
sources = [file("src/probe.c")]
use     = [template("tool")]
```

A template **expands into the block it came from**: its `private` becomes
the target's `private`, its `public` becomes the target's `public`. That is
what a library with no sources cannot do — `public` is the only block that
propagates, so sharing a setting through a dependency means publishing it
to everything downstream.

Expansion places the template's values ahead of the target's own and then
merges normally, so `append` keeps that order and `replace` lets the target
win. `dowel why` names the template's line.

- A template holds settings only. `sources`, `targets`, `linkage`,
  `exports`, and `use` itself are refused — the root block says *what a
  target is*, and a template is not a target
- Templates do not use templates: reading is one level
- A template produces no artifact and is not in the graph. Naming one on
  the command line is `not-a-target`; declaring one is not, and `check`
  passes (issue #141)
- `use` naming an undeclared template is `unknown-template`

#### Restricting a target to some triples

`targets` names the triples a single target is built for. `[package]
targets` covers the whole package, which a library supporting several
triples cannot use — it needs "built for all four, tested on the three
that have an OS":

```
[test.vectors]
sources = [file("tests/vectors.c")]
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
```

A target outside its triples **does not appear** in that triple's plan; it
is not an error there, it is out of scope. Naming it explicitly is still
refused with `unsupported-target`, because a named target is a request and
a build that quietly produces nothing reads as success (issue #126).

Building or testing without naming targets reaches only this tree's
package. A dependency's own tests are not built by its consumers.

#### Shared libraries

A `lib` with `linkage = "shared"` produces a shared library rather than an
archive, and **must** declare `exports`
([ADR-0030](adr/0030-shared-libraries.md)):

```
[lib.core]
sources = glob("src/*.c")
linkage = "shared"
exports = ["core_open", "core_close"]
```

`exports` has no default. It is the one place where the platforms disagree
about what a declaration means: on ELF and Mach-O every non-`static` symbol
is exported unless something says otherwise, and on Windows nothing is
exported unless something says so. Taking either as the default would make
the same manifest describe two different interfaces, so dowel requires the
list and generates each linker's form of it — a version script, a Mach-O
symbol list, or a `.def` — from the one declaration. Omitting it is
`missing-exports`.

Names are written as the linker sees them. For C that is the function name;
for C++ it is the mangled name. dowel does not mangle, and adds only the
uniform `_` prefix Mach-O requires.

**The list is checked against the library that was built**
([ADR-0039](adr/0039-exports-are-checked.md)). After the build dowel asks
the toolchain's symbol lister what the library exports. A name in `exports`
that is not in the answer is `unexported-symbol`, reported at the line that
declared it. A misspelling is otherwise silent: the wrong name is simply
absent from the dynamic symbol table, and the failure appears in someone
else's build as an undefined reference. If the symbol lister is not
available the check is skipped, and the build succeeds as before.

**`soversion` declares the ABI generation**
([ADR-0040](adr/0040-shared-library-version.md)):

```
[lib.core]
sources   = glob("src/*.c")
linkage   = "shared"
soversion = 2
exports   = ["core_open", "core_close"]
```

The library becomes `libcore.so.2` — `libcore.2.dylib` on macOS,
`libcore-2.dll` on Windows — and consumers record that name, so two
generations can sit in one directory. The unversioned `libcore.so` is placed
beside it as a symlink, which is what `-lcore` resolves through.

The number is the ABI generation, not the release: it changes when the
interface stops being compatible. That is why `[package] version` does not
supply it — a patch release would otherwise relink every consumer. dowel
does not decide when the number must change; the author does. Declaring
nothing keeps the plain name, and a negative number is `invalid-soversion`.

**Within its own package, a shared library is linked statically**
([ADR-0038](adr/0038-shared-inside-its-package.md)). `exports` is a
boundary toward code that was not written alongside it, and a package is
the unit of distribution — so a sibling target links the archive that is
built beside the shared object, and sees everything. This is what lets a
library's own tests reach inside it; testing only the public surface
cannot cover what is behind it. A consumer in another package
(`dep("...")`) links the shared library and sees exactly `exports`.

The shared library is built even when nothing links it, since the reason
to declare one is to ship it.

Declaring one library shared also changes how its dependencies are
compiled: every target in a shared library's link closure is compiled
`-fPIC`, because non-position-independent objects cannot be linked into a
position-independent output.

Binaries that link a shared library record a run-time search path pointing
at the build tree's `lib/` directory, so they run from the build tree
without help. They also record one **relative to themselves**
(`$ORIGIN/../lib`, `@loader_path/../lib` on macOS), which is what lets
`dowel install` copy rather than relink
([ADR-0041](adr/0041-install.md)) — the installed executable finds its
libraries wherever the prefix ends up. Windows has neither mechanism, so
`dowel test` and `dowel bench` put that directory on `PATH` for the child
process instead; a Windows executable started by hand from the build tree
will not find its DLLs.

`dowel install --prefix=<dir>` copies the products out of the build tree:
`bin` targets into `bin/`, `lib` targets into `lib/` with their unversioned
name, and each library's own `public.includes` into `include/`. See
[60-cli.md](60-cli.md).

Symbol versioning *inside* the library — version nodes in the generated
script, so one file carries two generations of a symbol — is not
implemented.

### `[<kind>.<name>.public]` and `[<kind>.<name>.private]`

`public` affects this target **and** everyone who depends on it. `private`
affects this target only. (The precise formulas are in
[13-semantics.md](13-semantics.md).)

| Property | Type | Merge | Meaning |
|---|---|---|---|
| `includes` | `Set<Path>` | `union` | include search paths (`-I`). Ordered along the dependency graph: your own first, dependencies after |
| `defines` | `Map<Ident, Val>` | `error_on_conflict` | preprocessor definitions (`-D`). The value's **type decides its form**: a `Str` becomes a C string literal (`-DNAME="hashx"`), an `Int` or `Bool` a bare token (`-DLIMIT=64`, `-DDEBUG=1`). Two different values arriving for the same name fail, with both provenances shown |
| `flags` | `List<Str>` | `append` | compile flags for every language, order-preserving |
| `c_flags` | `List<Str>` | `append` | compile flags for C sources only, placed after `flags` |
| `cxx_flags` | `List<Str>` | `append` | compile flags for C++ sources only, placed after `flags` |
| `asm_flags` | `List<Word>` | `append` | flags for assembly sources only, placed after `flags` ([ADR-0048](adr/0048-assembly.md)). `c_flags` and `c_std` do **not** reach assembly — a language-specific flag belongs to its language |
| `c_std` | `Str` | `max` | the C standard: `c89` `c99` `c11` `c17` `c23`. Becomes `-std=` for C sources |
| `cxx_std` | `Str` | `max` | the C++ standard: `c++98` `c++03` `c++11` `c++14` `c++17` `c++20` `c++23` `c++26`. Becomes `-std=` for C++ sources |
| `link_flags` | `List<Str \| Path>` | `append` | link flags, order-preserving. A `Path` element expands to its absolute path, which is how a linker script inside the package is named (`["-T", file("ld/app.ld")]`) — the link runs in the build directory, so a relative string would not reach it. Unlike the translation properties, these follow the **link closure** even across `private` edges — a static archive cannot carry its own link requirements ([13-semantics.md](13-semantics.md)) |
| `deps` | `List<DepRef \| TargetRef>` | `append` | edges: `dep("name")` is a package dependency declared in `dowel.toml`; `target("name")` is a target in the same package |
| `abi` | `AbiLabel` | `must_equal` | ABI label, written as one word or as a set of components. Targets linked together must not contradict each other, or the build fails (`abi-mismatch`) before linking. Components are compared one by one, so a component only one side names is not a constraint ([ADR-0042](adr/0042-abi-label-components.md)). The word `c` names the **C ABI boundary** rather than a language, matches any label, and never replaces one ([ADR-0019](adr/0019-c-abi-label.md)). Labels are hand-written; automatic computation is planned |

Unknown properties fail with `unknown-property` and an edit-distance
suggestion; wrong types with `type-mismatch`. `c_std` / `cxx_std` also have
a closed vocabulary: a value outside it is `unknown-standard`, checked where
it is written — every `match` arm and `when` branch included — so a
misspelling does not wait for the configuration that selects it.

**`max` is why a standard is not a flag.** The highest standard reached
along the closure wins ([ADR-0016](adr/0016-language-standard-property.md)).
A library requiring `c++17` used by a `c++20` binary compiles fine, and a
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

### `[test.<name>.cases]` — registering several tests from one binary

A `test` target runs its binary once and is judged by exit status. Declaring
cases registers **several invocations of that same binary**, each reported
and selected on its own ([ADR-0022](adr/0022-test-cases.md)).

`[bench.<name>.cases]` takes the same shape and keys, with one exception:
`should_fail` is refused there — a benchmark is measured, not judged, so
there is no verdict to invert ([ADR-0025](adr/0025-bench-wall-clock.md)).
A `harness` is not accepted on a bench either.

```toml
[test.suite]
sources = glob("tests/*.c")

[test.suite.cases]
parse   = { args = ["parse"], timeout = 10 }
emit    = { args = ["emit"], labels = ["slow"] }
rejects = { args = ["bad"], should_fail = true }
strict  = { args = ["check"], env = { SUITE_MODE = "strict" } }
```

| Key | Type | Behavior |
|---|---|---|
| `args` | `List<Str>` | appended to the launch command. This is what distinguishes one case from another; a case with no `args` runs the binary bare |
| `env` | `Map<Ident, Str>` | environment variables set for this case only |
| `timeout` | `Int` | seconds. The case is killed and reported as timed out, whatever exit status the kill produced. Without it, dowel waits |
| `should_fail` | `Bool` | the case passes on a nonzero exit. Exiting 0 fails, and says that `should_fail` expected otherwise |
| `labels` | `List<Str>` | names this case answers to; `dowel test --label <name>` selects by them |
| `cwd` | `Path` | the directory the case runs in. The default is the package root |

- The case's name is the key. Its label is `<package>:<target>/<case>`, which
  is what the summary, `--message-format=json`, `--failed`, and the command
  line all read. A name containing `/` or whitespace, or an empty one, breaks
  that grammar and is refused (`invalid-name`) — use `-` or `_` where a
  separator is wanted (issue #97)
- The **working directory is the package root** unless the case says
  otherwise, the same as a target with no cases. Fixed assets a test reads
  therefore resolve against the same base the manifest wrote them against.
  This is a promise, not an observation — a test may rely on it (issue #95)
- `cwd` moves one case elsewhere, for tests that read their data by relative
  path or write output files. Two cases of the same binary writing to the
  same place is one of the reasons `--test-jobs` defaults to sequential;
  giving each its own directory removes it:

  ```toml
  [test.suite.cases]
  golden = { args = ["golden"], cwd = dir("tests/golden") }
  ```

  The path is relative to the package that wrote it, like every other
  `dir()`. A directory that does not exist is reported as such, rather than
  as a binary that could not be started
- `should_fail` says the binary **exits nonzero**. A case killed by a signal
  does not satisfy it and is reported as a crash — the place where
  `should_fail` is written is the place a crash is most likely, and treating
  the two alike turns the defect most worth catching green (issue #88)
- `timeout` must be positive. `0` and negative values would silently mean
  "wait forever", the opposite of what writing a timeout says
  (`invalid-value`)
- A `cases` block with no case in it is refused (`empty-block`). "No cases
  block" and "a cases block that ended up empty" are different intentions,
  and the second would otherwise become one bare run of the binary with no
  arguments (issue #99)
- A target with **no** `cases` block is one test named after the target —
  the behavior that existed before, unchanged
- A case adds no translation unit. To compile something else, write another
  `[test.<name>]`
- `match` / `when` apply both **inside** a case and **to the case itself**.
  A timeout that differs per configuration is one use; the stronger one is a
  case that must not exist at all for some target — one that only means
  something on real hardware, or that an emulator cannot finish in a
  realistic time (issue #92):

  ```toml
  [test.suite.cases]
  onhw = { args = ["hw"] } when cfg.target == "thumbv7em-none-eabihf"
  slow = { args = ["big"], labels = ["slow"] } when feature.long_tests
  ```

  Every arm is checked, not only the one the current configuration picks —
  otherwise an error in another arm surfaces on the day the configuration
  changes. A target whose cases all drop out runs nothing, which is not a
  failure: it is what the manifest asked for
- No test harness is imposed. dowel never asks the binary what cases it
  contains — which framework the tests use stays the project's decision.
  A suite with many functions is registered per *group*, passing the
  framework's own filter in `args`

### `[test.<name>.harness]` — letting the binary list its own cases

Where a suite already enumerates itself, the cases can come from the code
instead of being written a second time in the manifest
([ADR-0023](adr/0023-harness-protocol.md)).

```toml
[test.suite]
sources = glob("tests/*.c")

[test.suite.harness]
list    = ["--list"]      # these arguments make it print the case names
run     = ["--run"]       # these, then the name, run one case
timeout = 30
labels  = ["unit"]
```

| Key | Type | Behavior |
|---|---|---|
| `list` | `List<Str>` | required. Arguments that make the binary print its case names on stdout, **one per line**. Blank lines and lines starting with `#` are skipped; nothing else is interpreted. There is no default — a harness that does not say how to list says nothing |
| `run` | `List<Str>` | arguments placed before the case name when running one case. The name is appended positionally, like every other command dowel assembles ([ADR-0008](adr/0008-runner-transfer.md)) |
| `timeout` | `Int` | seconds, applied to the listing and to each discovered case |
| `env` | `Map<Ident, Str>` | set for the listing and for every discovered case |
| `labels` | `List<Str>` | carried by every discovered case |

- Each name becomes a case labelled `<package>:<target>/<name>`. Selection,
  `--failed`, parallelism, and reporting work exactly as for declared cases
- The listing runs at test time, through the same runner as the tests, so a
  cross build asks the binary through its `[runner.<triple>]`
- A listing that fails, times out, or prints nothing is a **failure of that
  target** — not zero tests. Being unable to enumerate is not the same as
  having nothing to run
- A listed name has to satisfy the same grammar as one written in the
  manifest: no `/`, no whitespace, not empty. The line's contents are still
  not interpreted, but **the grammar of an acceptable name is one, whichever
  entrance it came through** — otherwise a name refused in the manifest
  enters here and produces a label nothing can split back apart (issue
  #108). A name that breaks it is reported like any other listing failure,
  because the user cannot edit it in place; have the harness print names
  without those characters
- `cases` and `harness` cannot both be declared (`conflicting-declaration`):
  both answer what the cases are
- dowel knows no test framework, only these two argument lists. A framework
  whose listing is not one name per line, or whose selection needs
  `--flag=NAME` instead of a separate argument, needs a few lines of wrapper
  in the project that chose it

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
| `debug_args` | `List<Str>` | arguments that make the runner host the program behind a debug stub, such as qemu's `-g <port>` or `-gdb tcp::1234`. Inserted **before** `args`, giving `<command> <debug_args...> <args...> <artifact>` — they cannot go after, because `args` may end with the flag that takes the artifact (`-kernel`) and anything between that flag and the artifact is eaten as its operand ([ADR-0024](adr/0024-debug-command.md)) |
| `debug_connect` | `Str` | where the debugger attaches, such as `localhost:1234`. Written separately from `debug_args` because dowel does not parse the runner's flags and can derive neither from the other. `dowel debug --target=<triple>` needs both, and refuses with `missing-debug-stub` without them |

Runner values may use `match` / `when` like any other property.

## 4. Functions

Callable in value position; unknown names fail with a suggestion.

| Function | Signature | Meaning |
|---|---|---|
| `glob(pattern)` | `(Str) -> List<Path>` | files matching the pattern, expanded at plan time (never during evaluation). Patterns: `*` any run without `/`, `**` any run including `/`, `?` one character except `/` |
| `dir(path)` | `(Str) -> Path` | a directory, relative to the root of the package that writes the call |
| `file(path)` | `(Str) -> Path` | a file, relative to the same root |
| `dep(name)` | `(Str) -> DepRef` | reference to a dependency declared in this package's `dowel.toml`. An undeclared name is `undeclared-dependency` |
| `target(name)` | `(Str) -> TargetRef` | reference to another target in the same package |
| `template(name)` | `(Str) -> TemplateRef` | reference to a `[template.<name>]` in the same file ([ADR-0035](adr/0035-template-kind.md)) |
| `sysroot([path])` | `([Str]) -> Path` | the toolchain's sysroot, or a path under it ([ADR-0047](adr/0047-sysroot.md)). The one function that takes no argument, since the root itself is the common case. Declared as `[toolchain] sysroot`; writing it with none declared is `missing-sysroot` |

`Path` is a distinct type from `Str`: paths always carry their base point —
the declaring package's root, the build directory, or the sysroot — and the
language has no string concatenation with which to build one. A plain string
where a path is expected is a type error.

That is also why `flags`, `c_flags`, `cxx_flags`, and `link_flags` are
`List<Word>`: an element may be a `Str` or a `Path`, and a `Path` expands to
its absolute spelling. `["-I", sysroot("usr/include")]` is two words, which
is how a path reaches a command line without concatenation (issue #70).

## 5. Configuration references and conditionals

### The configuration vocabulary

Values can branch on the build configuration through a closed,
dot-separated vocabulary. **Closed means nothing extends it** — not a
toolchain, not a package, not a flag
([ADR-0034](adr/0034-closed-vocabulary.md)). `dowel schema dump` prints the
live version.

It holds what dowel itself knows about a build. A project's own axes —
sanitizers, LTO modes, a vendored-vs-system choice — go in `[features]`,
which is the second layer: dowel declares what it knows, the package
declares the rest. An unknown key says so and names the alternative.

| Key | Domain | Values |
|---|---|---|
| `cfg.opt` | finite | `debug`, `release` (selected by `--config`) |
| `cfg.target` | open | the target triple (selected by `--target`); `match` on it requires a `_` arm |
| `target.os` | finite | `linux`, `macos`, `windows`, `none` (bare metal), `other` — the OS **being built for**, read off the triple |
| `target.arch` | finite | `x86_64`, `x86`, `aarch64`, `arm`, `riscv64`, `other` — the architecture being built for |
| `target.env` | finite | `gnu`, `musl`, `msvc`, `apple`, `none`, `other` — the C runtime being built against, also read off the triple. `target.os` does not answer this: `linux-gnu` and `linux-musl` are the same OS and two runtimes that do not link ([ADR-0042](adr/0042-abi-label-components.md)) |
| `host.os` | finite | `linux`, `macos`, `windows` — the machine **doing the building** |
| `host.arch` | finite | `x86_64`, `aarch64`, `riscv64` |
| `feature.<name>` | boolean | feature flags declared in `[features]` of `dowel.toml`; undeclared names are diagnosed with a suggestion |
| `tc.c` | open | identifier of the selected C toolchain |
| `tc.cxx` | open | identifier of the selected C++ toolchain |
| `tc.ar` | open | identifier of the selected archiver |

`target.*` and `host.*` are a pair and answer different questions
([ADR-0026](adr/0026-target-os-arch.md)). Selecting an implementation per
operating system wants `target.os`:

```toml
sources = [file("src/text.c"), match target.os {
    windows => file("src/plat_win.c"),
    _       => file("src/plat_posix.c"),
}]
```

Writing `host.os` there compiles and picks the build machine's answer, which
is why the words are spelled apart. `host.*` is for questions about the
machine doing the work — whether the artifact could be run here, whether a
tool exists on it. Both domains are finite, so a `match` that covers every
value needs no `_` and a new value breaks the manifest instead of falling
into a default. `other` exists because `--target` takes any string: a triple
with no word of its own has to land somewhere, and specificity beyond these
words is what `cfg.target` is for.

### Package constants

`pkg` is not part of that vocabulary. It holds constants of the package
whose manifest declares them, and it is read **in a value position** — the
only namespace that can be ([ADR-0020](adr/0020-package-constants.md)).

| Reference | Type | Value |
|---|---|---|
| `pkg.name` | `Str` | `[package] name` of the declaring package |
| `pkg.version` | `Str` | `[package] version` of the declaring package |

```toml
[lib.hashx.private]
defines = { HASHX_VERSION = pkg.version, HASHX_NAME = pkg.name }
```

This is how a library's version reaches the code that reports it, instead of
being written a second time in a header where nothing compares the two
(issue #80). Because `defines` renders a `Str` as a C string literal, the
above produces `-DHASHX_VERSION="0.4.0"`.

A package constant belongs to the package that declares it, the same way a
feature does ([ADR-0017](adr/0017-feature-forwarding.md)): a dependency's
`pkg.version` is the dependency's own version, not the root's.

It is **not** usable as a `match` scrutinee or in a `when` predicate
(`not-a-configuration-key`). A package's own version is not an axis a build
varies along. Conversely a configuration reference is still refused in a
value position (`unexpected-reference`).

There is still no string concatenation ([ADR-0004](adr/0004-syntax.md)), so
a composite like `"hashx/0.4.0"` is not expressible.

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

Predicates compose with `and` / `or` / `not`
([ADR-0032](adr/0032-predicate-composition.md)):

```toml
flags = ["-pthread"] when target.os == "linux" or target.os == "macos"
flags = ["-fPIC"]    when not target.os == "windows"
deps  = [dep("zlib") when feature.zlib and not feature.minimal]
flags = ["-DX"]      when (target.os == "linux" or target.os == "macos") and cfg.opt == "debug"
```

Precedence is `not` > `and` > `or`, with parentheses to override. The
operators are words, not symbols, matching the rest of the language
(`when`, `match`, `glob`).

`not` is what keeps "everywhere except Windows" correct when the
vocabulary grows; listing the other values silently stops covering them
the day a word is added to `target.os`.

A `when` — and every operator inside it — binds on the same line. It does
not reach across a newline, so a following key that happens to be named
`or` is a key, not an operator.

Domain checking reaches every leaf: a misspelled value is `unknown-pattern`
inside `and` / `or` / `not` exactly as it is on its own.

**Use `match`, not stacked `when`s, to choose between implementations.**
Feature flags are additive — `--features=x11` does not switch `headless`
off — so two `when`s are not a choice:

```toml
# wrong: --features=x11 compiles both
sources = [
    file("src/shell_x11.c")      when feature.x11,
    file("src/shell_headless.c") when feature.headless,
]

# right: exactly one, always
sources = [
    match feature.x11 {
        true  => file("src/shell_x11.c"),
        false => file("src/shell_headless.c"),
    },
]
```

Compiling both is not always an error you will see. In a `bin` the linker
reports `multiple definition`; in a `lib` the build **succeeds** and the
archive keeps whichever member the linker reached first, so the artifact
silently holds an implementation nobody chose (issue #82). Where the choice
is genuinely between named features rather than one boolean, declare them
mutually exclusive with `[features] exclusive`
([11-toml-reference.md](11-toml-reference.md),
[ADR-0021](adr/0021-exclusive-features.md)).

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
abi      = "c"            # this surface is `extern "C"`; consumers keep their own label

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
