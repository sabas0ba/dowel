# Implementation status

[90-roadmap.md](90-roadmap.md) is the phase-level plan. This document records
what is implemented. Where the two disagree, this document describes the
current state.

The command reference is [60-cli.md](60-cli.md); task-oriented how-tos are in
[63-guides.md](63-guides.md).

## Approach: connect a minimal build end to end first

The roadmap orders Phase 1 (core) to completion before Phase 2 (generation),
but the implementation deviated from that order exactly once: parser →
evaluation → target graph → action graph → ninja generation → actual C
compilation were connected first, each in minimal form.

Two reasons:

- e2e verification becomes available from the start. "Compile real C, run it,
  and check the output" exists as a test, and every later change builds on
  that premise
- The constraints that cannot be retrofitted (docs/20-architecture.md
  section 2) get validated early. Lossless CSTs and span retention can only
  be validated once things connect all the way down to action generation

The incremental query engine and the persistent store were then plugged in
afterward. The insertion point is confined to
`dowel_model::session::Session`; both are in place (below).

## Crate layout

| Crate | Responsibility |
|---|---|
| `dowel-support` | spans, source maps, diagnostics, structured logging, JSON output |
| `dowel-syntax` | lexing, lossless CST, error-tolerant parser |
| `dowel-query` | memoization, dependency tracking, early cutoff, durability layers, cancellation |
| `dowel-store` | the on-disk store: append-only value log, fixed-length index, single writer |
| `dowel-eval` | typed values with provenance, expression evaluation, schema and merge semantics, configuration specialization, value serialization |
| `dowel-model` | package loading, targets, the dependency graph, interface merging, `why` |
| `dowel-build` | glob expansion, the action graph, ninja generation, `compile_commands.json`, execution |
| `dowel-lsp` | the language server: JSON-RPC framing, diagnostics publishing, hover |
| `dowel-cli` | the `dowel` binary |
| `dowel-up` | the `dowelup` binary: acquiring, pinning, and switching dowel itself |

Real-world test material lives in `tests/projects/` (realistic fixtures) and
`examples/` (the documented examples).

## Implemented

### Syntax (`dowel-syntax`)

- Lexing in which every byte belongs to exactly one token; whitespace,
  newlines, and comments are retained
- A lossless CST: walking the tree and concatenating reproduces the input
  (checked continuously by tests)
- Error tolerance: syntax errors do not stop parsing; an `Error` node is left
  and parsing continues. Recovery always consumes at least one token,
  guaranteeing loop progress
- Table headers, array tables, key-values, arrays, inline tables, function
  calls, `match`, postfix `when`, namespace references
- Robustness tests: over every prefix of real manifests, single-character
  deletions, and delimiter insertions, no panics and losslessness holds

### Incrementality (`dowel-query`)

- Memoization and dependency tracking; keys read during a query are pushed
  onto the running frame
- Early cutoff: if recomputation fingerprints the same as before, dependents
  are not invalidated
- Durability layers (`Low` / `Medium` / `High`); stable layers skip
  dependency traversal entirely
- Cancellation, checked at query boundaries and propagated via `Result`.
  The release profile is `panic = "abort"`, so unwinding is not used
- Re-setting an input to identical content does not advance the version
  (judged by content, not by mtime)
- Memos retain the computation procedure, so chains of derivations can be
  revalidated without recomputation. As a side effect, procedures can read
  values only through `Db`, and purity is enforced by types

`Session` reads files through this engine (`dowel_model::query`).
`Session::reload` re-reads the disk, but files whose contents did not change
are not re-lexed. The degree of reuse is observable via
`Session::query_stats`.

Early cutoff cannot help on per-file queries in principle (values contain
spans, so any change to the text changes the value). Where it does apply is
the per-target derivations (`interface` and `compile_env`), whose
fingerprints come from span-free summaries
([ADR-0011](adr/0011-cutoff-and-provenance.md)). A comment-only edit never
reaches merging. The path that displays provenance spans (`dowel why`)
bypasses the memo and redoes the merge on the spot.

### Persistence (`dowel-store`)

`.dowel/cache/<format-version>/` holds `lock` / `values` / `index`.

- An append-only value log and a fixed-length record index. The index needs
  no parsing to scan, and value bodies are not read until needed
- Index replacement is a temporary file + `rename`. A same-directory `rename`
  is atomic; readers see either the old or the new
- The writer is restricted to one via `flock`
  (`std::fs::File::try_lock`). A process that cannot take the lock reads
  only and writes nothing
- On load, records pointing outside the value log — and any trailing
  fragment — are discarded. After truncation or a mid-write crash, opening
  never fails
- Input change detection compares `(mtime, size, inode, ctime)` and takes a
  content fingerprint only when they differ
- `dowel cache info` / `dowel cache gc`

`Session` records the files it read into the store, and the next process
checks against that record to judge changes. The verdicts
(`UnchangedByStat` / `UnchangedByContent` / `Changed`) appear at
`--log-level=trace`.

Only evaluation results (`Evaluated`) are stored; per-target derivations stay
in the in-process memo. The reasons are in
[ADR-0012](adr/0012-store-contents.md). Storage is limited to files that
evaluated without emitting a single diagnostic.

Restoration is keyed on a matching content fingerprint. On a match, the file
is neither lexed, parsed, nor evaluated. A per-run summary appears at
`--log-level=debug`:

```
store: wrote 1 values, restored 3, skipped 0 with diagnostics
```

`FileId` is the hash of the normalized path, so the same file has the same
identifier across processes ([ADR-0009](adr/0009-file-identity.md)).
Restoring stored values requires no renumbering pass, and a restored document
carries its own `FileId`, so key collisions are detectable on the value side.

Serialization lives in `dowel_eval::codec`: length-prefixed bytes covering
every type in `Document`. A failed restore returns `None`, and the store
treats unreadable values as absent. Whether the format version mismatches,
the file was truncated, or something external rewrote it — the result never
changes, only the speed.

### Evaluation (`dowel-eval`)

- `Value = { type, data, provenance }`. Provenance is a constituent of the
  value and traceable to the root
- `Path` is a distinct type from `Str`; the language provides no string
  concatenation for building paths
- `Cfg<T>`: resolution of `match` and postfix `when` is deferred to
  specialization, so switching `--release` or `--target` does not re-run
  manifest evaluation
- `glob` is not expanded during evaluation either: scanning at evaluation
  time would mix in the current file system — an unrecorded input
- Merge rules belong to types: `union` / `append` / `error_on_conflict` /
  `must_equal` / `replace`
- Exhaustiveness checking of `match`: closed-domain `cfg` keys require full
  enumeration, and open-domain `cfg.target` requires `_`
- The strictness of `dowel.toml` is imposed by validation, not by a separate
  grammar

### Model (`dowel-model`)

- Loading multiple packages by following `path` dependencies
- Diagnostics for unknown properties and type mismatches against the schema
  (with suggestions)
- The `interface(T)` / `compile_env(T)` split: `private` dependencies affect
  one's own compilation but do not propagate to dependents
- Feature flags make dependency-graph edges appear and disappear
- Optional dependencies (`optional = true`) that are not enabled are not
  loaded — not just the edge but the node is absent. Feature selection is
  fixed before `Session` loads
- Cycle detection (iterative DFS, the path shown in a note)
- `dowel why` — propagation-path display (text / json)

### Build (`dowel-build`)

- `glob` expansion (`*` / `**` / `?`), sorted lexicographically to be
  independent of traversal order
- The action graph (compile / archive / link)
- C and C++. The compiler is chosen per source by extension (C++:
  `.cc` `.cp` `.cpp` `.cxx` `.c++` `.CPP` `.C`; `tc.cxx` defaults to `c++`,
  overridable with `[toolchain] cxx`). A link whose closure contains any C++
  translation unit uses the C++ driver, so the C++ runtime is linked even
  when the binary itself is pure C. The C++ toolchain is only required — and
  only probed — when C++ sources are present
- ninja file generation and `compile_commands.json` (`arguments` array form)
- Two executors: ninja (default) and direct (sequential, mtime-based
  freshness reading depfiles). Header dependency records (`.d` files) stay
  on disk and are shared between the executors — ninja is not allowed to
  fold them into `.ninja_deps` (`deps = gcc`), because a record private to
  one executor makes the other conclude "up to date" with no dependency
  information at all, silently keeping stale artifacts (issue #41). As a
  backstop, the direct executor treats an output whose declared depfile is
  missing as stale instead of fresh
- Per-configuration build directories
- `[toolchain.<triple>]` in `dowel.toml` — toolchain selection follows
  `--target`, the same shape as `[runner.<triple>]` (issue #42). A target
  triple with no declared toolchain is refused before building with
  `missing-toolchain`, next to `missing-runner` in spirit: building host
  artifacts under a foreign triple's name would report the configuration
  mistake one stage later (as a runner's `Invalid ELF image`, or not at all).
  Full toolchain *descriptions* (sysroots, probing) remain Phase 5
- Transfer for `[runner.<triple>]` (`transfer` / `remote_dir` / `host`):
  when the target machine cannot see the build machine's file system,
  artifacts are carried over before launch. Paths are not written in the
  manifest; the implementation appends them
  ([ADR-0008](adr/0008-runner-transfer.md))
- `[runner.<triple>]` — an execution wrapper per target triple.
  `dowel test --target=<triple>` launches through the wrapper transparently.
  If the triple differs from the host and no runner is declared, launch is
  refused with a diagnostic beforehand — afterward it would surface as
  `Exec format error`, reporting a configuration mistake as a test failure
- `dowel test` — launches test targets and judges pass/fail by exit status.
  There is no test harness; the C convention ("exit status 0 means success")
  applies. The working directory is the package root. Only failing tests'
  output is shown
  - `--fail-fast` stops at the first failure. The default keeps going (the
    full picture matters); when cut short, the summary reports how many were
    not run
  - `--failed` reruns only what failed last time. Verdicts persist in the
    build directory; verdicts of targets not run are kept
  - `--test-jobs=<n>` runs tests in parallel. The default is sequential: C
    tests may use shared resources (the same working directory, fixed ports,
    output files), and a parallel default produces order-dependent failures.
    Display is always in request order
  - `--no-run` / `--nocapture`, and `--message-format=json` for one result
    per line

### Language server (`dowel-lsp`)

`dowel lsp` speaks LSP on stdin/stdout. The editor is the starting party,
which distinguishes it from the resident daemon rejected by
[ADR-0002](adr/0002-no-daemon.md).

- Full-document sync: `publishDiagnostics` in response to `didOpen` /
  `didChange` / `didSave` / `didClose`
- Beyond parsing and evaluation, diagnostics include the type checking
  decidable from the single open file (`unknown-property` / `type-mismatch` /
  `unknown-kind`, …), produced by the same implementation as the CLI. A check
  enforces that every code in the case table either reaches the editor or has
  a reason in `dowel_lsp::UNSUPPORTED`
- `textDocument/hover`: property types and merge rules, each level of a table
  header, builtin function signatures, configuration key domains. The source
  is the same table `dowel schema dump` reads
- Diagnostic ranges are 0-based lines and UTF-16 columns; notes and
  fix-suggestion text are folded into the body
- `dowel.toml` is recognized by name and held to strict TOML validation
  ([ADR-0003](adr/0003-manifest-split.md))
- JSON-RPC framing and body reading are in-house
  ([ADR-0007](adr/0007-implementation-language.md)); an unreadable body is
  discarded and the next one read, so one bad message does not drop the
  connection

What it sees is the single open file. Cross-file diagnostics are listed with
reasons in `dowel_lsp::UNSUPPORTED`.

### VS Code extension (`editors/vscode`)

Starts `dowel lsp` and relays diagnostics and hover to the editor, with
syntax highlighting for `dowel.build` (a TextMate grammar). Zero runtime
dependencies; framing and JSON-RPC correlation are in-house (the design is in
`editors/vscode/README.md`). Development happens inside the container via
`editors/vscode/dev.sh`, and the checks include an integration test that
talks to the real `dowel lsp`. It is not yet published to the marketplace.

### Acquisition (`dowel-up`)

`dowelup` acquires dowel itself and pins a version per project
([ADR-0013](adr/0013-self-acquisition.md); usage in
[61-acquisition.md](61-acquisition.md)).

- Resolves specifiers (`stable` / `nightly` / `nightly-<date>` / `X.Y.Z` /
  `branch:` / `tag:` / sha) to a commit sha, builds a mirror checkout with
  `cargo build --release`, and places it under
  `$DOWELUP_HOME/versions/<sha>/`. History and network operations are
  delegated to `git`, building to `cargo`
- Selection via `.dowel-version` (pin) and the default. Launched under the
  name `dowel` it acts as a shim and execs the selected version; a leading
  `+<specifier>` picks directly among installed versions. Selection never
  touches the network
- Pins contain only resolved shas. A hand-written name is not resolved; the
  error points to `dowelup pin`
- `stable` cannot resolve until a release tag appears upstream. Prebuilt
  binary distribution is not started (Q10)

### Diagnostics and logging

- Severity, stable codes, multiple labels, notes, mechanically applicable fix
  suggestions
- Human rendering (rustc format) and `--message-format=json` (one diagnostic
  per line)
- Unknown names get edit-distance suggestions (properties, functions,
  configuration keys, feature names, `match` arms, CLI options and commands)
- The domain of feature names is defined by `[features]` in `dowel.toml`,
  checked both for references from `dowel.build` and for `--features`. The
  judgment is only possible after reading the manifest, so it lives in
  neither evaluation nor argument parsing
- Per-stage timing, dependency-graph edges, and action command lines go to
  the log
- `check` runs through the planning stage
  ([ADR-0010](adr/0010-check-scope.md)): glob expansion, path resolution, and
  toolchain existence cannot be judged during evaluation, and `check` emits
  the same diagnostics `build` would

What `--log-level=trace` shows (the material for tracing "why did this
argument end up like this" when debugging):

| Source | Contents |
|---|---|
| `session` | files read and their sizes, tables and key values evaluated, properties assigned to targets |
| `input` | per-input verdicts against the previous run |
| `query` | input changes and version advancement, files parsed/evaluated, memo validation vs recomputation |
| `graph` | edge resolution, topological order |
| `interface` | per-property counts of arriving values and merge results (both `interface` and `compile_env`) |
| `specialize` | which arm `match` chose, which elements `when` dropped |
| `glob` | files scanned with match/no-match, directories pruned, match counts |
| `plan` | resolved sources, includes, defines, flags; the full command line of every action |
| `exec` | why something was judged fresh; why something re-ran (which input was newer) |
| `test` | the list of tests to launch (before launching), their working directories and commands |
| `runner` | the declared wrappers and the command chosen for the configuration |

## Verification

One entry point; local runs and CI execute the same thing. What each layer
answers, and where a new test belongs, is in [51-testing.md](51-testing.md).

```sh
make verify      # run every stage, leaving results in .work/verify/
```

A mid-run failure does not stop the run; it proceeds to the end and fails
afterward. Results land in `summary.md` (for humans and the GitHub summary),
`results.json` (machine-readable), and `logs/<stage>.log`. CI
(`.github/workflows/verify.yml`) stores these as artifacts and prints the
summary into the job summary. Details in
[50-development.md](50-development.md) section 3.1.

Current breakdown (379 tests):

| Stage | Contents | Count |
|---|---|---|
| `fmt` / `clippy` | formatting check and lints (`-D warnings`) | — |
| `unit-*` | per-crate unit tests | 251 |
| `syntax-robustness` | no panics and losslessness on broken input | 5 |
| `model-integration` | manifest loading through interface merging | 10 |
| `model-incremental` | counting what a reload did not recompute | 10 |
| `e2e` | compile real C and C++, run it, check the output | 45 |
| `scenario` | operation sequences over time (edit and rebuild, configuration switches, cross-process change detection and restore) | 24 |
| `fixture` | real-shaped projects (`tests/projects/`) end to end | 11 |
| `diagnostics` | diagnostics reaching the CLI (45 cases), applying fix suggestions, location presence, `check` scope, coverage tracking | 12 |
| `example` | build the real `examples/hello` and run its tests | 3 |
| `up` | `dowelup` resolution, acquisition, and switching against an upstream fixture | 3 |
| `docs` | link resolution and index consistency | 5 |
| `startup` | startup-time measurement (informational; machine noise does not fail the run) | — |

The `scenario` / `fixture` / `diagnostics` layers were added later. Their
first runs surfaced the following four defects, none of which could appear in
the pre-existing layers:

| Defect | Why the existing layers could not catch it |
|---|---|
| merging deduplicated by relative path only, dropping another package's `include/` once dependencies exceeded two levels | the synthetic project has only one dependency level |
| the direct executor omitted the command line from freshness, missing flag changes | a single run never sees the second execution |
| a directory in `sources` surfaced as the linker's `input file unused` | `invalid-source` had never been reached |
| a nonexistent source surfaced as ninja's `no known rule` | `unresolved-path` had never been reached |

## Measurements

The startup budget is under 10ms with nothing to do
(docs/20-architecture.md 5.4). Release build, a 2-package / 2-target
configuration, min/median of 20 runs. `make measure` produces these on their
own.

| Run | Min | Median |
|---|---|---|
| `dowel --version` | 1.6ms | 1.7ms |
| `dowel check` | 2.3ms | 2.5ms |
| `dowel graph --format=json` | 2.0ms | 2.2ms |

Binary 1.2MB; 4 dynamic links (libc and friends). Currently inside budget.
The previous figures (`--version` 1.2/1.4ms, `check` 1.5/1.7ms, `graph`
1.4/1.6ms) were taken on a different machine and are not directly
comparable. The same-machine change in `check` is recorded in
[ADR-0010](adr/0010-check-scope.md).

### The effect of storing evaluation results

`check` min/median measured on one machine before and after
[ADR-0012](adr/0012-store-contents.md), separated into runs without manifest
changes (where restores happen) and with changes (where stores happen).

| Subject | unchanged, before | unchanged, after | changed, before | changed, after |
|---|---|---|---|---|
| `examples/hello` | 2.35/2.70ms | 2.24/2.44ms | 2.36/2.53ms | 5.21/7.22ms |
| `tests/projects/layered` | 3.48/3.71ms | 3.28/3.60ms | 3.70/4.22ms | 7.48/9.35ms |

At the current fixture sizes, restoring saves 0.1–0.2ms: manifests are a few
hundred bytes, and lexing + parsing + evaluation together are small next to
the fixed startup cost.

Runs that store gain 3–5ms, all of it the two `sync_data` calls in
`Writer::commit` (measuring without sync makes the increase vanish). The cost
falls only on runs that changed a manifest — the runs that proceed to a
build anyway. Unchanged runs have nothing to write and skip the sync.

The savings scale with size; the sync cost does not. The break-even size
cannot be measured with the current fixtures; the scale fixture
([51-testing.md](51-testing.md), "Future") is needed.

## Not implemented (deliberately deferred)

| Item | Standing |
|---|---|
| mmap-ing the index (currently read whole) | Phase 1; reading whole suffices up to thousands of records |
| making loading and name resolution queries (`Declared` / `Deps` as derivations) | Phase 1; today `Session` assembles them and passes them as inputs |
| the probe-fact DB | Phase 2 |
| the `bench` / `template` / `toolchain` kinds | Phase 2 / 4 |
| migration (`migrate verify` / `import`) | Phase 3 |
| `dowel debug` | Phase 4 |
| cross-file language-server diagnostics | Phase 4; per-file diagnostics are implemented (`dowel_lsp::UNSUPPORTED`) |
| cleaning up artifacts left on target machines; skipping redundant transfers | Phase 4; transfers run every time |
| dependency fetching (registry / git / tarball), `dowel.lock` | Phase 5; today only `path` dependencies |
| prebuilt acquisition for `dowelup` | Q10; today source builds only |
| automatic ABI label computation | Phase 6; today only `must_equal` verification of a hand-written `abi` |
| per-language flags (`cxx_flags`, C++ standard selection) | undecided; today `flags` applies to both C and C++ translation units |

## Divergences from the design documents

Points where the implementation departed from the documents, made explicit.
Whether to amend the documents is decided separately.

| Where | Document | Implementation | Reason |
|---|---|---|---|
| the consequence in [ADR-0003](adr/0003-manifest-split.md) | "there will be two parsers" | one parser; `dowel.toml` strictness is imposed by validation | the ADR's rationale (third-party tools read it without a custom parser) is equally satisfied by validation, and a single tree keeps provenance and diagnostics paths simpler |
| types | `defines : Map<Ident, Val>` | `Val` implemented as a type | the document's notation was adopted as-is |
| `abi` | ABI labels are computed | currently a hand-written string | computation is Phase 6; only the `must_equal` path is wired up |
| [30-devexp.md](30-devexp.md) section 1 | `args = ["-L", sysroot()]` | `args : List<Str>`; `sysroot()` cannot be written | sysroot-based paths are Phase 4 (`unimplemented-path-base`); strings work first, widening to `List<Val>` when bases land |
| [50-development.md](50-development.md) section 3 | CI runs in a `--network none` container built from dotfiles | GitHub Actions runners (staying so for now) | the path for evaluating the dotfiles flake from this repository's CI is not set up, and there is no present need. The checks are defined solely in `scripts/verify.sh`, so a migration later swaps only the workflow's internals |
