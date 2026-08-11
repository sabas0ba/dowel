# Command reference

The specification of every command and option `dowel` provides. Everything in
this document is implemented; the not-yet-implemented list is in
[91-implementation-status.md](91-implementation-status.md). Task-oriented
how-tos are in [63-guides.md](63-guides.md).

## Invocation

```
dowel <command> [options] [args]
```

- Options accept both `--name value` and `--name=value`
- Unknown commands and options come with edit-distance suggestions
  (`--confg` → `did you mean --config?`)
- Running with no arguments prints usage

## Contract shared by every command

### Output streams

| Stream | Contents |
|---|---|
| stdout | Artifacts: JSON diagnostics, graphs, the schema, `why` results |
| stderr | Progress and logs |

Because of this split, `dowel graph --format=dot | dot -Tsvg` works at any
log level.

### Exit status

| Status | Meaning |
|---|---|
| 0 | Success, including runs whose diagnostics are warnings only |
| anything else | An error occurred; diagnostics appear on stdout / stderr per the split above |

`dowel test` returns nonzero if even one test fails. When `--fail-fast` cut
the run short, the summary reports how many tests were not run.

### Common options

| Option | Values | Default | Meaning |
|---|---|---|---|
| `-C, --directory <path>` | path | `.` | operate on the package in this directory |
| `--config <name>` | `debug` / `release` | `debug` | build configuration |
| `--target <triple>` | target triple | host | cross-compilation target ([63-guides.md](63-guides.md) section 5) |
| `--features <a,b>` | comma-separated | — | feature flags to enable; may be repeated |
| `--no-default-features` | — | — | do not pull in `default` from `[features]` |
| `--message-format <fmt>` | `human` / `json` | `human` | diagnostic format |
| `-v, --verbose` | — | — | more logging; once for info, twice or more for debug |
| `--log-level <level>` | `off` / `error` / `warn` / `info` / `debug` / `trace` | — | log level; an explicit value overrides `-v` |
| `--log-format <fmt>` | `text` / `json` | `text` | log format (one object per line) |
| `--max-nesting <n>` | number | 64 | maximum value-nesting depth the parser accepts (see below) |
| `--color <when>` | `auto` / `always` / `never` | `auto` | color; `auto` currently resolves to no color (no terminal detection), so pass `always` explicitly when needed |
| `-h, --help` | — | — | print usage |
| `-V, --version` | — | — | print the version |

### Nesting limit

Parsing accepts value nesting up to 64 levels by default; anything deeper
gets a `nesting-too-deep` diagnostic at the offending position. If a
generated manifest exceeds this, raise it with `--max-nesting=<n>` (capped
at 512 — accepting stack-exhausting depth would return an abort instead of a
diagnostic).

### Environment variables

| Variable | Meaning |
|---|---|
| `DOWEL_LOG` | same as `--log-level`; `DOWEL_LOG=trace dowel build` |

What each log level shows (debug: per-stage timing and graph sizes; trace:
dependency edges and the full command line of every action) is broken down in
[91-implementation-status.md](91-implementation-status.md).

## `dowel new`

```
dowel new <path> [--lib]
```

Creates a package in a new directory (which must not exist or be empty).
The package name is the last path component and must be a valid identifier
(a letter or `_`, then letters, digits, `_`, `-`).

| Option | Meaning |
|---|---|
| `--lib` | generate a library package instead of an executable |

The default skeleton is a `bin` package (`dowel.toml`, `dowel.build`,
`src/main.c`, `.gitignore`); `--lib` generates a library with a public
header under `include/`, a private `src/`, and a `test` target that
`dowel test` runs as-is. The generated packages are built and executed by
the test suite on every run, so the skeletons cannot silently rot.

## `dowel add`

```
dowel add <path> [--name <n>]
dowel add --git <url> [--rev <rev>] [--name <n>]
```

Run inside a package; declares a dependency in its `dowel.toml`. Two forms:

- **`<path>`** — creates a library package at the sub-path (relative to the
  package root) and appends the matching `[[dependencies]]` entry with a
  `path` source
- **`--git <url>`** — appends a git dependency instead of scaffolding
  anything. The manifest only ever receives a **full 40-digit sha**. An
  explicit 40-digit `--rev` is written as-is; a name (or `HEAD`, when
  `--rev` is omitted) is resolved **once** via `git ls-remote`, and the
  resolved sha is what gets pinned — the same judgment as `dowelup pin`.
  `dowel check` fetches it on first use

| Option | Meaning |
|---|---|
| `--git <url>` | declare a git dependency instead of scaffolding a package |
| `--rev <rev>` | the commit to pin; a non-sha name is resolved once via `git ls-remote` |
| `--name <n>` | the dependency name (default: the last path or URL component) |

The append preserves the existing manifest text untouched — position carries
no meaning for array tables in strict TOML.

What it does not do: wire the dependency into a target. Which target uses it
is a per-target choice, so the command prints the exact
`deps = [dep("<name>")]` line to add instead
([12-build-reference.md](12-build-reference.md)). A name already declared in
`dowel.toml` is refused.

## `dowel check`

```
dowel check [common options]
```

Runs through the planning stage and reports diagnostics only. Nothing is
compiled, linked, or executed. Because it covers glob expansion, path
resolution, and toolchain existence, the configuration diagnostics that
`build` would report also come out of `check` (the scope is set by
[ADR-0010](adr/0010-check-scope.md)). It is faster than a build and intended
to run on every save.

## `dowel build`

```
dowel build [target...] [common options] [build options]
```

Plans the build and hands it to a backend. With no targets named, builds
every `bin` and `test`. Naming accepts `<target>` or `<package>:<target>`.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--backend <name>` | `ninja` / `direct` / `make` / `graph` | `ninja` when available | who runs the build (below) |
| `-j, --jobs <n>` | number | the backend's default | parallelism, passed to the backend |
| `--no-compdb` | — | — | do not write `compile_commands.json` |

The backend is the output stage ([ADR-0018](adr/0018-backend-layer.md)). All
of them receive the same build graph, so which one runs is not supposed to
change what gets built.

| Backend | What it does |
|---|---|
| `ninja` | writes `build.ninja` into the build directory and runs ninja. The default where ninja is on PATH |
| `direct` | runs the steps in process, one at a time, judging freshness by mtime, depfiles, and the command line itself. Needs no external generator. The fallback when ninja is absent |
| `make` | writes `Makefile` and runs `make`. Refuses a build whose paths make cannot name (whitespace, `:`, `#`, `$`, `%`, `;`, `=`, `\`, `*`, `?`, `[`, `]`) rather than writing a makefile that builds something else |
| `graph` | writes `build-graph.json` — the backend-neutral description ([14-build-graph.md](14-build-graph.md)) — and stops. Nothing is compiled; the document is for a tool of your own. `dowel test` and `dowel inspect` refuse it |

`--executor`, the previous spelling, is refused with a message naming
`--backend`: the set of values it takes has changed.

- Build directories are separated per configuration, under `.dowel/`.
  Executables land in its `bin/` (`./.dowel/build/*/bin/<name>`)
- The compiler comes from `[toolchain]` in `dowel.toml` (default: `cc` on
  PATH). Toolchain fetching is not implemented; whatever is named must be on
  PATH
- The toolchain is selected by the target triple. `--target=<triple>`
  requires a `[toolchain.<triple>]` declaration
  ([11-toml-reference.md](11-toml-reference.md)); a triple with none is
  refused before building with `missing-toolchain` rather than silently
  building host artifacts under that triple's name

## `dowel test`

```
dowel test [target...] [common options] [build options] [test options]
```

Builds the `test` targets, runs them, and judges pass/fail by exit status
(0 = success). No test harness is imposed; the C convention applies. The
working directory is the package root unless a case declares `cwd`. By
default only the output of failing tests is shown.

A target may register several tests from one binary with
`[test.<name>.cases]` ([12-build-reference.md](12-build-reference.md),
[ADR-0022](adr/0022-test-cases.md)), each with its own arguments,
environment, timeout, expected verdict, and labels. Alternatively
`[test.<name>.harness]` asks the binary itself to list its cases
([ADR-0023](adr/0023-harness-protocol.md)); a listing that fails or returns
nothing is reported as a failure of that target rather than as zero tests. Every option below then
operates on **cases**, not targets: a case's label is
`<package>:<target>/<case>`. A target with no cases is one test named after
the target.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--label <a,b>` | names | — | run only tests carrying one of these labels (declared in `[test.<name>.cases]`) |
| `--no-run` | — | — | build, then **list** what would run instead of running it |
| `--nocapture` | — | — | pass test output through |
| `--fail-fast` | — | keep going | stop at the first failure; the summary reports how many were not run |
| `--failed` | — | — | rerun only what failed last time; verdicts persist in the build directory, and verdicts of targets not run are kept |
| `--debug-failed` | — | — | open the failing test under the debugger instead of rerunning it |
| `--test-jobs <n>` | number | 1 (sequential) | how many tests run at once; display is always in request order |

- The default is sequential because C tests may use shared resources (working
  directory, fixed ports, output files). A case that declares its own `cwd`
  removes the first of those
- When `--target=<triple>` differs from the host, launch goes through the
  declared runner (`[runner.<triple>]` in
  [12-build-reference.md](12-build-reference.md)). If no runner is declared,
  the launch is refused with a diagnostic beforehand
- A positional argument names either a target (`app:unit`) or a **case**
  (`app:unit/parse`) — the same string the summary and the JSON output print,
  so a failing case can be rerun on its own (issue #93). Naming a target runs
  all of its cases
- **A selection that matches nothing fails.** `--label` with a name nobody
  carries, a case that does not exist, or `--failed` whose remembered cases
  are gone all exit nonzero and say so (issues #89 / #91). The report goes to
  stderr, where a CI log buries it, so the exit status has to carry it:
  otherwise a mistyped `--label` is a green step that ran nothing. Two cases
  are **not** failures, because neither contradicts what was asked: a tree
  with no `test` targets at all, and `--failed` when nothing failed last time
- `--no-run` builds and then lists the cases that would run, after the
  selection is applied, with their labels, `should_fail`, and `timeout`
  (issue #94). It is the only way to see what exists without running it, and
  it is what makes `--label` usable — the labels have to be discoverable
  somewhere. With `--message-format=json` each case is one `test-case` line
  carrying the same `target` / `case` / `label` fields a result would.
  Nothing is launched, so a cross target needs no runner; the exception is a
  `harness` target, where listing means asking the binary
- A case with a `timeout` is killed when it expires and reported as timed
  out, whatever exit status the kill produced. The kill reaches the test
  process only — a test that spawns grandchildren leaks them
- `--debug-failed` joins the test job list with the debug launch
  (docs/30-devexp.md section 2.3, [ADR-0024](adr/0024-debug-command.md)).
  The failing case reopens under the toolchain's debugger with its declared
  `args`, `env`, and `cwd` — nothing is copied by hand. It reads the same
  record as `--failed` and narrows the same way (a positional label,
  `--label`). The selection has to come to **exactly one** case: a debugger
  attaches to one process, and picking silently would leave the user
  guessing which one opened. Several failures are listed with a note to
  name one; none is a success that says so. `--dap` writes the launch
  configuration instead of starting the debugger, with the case's arguments
  and environment in it; `--no-run` is refused, since one flag says "do not
  run" and the other reruns. The verdict record is not updated — the
  debugger session is interactive, not a judgment
- A case killed by a signal fails, including one that declared
  `should_fail`: what that declares is a nonzero **exit**, and a crash is not
  one (issue #88). The line says which signal it was
- `--message-format=json` emits one `test-result` line per case on stdout.
  The target and the case are separate fields, so nothing downstream has to
  split a string to group results by target (issue #100):

  ```json
  {"kind":"test-result","target":"c:suite","case":"parse","label":"c:suite/parse",
   "labels":["slow"],"should_fail":false,"timeout":null,
   "binary":"…/bin/suite","args":["parse"],"passed":true,
   "timed_out":false,"exit_status":0,"signal":null,
   "duration_ms":1,"stdout":"…","stderr":"","launch_error":null}
  ```

  `case` is `null` and `label` equals `target` for a target with no cases.
  There are three separate ways to end without an exit status, and each has
  its own field: `timed_out` (dowel killed it), `signal` (it died on its
  own), and `launch_error` (it never started). `args` says which invocation
  of the binary this was

## `dowel bench`

```
dowel bench [target...] [common options] [build options] [--iterations <n>]
```

Builds the `bench` targets and measures the **wall-clock time of the whole
process**, start to exit, reporting min and median over the requested number
of runs ([ADR-0025](adr/0025-bench-wall-clock.md)). No benchmarking
framework is imposed, and none is read: there is no C convention for
measurement output, and parsing one format per framework is the
entanglement the ADR refuses. The process-level number is the same
yardstick for every binary.

```
bench b:spin/small ... min 1.02ms  median 1.15ms  (10 runs)
```

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--iterations <n>` | number | 10 | runs per benchmark. min and median are computed over them |

- `[bench.<name>.cases]` registers several measurements of one binary,
  distinguished by arguments — the same shape as test cases
  ([12-build-reference.md](12-build-reference.md)), minus `should_fail`. A
  positional argument names a target (`b:spin`) or a case (`b:spin/small`)
- Runs are always sequential. Measurement assumes a quiet machine; two
  benchmarks in parallel are each other's noise, so there is deliberately
  no `--bench-jobs`
- **Speed has no verdict.** `dowel bench` fails only when a run could not
  be completed — nonzero exit, signal, a case's `timeout`, launch failure —
  and then reports no numbers at all: statistics over a partial series read
  as a finished measurement. Thresholds and regression gates are downstream
  policy, applied to the JSON
- `--message-format=json` emits one `bench-result` line per measurement,
  with `target` / `case` / `label` fields as in `test-result`. Times are
  **integer microseconds** (`min_us` / `median_us` / `max_us`); rendering
  fractional milliseconds is the reader's formatting decision
- min approximates what the code does when the machine does not interfere;
  median, what a user sees. The mean follows outliers and is not reported
- Cross execution measures the runner too (qemu's translation, ssh's round
  trip): honest as "how long does this take here", meaningless as hardware
  time

## `dowel debug`

```
dowel debug <target>[/<case>] [common options] [build options] [--dap]
```

Builds the target and starts a debugger on its artifact, with the package
root as the working directory ([ADR-0024](adr/0024-debug-command.md)).
Only `bin`, `test`, and `bench` targets can be debugged — a library has
nothing to start (`not-debuggable`).

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--dap` | — | — | write a DAP launch configuration to stdout and start nothing, so an editor reproduces the same session |

- The positional argument names a target (`app:unit`) or a **case**
  (`app:unit/parse`), resolved exactly as `dowel test` resolves it. Naming a
  case carries its declaration into the session — `args`, `env`, `cwd`, and
  for a harness target the `run` arguments and the discovered name — so
  nothing has to be copied by hand (issue #110). Cases are reached whether
  or not they have ever failed: a passing case worth stepping through, one
  about to be written, or one whose failure was recorded under a different
  configuration are all ordinary reasons to open a debugger.
  `dowel test --debug-failed` remains a separate selection ("open what
  failed last time"), and both are wanted
- Naming a case on a target that has none says so; a name that does not
  exist lists the ones that do. A harness target is asked to list its cases
  here too, so one extra process runs
- The debugger is a toolchain tool: `debug` in `[toolchain]` /
  `[toolchain.<triple>]`, defaulting to `gdb`. A cross build therefore names
  its own (`debug = "riscv64-linux-gnu-gdb"`) the same way it names its
  compiler. It is probed only here, so a project that never debugs needs no
  gdb; missing from PATH is `missing-toolchain`
- Debugging an artifact built for **another** triple needs a stub. The
  runner declares both how to host the program and where to attach:

  ```toml
  [runner.riscv64gc-unknown-linux-gnu]
  command       = "qemu-riscv64"
  args          = ["-L", "/usr/riscv64-linux-gnu"]
  debug_args    = ["-g", "1234"]         # these turn the runner into a stub
  debug_connect = "localhost:1234"       # this is where the debugger attaches
  ```

  The port appears twice because dowel does not parse the runner's flags and
  can derive neither from the other. A cross target whose runner declares
  neither is refused with `missing-debug-stub` rather than pointing a host
  gdb at a foreign binary, and one that declares only **half** is told which
  half is missing rather than being called empty (issue #109)
- `debug_args` is inserted **before** the runner's own `args`, giving
  `<command> <debug_args...> <args...> <artifact>`. It cannot go after.
  A runner's `args` may end with the flag that takes the artifact
  (`args = [..., "-kernel"]`, the shape [ADR-0008](adr/0008-runner-transfer.md)
  asks for), and anything inserted between that flag and the artifact is
  eaten as its operand (issue #107). No debugging flag changes meaning by
  being earlier — option order is free as long as adjacent pairs stay
  together
- The stub is started before the debugger and killed when it exits
- No `substitute-path` is emitted: dowel does not pass `-ffile-prefix-map`,
  so there is no mapping to compensate for, and emitting one would be a
  fiction (docs/30-devexp.md section 2.1)

## `dowel inspect`

```
dowel inspect [target...] [common options] [build options]
```

Builds, then runs the tools declared in `[<kind>.<name>.inspect]`
([12-build-reference.md](12-build-reference.md)) and passes what they report
through. With no target, inspects every target that declares an inspection;
a named target with none simply reports nothing.

An inspection produces no file, which is why it is a command rather than
part of `build`: there is nothing to be up to date about, so running it on
every build would be noise and running it never would make the declaration
pointless. The tool's output is not parsed — `size`'s format differs between
implementations, and reading it is the tool's job.

- Tool output goes to **stdout**, the `== <target>: <name> (<tool>) ==`
  headings to stderr, so `dowel inspect > sizes.txt` keeps just the reports
- A tool exiting nonzero fails the run. A budget check is expressible today
  as a wrapper script that exits nonzero when over
- `--message-format=json` emits one object per inspection per line, carrying
  the full command, the exit verdict, and the output

## `dowel why`

```
dowel why <target> <property> [--format <text|json>]
```

Shows the path a value took to reach the target, down to its origin, with
source locations.

```
$ dowel why app:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

| Option | Values | Default |
|---|---|---|
| `--format <fmt>` | `text` / `json` | `text` |

## `dowel graph`

```
dowel graph [--kind <target|action>] [--format <text|dot|json>]
```

Dumps a graph to stdout.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--kind <kind>` | `target` / `action` | `target` | target dependency graph / action graph |
| `--format <fmt>` | `text` / `dot` / `json` | `text` | output format; `dot` can be fed straight to Graphviz |

`--kind=action --format=json` prints the build graph document — byte for byte
what `dowel build --backend=graph` writes to a file
([14-build-graph.md](14-build-graph.md)). There is one JSON description of an
action graph, and it is the one the backends run on.

## `dowel migrate verify`

```
dowel migrate verify <compile_commands.json> [--format <text|json>]
```

Compares a reference compile database — what the existing build system
actually does — against dowel's plan, source by source
([13-semantics.md](13-semantics.md); the design is
[40-migration.md](40-migration.md) section 4). Migration becomes a
continuous equivalence check instead of a one-shot conversion: "this target
is ported and produces the same compile arguments" is confirmed
mechanically.

Commands are normalized before comparison, so equivalent-but-differently-
spelled commands match. `-D NAME` / `-DNAME` / `-DNAME=1` are the same
define, `-I` paths are resolved against each entry's `directory`, and the
compiler name, `-c` / `-o`, and depfile flags (`-MD` family) are ignored.
Configuration-level flags (optimization, debug info, `NDEBUG`) are dropped
from **both** sides: dowel's debug/release configuration supplies them on
one side and the reference's build type on the other, so they say nothing
about whether the port is faithful. Remaining flags are compared as a
multiset.

The report has four buckets:

| Bucket | Meaning | Fails the run |
|---|---|---|
| equivalent | same source, same normalized arguments | — |
| differing | same source, different arguments; each difference is listed with its direction | **yes** |
| not ported | sources only in the reference | no (porting is incremental) |
| only in dowel | sources only in dowel's plan (tests, new targets) | no |

Exit status is nonzero only when a ported source differs. `--format=json`
prints the same report as one JSON object on stdout.

## `dowel migrate import`

```
dowel migrate import <old-build-dir>
```

Drafts `dowel.toml` / `dowel.build` from what an existing build system says
about itself, writing them **into its source directory** — next to the code
they describe. Existing manifests are never overwritten.

| Source | Read from | How to produce it |
|---|---|---|
| CMake | File API reply (`codemodel-v2`) | `mkdir -p build/.cmake/api/v1/query && touch build/.cmake/api/v1/query/codemodel-v2`, then re-run `cmake -B build ...` |
| Meson | `build/meson-info/` | `meson setup build <source>` writes it on its own |

Which one it is, is decided by **looking at the directory** — there is no
`--from=` flag. What gets passed is the old build directory, and what made
it is written there. If neither is present, the error names both recipes.

```sh
dowel migrate import build
```

The output is a draft, not a finished artifact
([40-migration.md](40-migration.md) section 3). It is a snapshot of one
configuration, so conditionals are lost, and the public/private intent of
includes and defines is unknowable from what either system reports.
Everything therefore lands in `private` blocks, and sources are listed
explicitly rather than globbed.
Each generated file opens with an **UNVERIFIED DRAFT** header that says so
and points at the follow-up:

```sh
dowel migrate verify <old-build>/compile_commands.json
```

Common mapping: executables → `bin`; static (and, with a note, shared)
libraries → `lib`; includes outside the source tree → `-I` flags. Target
names are mapped to valid identifiers. Configuration-level flags coming from
the build type (`-O` / `-g` / `-DNDEBUG`) are **not** copied — dowel's own
`--config` supplies them, and copying them unconditionally would make a
draft imported from Release produce optimized `NDEBUG` "debug" builds. The
draft header states this.

What differs between the two sources:

- **CMake** reports compile arguments already sorted into `defines`,
  `includes`, and fragments, and it names in-project `dependencies`, which
  become `target(...)`. External `-l...` libraries become `link_flags`
- **Meson** hands over one `parameters` array per target, so dowel does the
  sorting (`-I` → `includes`, `-D` → `defines`, the rest → `flags`). Its
  introspection does **not** say which targets link against which, so
  `deps` is left empty and has to be written by hand — guessing from output
  filenames would put wrong edges in a draft that is already unverified.
  Generated sources are listed as skipped comments rather than dropped
  silently, and subproject targets are not imported (they are a different
  package)

## `dowel schema dump`

```
dowel schema dump
```

Prints the schema and configuration vocabulary to stdout in machine-readable
form: every `kind`, each property's type and merge rule, and the domains of
the configuration keys (`cfg` / `host` / `feature` / `tc`). This is the same
table the language server's hover and diagnostics read; it is not duplicated.
The output is also intended as context for LLM agents
([30-devexp.md](30-devexp.md) section 4).

## `dowel cache`

```
dowel cache info
dowel cache gc
```

| Subcommand | Meaning |
|---|---|
| `info` | report the size and record count of the on-disk store, and of the probe-fact database |
| `gc` | remove stores and fact databases left by older formats |

Neither reads the manifests: cleanup must work even when a manifest is
broken. The store's contents and guarantees are described under "The store"
below.

There are **two** caches, and `info` names both. The store is per-project
(`.dowel/cache/`); the **probe facts** are per-user
(`$XDG_CACHE_HOME/dowel/facts/`, falling back to `~/.cache`) because a fact
about a tool is the same in every project that uses that tool
([ADR-0028](adr/0028-probe-facts.md)). What is recorded there is what dowel
asked a tool and what it answered: the triple a compiler calls itself
(`-dumpmachine`), whether a generator answers `--version`. Keys carry the
tool's path, size, and mtime, so replacing a tool makes the old facts
unreachable rather than wrong; `gc` collects them.

Deleting either by hand is safe — they are caches. The next run asks again.

## `dowel lsp`

```
dowel lsp
```

Speaks LSP on stdin and stdout. The editor is the process that starts it, and
it exits with the editor (it is not a resident daemon —
[ADR-0002](adr/0002-no-daemon.md)). The CLI never depends on the language
server's existence.

- Diagnostics: full-document sync; `publishDiagnostics` in response to
  changes. Cross-file diagnostics (the feature vocabulary, `dep(...)` /
  `target(...)` resolution, merge conflicts, cycles) come from a workspace
  model in which the open buffers overlay the disk. The editor session never
  fetches and never touches the store. Plan-stage checks that scan the file
  system are not produced (`dowel_lsp::UNSUPPORTED` lists them with reasons)
- Hover: property types and merge rules, builtin function signatures,
  configuration key domains
- `dowel.toml` is recognized by name and held to strict TOML validation

The VS Code client lives in [`editors/vscode/`](../editors/vscode/README.md).

## Machine-readable diagnostics

`--message-format=json` emits one JSON diagnostic per line on stdout. Each
diagnostic carries:

- A severity and a stable code (`unknown-property`, …). Codes are a
  compatibility surface
- Source locations (multiple labels) and notes
- Mechanically applicable fix suggestions (span + replacement string)

The list of codes, with the minimal input that produces each, is defined in
the case table of `crates/dowel-cli/tests/diagnostics.rs`.

## The store

Memos are kept under `.dowel/cache/<format-version>/`
([20-architecture.md](20-architecture.md) section 5).

The writer is limited to one process. A process that cannot take the lock
reads only and writes nothing back. Computation completes within the process
either way, so all that is lost is the cached speedup — results never change.
Deleting, truncating, or externally modifying the store likewise changes
nothing but speed.

## Examples

```sh
dowel check --message-format=json
dowel build --config=release
dowel test --failed --fail-fast
dowel why app:app includes
dowel graph --kind=action --format=dot | dot -Tsvg -o actions.svg
DOWEL_LOG=debug dowel build
```

A working example lives at [`examples/hello`](../examples/hello).
`crates/dowel-cli/tests/example.rs` builds and checks it for real, so a
change to syntax or semantics that misses the example is detected.
