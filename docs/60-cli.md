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
| stdout | Artifacts: JSON diagnostics, graphs, the schema, `why` and `status` results |
| stderr | Progress and logs |

Because of this split, `dowel graph --format=dot | dot -Tsvg` works at any
log level.

Progress is **output, not a log** ([ADR-0057](adr/0057-progress-is-shown-while-it-runs.md)):
one line per step, written while the build runs rather than collected and
replayed at the end, and shown at every log level except `off`. The line's
shape is `<description>`, preceded by `[n/m]` where the backend running the
steps supplies a count — `direct` counts the steps it has run out of the
steps in the graph, ninja supplies its own, and make supplies none.

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
| `--offline` | flag | off | do not touch the network; use only what is already fetched, and report `needs-fetch` for anything missing ([ADR-0045](adr/0045-offline.md)). `DOWEL_OFFLINE=1` does the same |
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

It takes **no target** — it checks everything, and a name passed to it is a
usage error rather than a silently ignored argument. What it checks is every
target that produces an artifact; a `template` produces none, so declaring
one does not make `check` fail (issue #141).

## `dowel fetch`

```
dowel fetch [common options]
```

Acquires every dependency and toolchain the build needs, and stops
([ADR-0045](adr/0045-offline.md)):

```
$ dowel fetch
ready: dep at /w/.dowel/deps/dep-1d726e00c095
ready: toolchain aarch64-unknown-linux-gnu at ~/.cache/dowel/toolchains/7e5b1e042540
fetched 1 package(s), 1 toolchain(s); the build can now run with --offline
```

Nothing is compiled. Acquisition already happens while the model loads
(dependencies) and while the configuration is assembled (the toolchain);
this command is those two steps without the build, so "ready to go offline"
is something you can see rather than infer. **Both** are listed and counted:
a cross tree usually fetches nothing but its toolchain, and a lone
"0 package(s)" reads as "nothing was needed" (issue #159). It takes no
target.

## `dowel build`

```
dowel build [target...] [common options] [build options]
```

Plans the build and hands it to a backend. With no targets named, builds
every `bin` and `test`. Naming accepts `<target>` or `<package>:<target>`.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--backend <name>` | `ninja` / `direct` / `make` / `graph` | `ninja` when available | who runs the build (below) |
| `-j, --jobs <n>` | number | the backend's default | how many steps run at once, passed to the backend. For `direct` the default is the machine's available parallelism ([ADR-0056](adr/0056-direct-backend-parallelism.md)) |
| `--no-compdb` | — | — | do not write `compile_commands.json` |

The backend is the output stage ([ADR-0018](adr/0018-backend-layer.md)). All
of them receive the same build graph, so which one runs is not supposed to
change what gets built.

| Backend | What it does |
|---|---|
| `ninja` | writes `build.ninja` into the build directory and runs ninja. The default where ninja is on PATH |
| `direct` | runs the steps in process, judging freshness by mtime, depfiles, and the command line itself. Needs no external generator. The fallback when ninja is absent. It runs `--jobs` steps at once, ordered by both the graph's edges and the files the steps read ([ADR-0056](adr/0056-direct-backend-parallelism.md)) |
| `make` | writes `Makefile` and runs `make`. Refuses a build whose paths make cannot name (whitespace, `:`, `#`, `$`, `%`, `;`, `=`, `\`, `*`, `?`, `[`, `]`) rather than writing a makefile that builds something else |
| `graph` | writes `build-graph.json` — the backend-neutral description ([14-build-graph.md](14-build-graph.md)) — and stops. Nothing is compiled; the document is for a tool of your own. `dowel test` and `dowel inspect` refuse it |

Neither `ninja` nor `make` can put a **line terminator** inside a command —
both spell one step as one line — and both refuse such a step rather than
rewriting it ([ADR-0058](adr/0058-a-command-a-backend-cannot-spell.md)).
`direct` passes `argv` to the program and has no such limit. Usually the fix
is in the manifest: `"printf 'a\nb'"` puts a real newline in the argument,
while `"printf 'a\\nb'"` passes the two characters `\n` for `printf` to
expand, which every backend can spell. The diagnostic says so.

`--executor`, the previous spelling, is refused with a message naming
`--backend`: the set of values it takes has changed.

- Build directories are separated per configuration, under `.dowel/`.
  Executables land in its `bin/` (`./.dowel/build/*/bin/<name>`)
- The compiler comes from `[toolchain]` in `dowel.toml` (default: `cc` on
  PATH). A toolchain table that declares `url` + `sha256` is fetched once
  into the user's cache and its tools are found inside it
  ([ADR-0044](adr/0044-toolchain-acquisition.md)); otherwise whatever is
  named must already be on PATH
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
- An artifact is **transferred once per destination**
  ([ADR-0046](adr/0046-transfer-once.md)). The fingerprint of what was sent
  is recorded in the build directory; a run that could not start drops it, so
  the next one sends again. Artifacts are left on the target machine — the
  alternative undoes the skip
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

## `dowel install`

```
dowel install [target...] [common options] [build options]
              --prefix <dir> [--destdir <dir>]
```

Builds, then **copies** the products under `<prefix>`
([ADR-0041](adr/0041-install.md)):

```
$ dowel install --prefix=/opt/myapp
installed: /opt/myapp/bin/app
installed: /opt/myapp/include/core.h
installed: /opt/myapp/lib/libcore.so
installed: /opt/myapp/lib/libcore.so.2
```

- `bin` targets land in `bin/`, `lib` targets in `lib/`. `test` and `bench`
  are not installed — they check the thing rather than being it. Naming
  targets overrides that default
- A library brings the contents of its own `public.includes` directories
  under `include/`. That block is the declaration that says a consumer
  compiles against those directories. The directory goes whole and
  unfiltered; when it also holds files dowel compiles, that is
  `source-among-headers`, pointing at the declaration
  ([ADR-0059](adr/0059-an-interface-directory-holds-the-interface.md)) —
  a warning, because a header-only library may `#include` a `.c` and dowel
  does not guess which files are the interface
- A versioned shared library brings its unversioned name as a symlink
  ([ADR-0040](adr/0040-shared-library-version.md)), and shared libraries a
  installed executable needs are copied too, including from other packages
- Each installed `lib` also gets `lib/pkgconfig/<name>.pc`
  ([ADR-0043](adr/0043-pkgconfig-generation.md)), so a consumer that knows
  nothing about dowel can build against it:
  `cc main.c $(pkg-config --cflags --libs core)`. Its contents are the
  target's `public` block in another notation, and `prefix` is the real
  prefix even under `--destdir`. A library that sits on a sibling of the
  same package names it in `Requires`, in link order — a static archive
  cannot carry its own link requirements, so without that a consumer using
  pkg-config alone gets undefined references (issue #156)
- After the copy, each installed header is **preprocessed against the
  installed `include/` alone**, the way a consumer would
  ([ADR-0060](adr/0060-the-surface-is-readable.md)). One that reaches a
  header which was not installed is `unreadable-surface`, quoting the
  compiler's own complaint — inside the build tree it compiles, because the
  private include path is there too, so nothing else would catch it
- Nothing is rebuilt: what was tested and what is shipped are the same bytes

Installed executables find their libraries **relative to themselves**, so
the prefix can be moved and the build tree deleted. This works because every
artifact linking a shared library records `$ORIGIN/../lib` (`@loader_path`
on macOS) beside the build-tree path.

| Option | Meaning |
|---|---|
| `--prefix <dir>` | Where to install. **Required** — `/usr/local` needs root, and a writable default would be a directory nobody wants |
| `--destdir <dir>` | Prepend this to every destination, for staging a package. `--prefix=/usr --destdir=/tmp/pkg` writes `/tmp/pkg/usr/...`. The recorded search paths are relative, so a staged tree and a final one behave the same |

There is no uninstall and no record of what was written. Installing into an
empty `--destdir` gives the file list.

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

## `dowel status`

```
dowel status [target...] [--format <text|json>]
```

Reports what a build would do, without doing it
([ADR-0061](adr/0061-the-state-is-a-question.md)). It plans exactly as
`check` does, then reads the build directory. Nothing is written, nothing is
started, and no backend is consulted.

```
$ dowel status
evaluation  1 recomputed, 8 unchanged after recomputing, 14 reused, 0 skipped
steps       4 planned, 3 would run

would run
  CC obj/app/core/src_core.c.o  src/core.c is newer than the output
  AR lib/libcore.a              obj/app/core/src_core.c.o is rewritten by an earlier step
  LINK bin/app                  lib/libcore.a is rewritten by an earlier step

up to date
  CC obj/app/app/src_main.c.o
```

Two stages, because two different machines decide them. The first line is the
manifest evaluation: how much of the previous run's work the query layer
reused, and how much it recomputed. The rest is the action graph: which steps
would run, and why each one.

The reasons a step would run:

| Reason | Meaning |
|---|---|
| `no record of a previous run for this output` | nothing has built this output in this build directory |
| `the command changed since the last run` | the same output, a different command line — a flag change no timestamp shows |
| `output missing <path>` | a declared output is not there |
| `no dependency record (<path> is missing)` | a depfile was declared and is not there, so the header dependencies are unknown ([ADR-0027](adr/0027-toolchain-style.md)) |
| `input missing <path>` | an input is gone |
| `<path> is newer than the output` | the ordinary case |
| `the tool changed (<path> is rewritten)` | the tool's identity stamp no longer matches the tool ([ADR-0055](adr/0055-tool-identity-in-freshness.md)) |
| `<path> is rewritten by an earlier step` | a step that runs first overwrites this input |

The judgment is the same function the direct backend calls just before running
a step, so what this reports is what that backend acts on. ninja and make
judge for themselves; in the ordinary case they agree, since all three read
the same file times and the command log is dowel's for every backend.

| Option | Values | Default |
|---|---|---|
| `--format <fmt>` | `text` / `json` | `text` |

`--format=json` carries the same two stages, so a CI job can assert on how
many steps would run rather than parsing build output.

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

Every drafted target also carries `unverified = true`
([ADR-0053](adr/0053-unverified-import.md)). That mark is machine-readable:
`check` reports each one as an `unverified-import` warning, and
`migrate verify` counts the targets still marked beside its verdict, so how
much is left to port is a number rather than a memory. It gates nothing and
suppresses nothing — a draft is checked exactly like anything else — and
only a person removes the line, since clearing it is the claim "I checked
this" and `verify` compares compile arguments without ever running the link.

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
  sorting: `-I` → `includes`, `-D` → `defines`, `-Wl,` / `-l` / `-L` →
  `link_flags`, other flags → `flags`. **The array also carries link
  inputs** — the archives a target linked, and the `ar` argument string of
  a static library (`csrDT`). Those are not compile flags: `cc` would read
  them as input files and the draft would not build, so they are dropped
  and listed as comments naming what they were (issue #135). Its
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
dowel cache gc [--older-than=<days>]
```

| Subcommand | Meaning |
|---|---|
| `info` | report the store's size, record count, and dead bytes; every build directory with its size and age; and the probe-fact database |
| `gc` | remove stores and fact databases left by older formats, then compact the current store. `--older-than=<days>` also removes build directories not written in that long |

Neither reads the manifests: cleanup must work even when a manifest is
broken. The store's contents and guarantees are described under "The store"
below.

Two things grow ([ADR-0037](adr/0037-store-gc.md)). The value log is
append-only, so overwriting a key leaves the old bytes in place; `info`
reports them as `dead`. The **build directories** are per configuration —
one per (triple, configuration) pair — and switching between debug and
release leaves the previous one behind with its objects in it. That is the
larger number.

**Growth is reported by default.** When a run ends and the store is over
budget, one line says so and how to collect it:

```
note: the store holds 8402 bytes no longer reachable; `dowel cache gc` frees them
note: set DOWEL_CACHE=gc to collect it automatically, or =off to stop saying this
```

The budget is the live bytes themselves: over budget means the dead exceed
the live. A tree's live size is what that tree needs, so the threshold
scales with the project rather than being a number that fits one
repository.

| `DOWEL_CACHE` | Behavior |
|---|---|
| `notify` (default) | report when over budget; collect nothing |
| `gc` | report and compact in place |
| `off` | say nothing |

The default reports rather than collects because compaction rewrites a
file, and a build that pauses to do so spends time its user did not ask
for.

`gc --older-than=<days>` removes build directories not **written** in that
long — a configuration nobody has built in a month is one nobody is using,
and its contents regenerate. Without a number, `gc` does not touch them:
"everything but the current one" would delete the release tree of someone
who alternates between two configurations daily.

Per-record ages are not recorded. Evicting individual entries by age would
mean writing on every read to maintain a last-used time, to manage what the
whole-store budget already covers.

`gc` takes the writer lock, so it does not run against a concurrent build;
it says so rather than waiting.

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
