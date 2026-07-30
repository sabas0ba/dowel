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
  anything. The manifest only ever receives a **full 40-digit sha**: an
  explicit 40-digit `--rev` is written as-is, while a name (or, with `--rev`
  omitted, `HEAD`) is resolved **once** via `git ls-remote` and the
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

Generates ninja files and runs them. With no targets named, builds every
`bin` and `test`. Naming accepts `<target>` or `<package>:<target>`.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--executor <name>` | `ninja` / `direct` | `ninja` when available | executor; `direct` runs sequentially (mtime-based freshness reading depfiles) |
| `-j, --jobs <n>` | number | ninja's default | parallelism, passed to ninja |
| `--no-compdb` | — | — | do not write `compile_commands.json` |

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
dowel test [target...] [common options] [test options]
```

Builds the `test` targets, runs them, and judges pass/fail by exit status
(0 = success). No test harness is imposed; the C convention applies. The
working directory is the package root. By default only the output of failing
tests is shown.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `--no-run` | — | — | build only; do not run |
| `--nocapture` | — | — | pass test output through |
| `--fail-fast` | — | keep going | stop at the first failure; the summary reports how many were not run |
| `--failed` | — | — | rerun only what failed last time; verdicts persist in the build directory, and verdicts of targets not run are kept |
| `--test-jobs <n>` | number | 1 (sequential) | how many tests run at once; display is always in request order |

- The default is sequential because C tests may use shared resources (working
  directory, fixed ports, output files)
- When `--target=<triple>` differs from the host, launch goes through the
  declared runner (`[runner.<triple>]` in
  [12-build-reference.md](12-build-reference.md)). If no runner is declared,
  the launch is refused with a diagnostic beforehand
- `--message-format=json` emits one result per line on stdout

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
| `info` | report the size and record count of the on-disk store |
| `gc` | remove stores left by older formats |

Neither reads the manifests: cleanup must work even when a manifest is
broken. The store's contents and guarantees are described under "The store"
below.

## `dowel lsp`

```
dowel lsp
```

Speaks LSP on stdin and stdout. The editor is the process that starts it, and
it exits with the editor (it is not a resident daemon —
[ADR-0002](adr/0002-no-daemon.md)). The CLI never depends on the language
server's existence.

- Diagnostics: full-document sync; `publishDiagnostics` in response to
  changes. The unit is the single open file; cross-file diagnostics are not
  produced yet (`dowel_lsp::UNSUPPORTED` lists them with reasons)
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
