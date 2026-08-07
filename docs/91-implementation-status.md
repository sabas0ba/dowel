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
| `dowel-build` | glob expansion, the action graph, the backend layer (ninja / direct / make / graph), `compile_commands.json`, execution |
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
  `must_equal` / `replace` / `max`
- Features are additive, and exclusivity is **declared**
  ([ADR-0021](adr/0021-exclusive-features.md)). `[features] exclusive`
  takes sets of features that must not be active together; two or more of a
  group active for a package is `conflicting-features`, naming them and
  where each came from (`default` is the one that gets forgotten, so it is
  called out with `--no-default-features`). Nothing is inferred — dowel
  cannot see that two sources define the same symbol. Without it, choosing
  an implementation with stacked `when`s compiles both: a `bin` fails with
  the linker's `multiple definition`, and a `lib` **succeeds**, keeping
  whichever archive member the linker reached first (issue #82)
- `pkg.name` / `pkg.version` are package constants readable in value position
  ([ADR-0020](adr/0020-package-constants.md)). They are the only namespace
  that can appear as a value, and the only one refused as a `match`
  scrutinee — a package's own version is not an axis a build varies along.
  They resolve at specialization, not at evaluation: evaluation results are
  stored keyed by file content, and a `dowel.build` does not change when its
  `dowel.toml`'s version does, so substituting earlier would keep a stale
  version in the store (issue #80)
- A `defines` value's type decides its `-D` form: a `Str` becomes a C string
  literal, an `Int` or `Bool` a bare token. A version arriving as a bare
  `0.4.0` could not be passed to `%s`, which would leave `pkg.version`
  unusable for the case it exists for
- An `abi` label may name a boundary instead of a language: `c` matches every
  label and never replaces one ([ADR-0019](adr/0019-c-abi-label.md)). Without
  it, a C library and a C++ consumer each stating its own language honestly
  produce different labels and the build is refused, and the way out is for
  the consumer to copy the library's label — at which point the label stops
  describing an ABI (issue #78). The exemption belongs to the ABI label
  vocabulary, not to `must_equal`, which still means equality everywhere else
- Exhaustiveness checking of `match`: closed-domain `cfg` keys require full
  enumeration, and open-domain `cfg.target` requires `_`
- The strictness of `dowel.toml` is imposed by validation, not by a separate
  grammar. Its top-level tables are a closed set: an unknown one is
  `unknown-table`, and a name from `dowel.build`'s vocabulary is told where
  it belongs rather than skipped (issue #74). `[policy]` remains accepted as
  documented-reserved

### Model (`dowel-model`)

- Loading multiple packages by following `path`, `git`, and `version`
  dependencies. An entry names **exactly one** source: none is
  `incomplete-dependency`, two or more is `conflicting-dependency-source`.
  The second half is what the rule was missing — one of the declarations
  would never be read, and nothing said which one won, so a `path` left
  behind while switching to `git` kept building for whoever still had the
  tree (issue #79)
- git dependencies are pinned to a full 40-digit commit sha (anything else
  is `unpinned-dependency`) and fetched once into
  `.dowel/deps/<name>-<rev12>/` by delegating to the `git` command; the
  checkout is placed atomically with a completion marker, and later runs
  never touch the network. Because the rev pins the content exactly, no
  lock file is involved
- `version` dependencies resolve through the system pkg-config
  ([ADR-0015](adr/0015-version-deps-pkgconfig.md)): the constraint is a
  minimum (`--atleast-version`), and `--cflags` / `--libs` become the
  public flags and link flags of a synthetic external node. Failure is
  `unsatisfied-dependency`. Each resolution is reconciled against
  `dowel.lock` — appended when new, silent when matching, `lockfile-drift`
  (never a silent rewrite) when differing. Editor sessions skip resolution
  entirely: the LSP starts no external processes
- Diagnostics for unknown properties and type mismatches against the schema
  (with suggestions)
- The `interface(T)` / `compile_env(T)` split: `private` dependencies affect
  one's own compilation but do not propagate to dependents
- Feature flags make dependency-graph edges appear and disappear. Features
  are per package ([ADR-0017](adr/0017-feature-forwarding.md)): the active
  set is carried as `<package>/<feature>`, `feature.<name>` is answered
  qualified by the package whose manifest declared the value, and a
  `[features]` value spelled `dep/feat` forwards into that dependency. A
  forward to an undeclared dependency is `undeclared-dependency`; one naming
  a feature the dependency does not declare is `unknown-feature`. Loading
  and resolution iterate to a fixpoint because a forwarded feature can
  activate an optional dependency
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
- Per-language flags: `flags` applies to every language, `c_flags` /
  `cxx_flags` follow it and reach only their own language
- The language standard is typed: `c_std` / `cxx_std` take a value from a
  closed, ordered vocabulary and merge with the `max` rule, so the highest
  standard in the closure wins ([ADR-0016](adr/0016-language-standard-property.md)).
  A value outside the vocabulary is `unknown-standard`, checked where it is
  written (`match` arms and `when` branches included). The generated `-std=`
  precedes `c_flags` / `cxx_flags`, so an explicit flag still overrides it —
  the escape hatch for GNU dialects, which are deliberately not in the
  vocabulary
- `link_flags` accept `Path` elements, which expand to absolute paths. That
  is how a linker script inside the package is named
  (`["-T", file("ld/app.ld")]`): the link runs in the build directory, so a
  relative string never reaches it, and the language has no string
  concatenation with which to build an absolute one (issue #70)
- `[package] targets` declares the triples a package is for. Any other
  triple — the host included — is refused with `unsupported-target` before
  building, so a bare-metal tree does not quietly produce an x86-64
  "firmware" image when `--target` is forgotten. Undeclared means any triple,
  which keeps a package that builds for the host while swapping tools when
  cross-compiling expressible; the declaration is deliberately separate from
  `[toolchain.<triple>]` (issue #71)
- `link_flags` ride the link closure, `private` included: a static archive
  cannot carry its own link requirements, so the flags a library declares —
  and the `--libs` of a `version` dependency it keeps private — reach the
  final link the same way its archive does. The `public` / `private` split
  keeps controlling translation propagation only, so "do not leak the
  headers" and "stay linkable" hold at the same time (issue #56)
- `[<kind>.<name>.artifacts]` derives files from a target's artifact —
  the step embedded work needs after linking (`objcopy -O binary` /
  `-O ihex`, a stripped copy). Each entry names a toolchain tool by name and
  its arguments; the input and output are appended positionally
  ([ADR-0008](adr/0008-runner-transfer.md)), the output being the artifact
  with its extension replaced. The transforms are ordinary graph nodes: they
  are produced by `dowel build`, skipped when the input has not changed,
  performed by the tool the triple selects, and rebuilt when the declaration
  changes. The tool is probed only when a declaration uses it (issue #60).
  A derived file appears whenever its target's artifact does — a library
  reached only as a dependency keeps producing it (issue #64); nothing
  consumes a derived file, so it has to be named as a default explicitly or
  the backends would produce different trees
- `[<kind>.<name>.inspect]` declares reporting tools (`size` / `nm` /
  `objdump` / `readelf`) run by `dowel inspect`. An inspection produces no
  file, so it is deliberately outside the build graph: nothing about it can
  be up to date. The tool is named, not spelled out, so a cross build
  reports with the triple's tool; output is passed through unparsed and a
  nonzero exit fails the run, which is how a budget check is expressible
  without dowel knowing any tool's output format (issue #60)
  Inspection tools that produce no file (`size`, `nm`, `objdump`) are not
  expressible yet
- The archiver is part of the toolchain: `[toolchain] ar` (default `ar`,
  also per-triple in `[toolchain.<triple>]`, configuration key `tc.ar`)
  names the tool that creates static libraries, so cross builds do not fall
  back to the host's `ar` (issue #50). It is probed — and required — only
  when the build produces an archive; because the name is part of the
  action's command line, changing it rebuilds the archive
- The tool set is table-driven (`dowel_eval::config::TOOLS`): the
  `[toolchain]` keys, the `tc.*` configuration vocabulary, the defaults,
  declaration copying, and the `toolchain-mismatch` comparison all follow
  the one table, and a `missing-toolchain` probe helper is shared. Keys
  outside the table are `unknown-property` with a suggestion — a misspelled
  tool would otherwise silently fall back to its default (issue #59). Adding a
  future utility (a disassembler, `objcopy`, …) is one table row plus the
  plan-stage site that uses it — only *when* a tool is required stays a
  per-use-site judgment (the C compiler always, C++ when C++ sources
  appear, the archiver when an archive is produced)
- `compile_commands.json` (`arguments` array form)
- The output stage is a backend layer over one neutral build graph
  ([ADR-0018](adr/0018-backend-layer.md)). Four backends: `ninja` (default),
  `direct` (in-process, sequential, mtime-based freshness reading depfiles),
  `make` (generates a `Makefile`), and `graph` (writes `build-graph.json`
  and builds nothing). Each receives a `BuildGraph` and nothing else — the
  same value the document serializes — so a fact missing from the format is
  a broken build rather than a documentation defect. Adding a backend is one
  row in `NAMES` and one trait implementation
- `build-graph.json` ([14-build-graph.md](14-build-graph.md)) is the
  interchange format for a backend outside this repository: versioned,
  parseable back into an equal graph, and the same document
  `dowel graph --kind=action --format=json` prints. There is one JSON
  description of an action graph, not two
- The record of "which command produced this output" belongs to the layer,
  not to a backend, so it stays consistent across switching between them. It
  is **merged** into the previous record rather than replacing it: an output
  the current invocation did not plan is still the product of the command
  last recorded for it, so a narrow call (`dowel test`,
  `dowel build <name>`) does not make the next full build redo untouched
  work (issue #69). Header dependency records (`.d` files) stay on disk and
  are shared between the backends — ninja is not allowed to fold them into
  `.ninja_deps` (`deps = gcc`), because a record private to one backend
  makes the next conclude "up to date" with no dependency information at
  all, silently keeping stale artifacts (issue #41). As a backstop, the
  direct backend treats an output whose declared depfile is missing as stale
  instead of fresh
- `make` has limits ninja does not: it cannot name a path containing
  whitespace, `:`, `#`, `$`, `%`, `;`, `=`, `\`, `*`, `?`, `[`, or `]`. The
  backend refuses such a build, naming the path, instead of writing a
  makefile that quietly builds something else
- Per-configuration build directories. The identifier is folded to one path
  component (anything outside `[A-Za-z0-9_.+-]` becomes `--`), so a feature
  name containing `/` cannot split a configuration across two levels
  (issue #68)
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

Documents inside a package are diagnosed to the same depth as `check`
([ADR-0010](adr/0010-check-scope.md)): a workspace model is built per
change — the open buffers overlay the disk (the buffer is the source of
truth), and the model is loaded from every open manifest's directory, so a
document edited as someone's dependency gets its diagnostics (e.g. its half
of a merge conflict) from the dependent's model — and then the plan stage
runs over it, producing glob-expansion, path-resolution, and
toolchain-existence diagnostics (`empty-glob` / `unresolved-path` /
`invalid-source` / `no-sources` / `missing-toolchain`) from real file-system
scans. Everything is read-only: the editor session never touches the
network (git checkouts are reused, not fetched), never reads or writes the
store, starts no external processes, and is created and dropped per
change — it is not a daemon. What remains excluded — fetching, `--target`
triggered checks, and system-package resolution — is listed with reasons
in `dowel_lsp::UNSUPPORTED`.

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

### Migration (`dowel-build`)

- `dowel migrate verify <compile_commands.json>` compares a reference
  compile database against the plan, source by source, after normalization
  (`-DX` ≡ `-D X` ≡ `-DX=1`; `-I` resolved against each entry's
  `directory`; compiler name, `-c`/`-o`, and the `-MD` family ignored;
  configuration-level flags — optimization, debug info, `NDEBUG` — dropped
  from both sides, since dowel's `cfg.opt` and the reference's build type
  supply them independently (issue #54); remaining flags as a multiset).
  Differing ported sources fail the run; unported sources are reported
  without failing — porting is incremental (docs/40-migration.md 4)
- Both reference forms are read (`arguments` array and shell-quoted
  `command` string). Output is text or `--format=json`
- `dowel migrate import <cmake-build-dir>` drafts `dowel.toml` /
  `dowel.build` into the CMake source directory from a File API
  `codemodel-v2` reply, refusing to overwrite. The draft is marked
  **UNVERIFIED** in a header comment (the favored shape of Q6; a machine
  gate on unverified targets remains open) and points at `migrate verify`.
  Everything lands in `private` blocks — the public/private intent is
  unknowable from the File API — and sources are listed explicitly, not
  globbed, so the draft stays faithful to the extracted projection.
  Configuration-level flags from the CMake build type (`-O` / `-g` /
  `-DNDEBUG`) are not copied: dowel's `--config` supplies them, and copying
  them unconditionally would make a draft imported from Release produce
  optimized `NDEBUG` debug builds (issue #54)

### Scaffolding (`dowel-cli`)

- `dowel new <path>` generates a working `bin` package, or a library with a
  passing test target under `--lib`. The skeletons match `examples/hello`,
  and e2e builds and runs every generated form, so they cannot silently rot
- `dowel add <path>` creates a library package in a subdirectory and appends
  the `[[dependencies]]` entry to `dowel.toml` (an append preserves the
  existing text; array-table position carries no meaning in strict TOML).
  Wiring `deps = [dep("...")]` into a target stays explicit — the command
  prints the line to add
- `dowel add --git <url> [--rev <rev>]` declares a git dependency. The
  manifest only receives a full 40-digit sha: a name (or omitted rev = HEAD)
  is resolved once via `git ls-remote`, the same judgment as `dowelup pin`

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

Current breakdown (493 tests):

| Stage | Contents | Count |
|---|---|---|
| `fmt` / `clippy` | formatting check and lints (`-D warnings`) | — |
| `unit-*` | per-crate unit tests | 293 |
| `syntax-robustness` | no panics and losslessness on broken input | 5 |
| `model-integration` | manifest loading through interface merging | 10 |
| `model-incremental` | counting what a reload did not recompute | 10 |
| `e2e` | compile real C and C++, run it, check the output | 117 |
| `scenario` | operation sequences over time (edit and rebuild, configuration switches, cross-process change detection and restore) | 24 |
| `fixture` | real-shaped projects (`tests/projects/`) end to end | 11 |
| `diagnostics` | diagnostics reaching the CLI (59 cases), applying fix suggestions, location presence, `check` scope, coverage tracking | 12 |
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
| the direct backend omitted the command line from freshness, missing flag changes | a single run never sees the second execution |
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
| Meson `introspect` import | Phase 3 backlog; CMake File API import and `migrate verify` are implemented |
| `dowel debug` | Phase 4 |
| language-server diagnostics that need fetching, `--target`, or external processes | the editor session is read-only and host-targeted by design; the remaining exclusions are listed with reasons in `dowel_lsp::UNSUPPORTED` |
| cleaning up artifacts left on target machines; skipping redundant transfers | Phase 4; transfers run every time |
| a native registry / tarball dependency source | Phase 5; `version` deps delegate to pkg-config ([ADR-0015](adr/0015-version-deps-pkgconfig.md)) and `dowel.lock` records their resolutions — a dowel-run registry, if ever wanted, is a separate future decision |
| prebuilt acquisition for `dowelup` | Q10; today source builds only |
| automatic ABI label computation | Phase 6; today only `must_equal` verification of a hand-written `abi`. Nothing verifies that a surface declaring `abi = "c"` really is `extern "C"` — the claim is narrower and more checkable than a language label, and is what an IDL or a header scan would confirm ([ADR-0019](adr/0019-c-abi-label.md)) |
| automatic composition of the ABI label from its components | Q2; `c_std` / `cxx_std` are now typed values the label can read ([ADR-0016](adr/0016-language-standard-property.md)), but which components make up the label, and at what granularity, is still open |

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
