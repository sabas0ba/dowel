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
| `dowel-support` | spans, source maps, diagnostics, structured logging, JSON output, SHA-256 |
| `dowel-syntax` | lexing, lossless CST, error-tolerant parser |
| `dowel-query` | memoization, dependency tracking, early cutoff, durability layers, cancellation |
| `dowel-store` | the on-disk store (append-only value log, fixed-length index, single writer) and the per-user probe-fact database |
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
  ([ADR-0037](adr/0037-store-gc.md)). Two things grow: the append-only log
  keeps the old bytes of every overwritten key, and the per-configuration
  build directories accumulate as triples and configurations are switched
  — the larger number by an order of magnitude, and previously collected
  by nothing.
  - **Growth is reported by default.** A run that ends over budget says so
    in one line. The budget is the live bytes themselves, so it follows
    the graph instead of being a number that fits one repository.
    `DOWEL_CACHE` picks `notify` (default) / `gc` / `off`; the default
    reports rather than collects, because compaction rewrites a file and a
    build should not pause for work its user did not ask for
  - `gc` compacts the store and removes older format versions;
    `--older-than=<days>` also removes build directories not *written* in
    that long. Without a number it leaves them alone — "everything but the
    current one" would delete the release tree of someone alternating
    between two configurations daily
  - The index is deleted **first** during compaction: offsets move, and an
    index that survives a replaced log points at the right offsets in the
    wrong file, which reads as plausible garbage. Deleting it first means a
    crash leaves an empty or shorter store, never a wrong one
  - Per-record ages are not recorded: evicting entries individually would
    mean writing on every read to maintain a last-used time

### Probe facts (`dowel-store::facts`, `dowel-build::probe`)

What dowel asked a tool and what it answered, recorded in the **user's**
cache (`$XDG_CACHE_HOME/dowel/facts/<format-version>/`) rather than the
project's ([ADR-0028](adr/0028-probe-facts.md), docs/20-architecture.md
section 9).

- Outside the project because a fact belongs to the tool: the same compiler
  gives the same answer in every tree, and under `.dowel/cache/` the
  question is re-asked once per tree — the top of the durability hierarchy
  living in the most volatile place
- The key carries the tool's identity (path, size, mtime) beside the
  question, which is why there is no invalidation mechanism: replace the
  tool and the key changes, so the stale fact is unreachable. `cache gc`
  collects old format versions
- Only questions that **start a process** are recorded: `-dumpmachine` and
  `--version`. Scanning `PATH` is a few `stat` calls, and recording it would
  cost more to keep honest than it saves. A recorded resolution is still
  checked for existence, since a tool can be removed without `PATH` changing
- "It did not answer" is recorded too — `cl` has no `-dumpmachine`, and
  without recording the silence every run asks again
- Its first reader replaced an unrecorded input: the host triple used to be
  assembled from the OS and architecture dowel itself was compiled for, so a
  machine whose compiler says `x86_64-pc-linux-gnu` would treat that
  spelling as a cross target and demand a runner. `configure` now asks the
  host C compiler, and both spellings count as the host
- Unwritable is not an error, the same judgment as the store: what is lost
  is the saving, not the answer

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
- `when` predicates compose with `and` / `or` / `not`, precedence
  `not` > `and` > `or`, parentheses to override
  ([ADR-0032](adr/0032-predicate-composition.md)). "Linux or macOS" had to
  be two identical lines that nothing tied together, and "everywhere except
  Windows" could only be written by listing the other values — which stops
  covering them the day a word is added to `target.os`, a vocabulary that
  is expected to grow. The operators are words to match the rest of the
  language, and every operator binds on the same line, so a following key
  named `or` is a key. Domain checking reaches each leaf, so a misspelling
  inside a composition points at the misspelling. `match` is still the way
  to choose *between* alternatives: `or` makes one value reachable under
  several conditions, it does not make two values exclusive. Exhaustiveness
  checking is untouched — it belongs to `match`, which has arms; `when`
  has none
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
- **An `abi` label may be a set of components**, compared one by one
  ([ADR-0042](adr/0042-abi-label-components.md)). Two labels conflict when
  they name the same component with different values; a component only one
  side names is not a constraint, and the merged label is the union. That is
  the answer to Q2's dilemma — too coarse makes verification meaningless,
  too fine breaks sharing — which existed only because the label was one
  opaque token: with components, granularity is chosen per declaration
  rather than once for everyone.
  - Component names and values are a closed vocabulary
    (ADR-0034's procedure); anything else is `unknown-abi-component`. Two
    exist: `libc` (`gnu` / `musl` / `msvc` / `apple` / `none` / `other`) and
    `cxx_stdlib` (`libstdc++` / `libc++` / `msvc-stl`)
  - `libc` is read off the triple, so `target.env` joined the configuration
    vocabulary — `target.os` does not answer this axis, since `linux-gnu`
    and `linux-musl` are the same OS and two runtimes that do not link
  - A declared `libc` is also checked **against the build**. Comparing
    labels only asks who requires what, never what this build is; a surface
    requiring `musl` built for a gnu triple links fine and fails at run time
  - A label written as one word keeps its meaning and is compared whole.
    A word and a component set cannot be compared, since a word cannot be
    taken apart
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
- Archive dependencies name a `url` and a `sha256`
  ([ADR-0029](adr/0029-tarball-dependencies.md)), which is how most C is
  actually distributed. The hash is required for the same reason `rev` is: a
  URL is a name, and the bytes behind a name change. Fetching and unpacking
  are delegated (`curl` or `wget`, then `tar`) but **verification is not** —
  the tool that computes SHA-256 differs per system, and a pin that can only
  be checked where a particular tool exists is a weaker promise than a pin,
  so it is implemented in-tree (`dowel_support::sha256`, pinned by the
  published test vectors). The archive is verified before unpacking, since
  unpacking lets the archive decide where bytes land. Layout follows the git
  checkout: `.dowel/deps/<name>-<hash12>/` with a completion marker written
  last. One top-level directory is stripped by looking rather than by
  declaring a `strip_components`
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
- The configuration vocabulary gained `target.os` / `target.arch`, derived
  from the target triple and finite, so `match` on them is
  exhaustiveness-checked ([ADR-0026](adr/0026-target-os-arch.md)). Before that the target could only be reached as a free-form triple,
  and the word that *read* like the OS — `host.os` — meant the build host,
  so the obvious spelling compiled and selected the wrong sources
  (issue #115). `other` is in both domains because `--target` takes any
  string, and a domain that cannot be closed brings back the `_` arm
- The vocabulary is **closed** ([ADR-0034](adr/0034-closed-vocabulary.md),
  settling the last of Q1): nothing extends it. Three things depend on
  that — exhaustiveness checking, findable misspellings, and a
  configuration identity that does not depend on which toolchain was
  picked. A project's own axes are `[features]`, and the diagnostic now
  says so instead of answering "the vocabulary is provisional; see Q1",
  which told a reader their spelling was wrong but not what to write. It
  is a note, not a fix: rewriting `cfg.sanitizer` to `feature.sanitizer`
  leaves `unknown-feature` behind, and the property test for suggestions
  rejected the first attempt to offer it as one
- The executable's spelling follows `target.os`: `bin/<name>.exe` for a
  Windows target, decided in one place so the runner, `artifacts`,
  `inspect`, `dowel debug`, the `built:` line, and the freshness
  fingerprint read the same value (issue #112). While they did not, the
  build succeeded and every later stage was handed a path that did not
  exist — and, because "the output is missing" and "it has not been built
  yet" are the same state, the incremental build never converged: relinking
  every run, silently, with only the elapsed time to notice by
- A target's name is unique within its package across kinds: `[lib.foo]`
  beside `[bin.foo]` is `duplicate-target`, naming both sites (issue #114).
  The name keys three separate things — `target("...")`, the
  `<package>:<target>` label, and `obj/<package>/<target>/` — so two of a
  name meant a `public` block reaching nobody, a graph whose steps could
  not be told apart, an ambiguity whose own diagnostic suggested a spelling
  that was still ambiguous, and, once both compiled the same source, two
  rules writing one object path (which surfaced in ninja's words, not
  dowel's). Qualifying all three by kind would buy only the coexistence of
  `libfoo.a` and `foo`
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
- The default reach of an unnamed `dowel build` / `dowel test` / `dowel
  bench` is **this tree's package** (issue #126). A consumer's build used to
  build its dependencies' tests: harmless noise on a hosted triple, a
  failure on one without an OS, since the library's tests are written for
  a host and the consumer's manifest has nothing wrong with it. Naming a
  dependency's target still reaches it — what changed is the default, not
  what is reachable
- `targets` on a target names the triples it is built for, with the same
  spelling as `[package] targets` and a narrower reach (issue #126). A
  library supporting four triples can now say its host-side test runs on
  three; the package-level list could not express that, since the package
  supports all four. A target outside its triples does not appear in that
  triple's plan rather than failing there — but naming it explicitly is
  `unsupported-target`, because a named target is a request and a build
  that quietly produces nothing reads as success
- `[package] toolchains` names a file of shared toolchain declarations
  ([ADR-0033](adr/0033-shared-toolchain-file.md)), which is what removes
  the copying cost ADR-0031 left standing: a tree with four triples and
  three consumers wrote the same table once per (consumer, triple) pair.
  The file holds the same `[toolchain.<triple>]` tables, read by the same
  code, and **a local declaration wins one tool at a time** — overriding
  per triple would mean rewriting the whole table to change one compiler,
  which is the cost being removed. Reading is one level; a dependency's
  `toolchains` is not read either. The file goes through the query engine
  so it is recorded as an input: a one-shot build would survive a
  shortcut, but the language server holds a `Session` and would keep
  answering with the previous compiler
- A dependency's `[toolchain]` does not apply to the build
  ([ADR-0031](adr/0031-toolchain-is-the-builds.md)); when one exists for
  the requested triple, `missing-toolchain` reads out its values and says
  why they do not apply. The error used to advise "declare one, for
  example …" while `toolchain-mismatch` printed the actual answer two
  lines below — dowel had found what it was looking for and still said it
  was missing (issue #125). The position is unchanged, because a tool's
  *name* comes from the machine doing the build, not from the library;
  what changed is that the output states it
- `[template.<name>]` holds shared settings that targets take with
  `use = [template("...")]` ([ADR-0035](adr/0035-template-kind.md)). A
  library with no sources already shared settings through a dependency, but
  only `public` propagates, so sharing meant publishing — warning flags
  pushed onto everything downstream. A template **expands into the block it
  came from**: its `private` becomes the user's `private`. Expansion places
  the template's values ahead of the target's own and merges normally, so
  no special case enters the merge algebra and `dowel why` still names the
  template's line. A template holds settings only (root properties are
  refused, which is also why templates do not use templates), produces no
  artifact, and is `not-a-target` when named on the command line. Declaring
  one is not: `check`, `migrate verify`, and the language server enumerate
  the targets that produce an artifact, and each had been counting every
  declared table instead — so the plan received the template as a request
  and refused it, and a package that used a template could not pass `check`
  at all (issue #141). One shared enumeration now serves all three.
  `check` also refuses a target name outright rather than dropping it
- Shared libraries: `[lib.<name>] linkage = "shared"`
  ([ADR-0030](adr/0030-shared-libraries.md)) links `lib<name>.so` /
  `lib<name>.dylib` / `<name>.dll` instead of an archive.
  - It **must** declare `exports`, and there is no default. The exported
    surface is the one thing the platforms disagree about — everything
    non-`static` on ELF and Mach-O, nothing at all on Windows — so adopting
    either behavior would let the same manifest describe two different
    interfaces. Omitting the list is `missing-exports`
  - From that one list dowel generates the linker's own form: an ELF
    version script, a Mach-O symbol list (with the platform's `_` prefix),
    or a `.def` for PE. The form follows the **object format**, not the
    argument style, because mingw spells arguments the GNU way while
    producing PE. The generated file is an input of the link, so changing
    `exports` relinks
  - `-fvisibility=hidden` is deliberately *not* added. A symbol hidden at
    compile time cannot be restored by the version script's `global:` list,
    so the pair exports nothing at all — measured rather than reasoned
    about. The script alone does the job
  - Every target in a shared library's link closure is compiled `-fPIC`,
    not just the one that declares the linkage: a static library linked
    into a position-independent output must be position-independent too
  - Dependents get a run-time search path into the build tree's `lib/` and
    the library gets a soname (or a macOS install name), which is what
    makes the search path effective — without it the executable records
    the path it linked against. Windows has no rpath, so `dowel test` and
    `dowel bench` prepend that directory to the child's `PATH` instead,
    after the declared `env` is applied so a case that sets `env` does not
    lose it
  - **Within its own package the library links statically**
    ([ADR-0038](adr/0038-shared-inside-its-package.md)): a shared library
    also produces an archive, and sibling targets link that. `exports` is
    a boundary toward code not written alongside it, and the package is
    the unit of distribution — declaring `linkage = "shared"` used to stop
    the library's own tests from linking, since internal names are not on
    the surface (issue #134). Testing only the public surface cannot cover
    what is behind it, which is why every system has this (CMake's
    `OBJECT`, Meson's `objects:`, Cargo's in-crate tests). The objects are
    compiled once; the archive costs one `ar` run. The shared object is
    built even when nothing links it, since shipping it is the reason to
    declare one
  - **`exports` is checked against the library that was built**
    ([ADR-0039](adr/0039-exports-are-checked.md)). After the build dowel
    asks the toolchain's symbol lister (`tc.nm`, or `dumpbin` under MSVC)
    what the library exports and compares. A name that is not in the answer
    is `unexported-symbol`, pointing at the line that declared it and
    naming the closest symbol that does exist. The linker cannot do this —
    a shared library may legitimately have undefined symbols, so neither
    `-Wl,-u` nor `--no-undefined` turns a missing export into an error
    (measured). A symbol lister that is not on `PATH` skips the check
    rather than failing the build
  - **`soversion` declares the ABI generation**
    ([ADR-0040](adr/0040-shared-library-version.md)) and enters the name:
    `libcore.so.2`, `libcore.2.dylib`, `libcore-2.dll`. The soname comes
    from the output's file name, so consumers record the versioned one —
    which is the point, since a name recorded at link time cannot be
    corrected afterward. The unversioned name is placed beside it as a
    symlink; without it `-lcore` finds the archive that sits in the same
    directory (ADR-0038) and links statically, measured rather than
    reasoned about. The release is not the generation, so `[package]
    version` does not supply the number, and declaring nothing keeps the
    plain name. A negative number is `invalid-soversion`
  - **`dowel install --prefix=<dir>`** copies the products out of the build
    tree ([ADR-0041](adr/0041-install.md)): `bin` into `bin/`, `lib` into
    `lib/` with the unversioned name, and each library's own
    `public.includes` into `include/`. `test` and `bench` are not
    installed. Nothing is rebuilt, so what was tested and what ships are
    the same bytes.
    - The obstacle was the run-time search path: an absolute path into the
      build tree keeps working while that tree exists, so the breakage
      appears at the receiver. Every artifact linking a shared library now
      also records one relative to itself (`$ORIGIN/../lib`,
      `@loader_path` on macOS), which makes a copy sufficient — no
      relinking, and no `patchelf` reading object formats
    - That `$` is meaningful to ninja, to make, and to the shell running a
      make recipe, so all three backends are checked. A missed quote links
      fine and fails only after the artifact moves
    - `--prefix` is required: `/usr/local` needs root, and a writable
      default would be a directory nobody wants. `--destdir` prepends a
      staging root, which works unchanged because the recorded paths are
      relative
    - Shared libraries from other packages in the link closure are copied
      too. That crosses the boundary ADR-0038 draws, and the alternative is
      an install that does not run
  - **An installed library describes itself in pkg-config**
    ([ADR-0043](adr/0043-pkgconfig-generation.md)):
    `lib/pkgconfig/<name>.pc` is written from the target's `public` block —
    `includes` becomes `-I${includedir}`, `defines` and `flags` the rest of
    `Cflags`, `link_flags` join `Libs`. Nothing new is declared to get it,
    and `prefix` is the real prefix even under `--destdir`. dowel already
    read this notation ([ADR-0015](adr/0015-version-deps-pkgconfig.md)) and
    could not write it, so a library could move to dowel only if every
    consumer moved with it. `Requires` names only what is certainly present:
    system dependencies, the **sibling libraries of the same package**
    this run wrote a descriptor for, and dowel packages this same run installed —
    a `Requires` pointing at a missing file makes pkg-config fail outright.
    `[package] description` was added because pkg-config requires
    `Description:`; absent, the target name stands in
    - The sibling case is the one that actually occurs, and it was missing
      (issue #156). `install` writes the current package's libraries
      (ADR-0041), so "installed in this same run" is a condition only a
      sibling can meet. A static archive carries no link requirements of its
      own, so a `top` that sits on a `base` linked with undefined references
      for any consumer using pkg-config alone. It passed when the libraries
      were shared, because `DT_NEEDED` fetched the sibling — one line of
      `linkage` decided whether the published surface worked
    - Siblings are listed in link order (dependents first), which is what a
      static resolution requires. `Requires` rather than `-lbase` in `Libs`,
      so the sibling's own `Cflags` and `link_flags` travel with it
  - Symbol versioning *inside* the library (version nodes in the script) is
    not implemented, and neither are CMake package config files (CMake reads
    pkg-config through `FindPkgConfig`, so the common case is covered).
    macOS's `-compatibility_version` is not set: it is a second,
    independently checked number that the one declaration does not decide
- Per-language flags: `flags` applies to every language, `c_flags` /
  `cxx_flags` / `asm_flags` follow it and reach only their own language
- **Assembly is a third language** ([ADR-0048](adr/0048-assembly.md)).
  `.s` and `.S` select it.
  Passing `foo.s` to `cc` did assemble it, so it looked like it already
  worked — measuring what came out found three things wrong.
  - `-std=c17` and `-Wall` were reaching the assembler, because assembly was
    "not C++, therefore C". `asm_flags` is its own, and `c_flags` / `c_std`
    stop at C
  - The objects had no `.note.GNU-stack`, so the linker warned that an
    executable stack was implied — and said the warning will become an
    error. The C compiler marks its own output; nothing marks hand-written
    assembly, so dowel passes `-Wa,--noexecstack`. Same category as `-fPIC`
    for a shared library: the correctness of the output depends on it
  - `-MD -MF` was passed to `.s` and no `.d` was ever written — a declared
    output that does not appear, the shape of bug that makes an incremental
    build never converge. Only `.S` goes through the preprocessor, so only
    `.S` asks for one
  - Expressing "no depfile" needed a ninja fix: its rule says
    `depfile = $depfile`, and an edge that leaves the variable unbound makes
    ninja resolve it to itself and refuse the file as a cycle. The other two
    backends already coped, which is why the test covers all three
- **A build may declare its own assembler**
  ([ADR-0050](adr/0050-separate-assembler.md)). `[toolchain] asm` joins the
  tool table with no default; empty means the C driver assembles, which is
  ADR-0048 unchanged. `.asm` became an assembly extension, since the
  reason for excluding it — no tool could accept that syntax — stopped
  holding once one can be named. The projects that need this are the ones
  dowel is for: OpenSSL's and BoringSSL's generators emit gas syntax for
  the Unix triples and NASM syntax for Windows.
  - **A declared assembler takes every assembly source in that build**, not
    only `.asm`. One build, one assembly syntax; a tree shipping both
    selects them per triple with `match target.os`, where the toolchain
    declaration already lives. Routing by extension within one build would
    put two assemblers behind one `asm_flags`, and `-f elf64` means nothing
    to the other one
  - **dowel passes the input, the output, and `asm_flags` — nothing else.**
    The rest of a compile line is spelled for a C driver, and an assembler
    is not one. Since `asm_flags` is `List<Word>`, what the assembler does
    need is written there and can carry paths:
    `["-f", "elf64", "-I", dir("asm")]`. The I/O spelling follows the style:
    `-o out in` for `nasm`, `/c /Fo<out> in` for `ml64`
  - A `.asm` source with nothing declared is `missing-assembler`, naming the
    file and the declaration to write. Handed to the driver it comes back as
    "file format not recognized" from the *linker*, two stages later
  - **Executable stack is refused at the link instead.** dowel cannot ask a
    tool whose spelling it does not know for `-Wa,--noexecstack`, but it
    knows the linker's: a link closure containing objects from a declared
    assembler gets `-z noexecstack`, before `link_flags` so it can be
    overridden. What does not travel is the per-object marking — an
    installed archive of NASM objects carries none, and only a `section
    .note.GNU-stack` directive in the source survives redistribution
  - No depfile is requested from a declared assembler. NASM can write one,
    but the spelling belongs to that assembler rather than to the tool slot,
    so a `%include` edit does not rebuild today
- **The set of source spellings is closed**
  ([ADR-0051](adr/0051-source-language-is-closed.md)). C is `.c` and `.i`;
  anything outside the three lists is `unknown-source-language`, reported
  where it is declared. The old rule was "everything that is not C++ is C",
  which was a decision about `.c` that also swallowed `README`.
  - What it cost was measured: `cc -c note.txt -o note.o` **warns, exits 0,
    and writes nothing**. dowel does not show a successful command's
    warnings, so the failure arrived from the linker, about a path inside
    the build directory, with no source name and no line — and `dowel check`
    passed. The declared-but-absent object also made the step permanently
    stale, so an unchanged tree never converged (issue #157, the shape
    ADR-0048 refused for depfiles)
  - A glob that sweeps up a `README` is reported at the glob, since that is
    where the written line is
  - **A command that exits 0 without writing its output has failed.** The
    direct backend checks a step's outputs and reports the tool's own
    stderr; after *any* backend, dowel checks that what it is about to print
    as `built:` exists. The second net is what covers ninja and make —
    neither fails on a missing output, and dowel was printing `built:` for a
    file that was not there. It sits beside the export check, after the
    build, for the same reason
  - A source with no extension is refused too, and there is no `-x c` escape
    hatch. Nothing checks that an artifact is *correct*, only that it is
    there
- **A `lib` may name a library that already exists**
  ([ADR-0049](adr/0049-prebuilt-libraries.md)). `prebuilt` takes the place
  of `sources`, so a Rust `staticlib`, a Zig `build-lib`, a Go `c-archive`,
  or a vendor blob is a dependency instead of a `-L` and a `-l` written in
  every consumer. The target is ordinary from there on: `public` propagates,
  `dowel why` traces it, and it produces no compile and no archive action.
  - dowel does not run cargo, zig, or go. Doing so would make it a general
    build system, which [ADR-0001](adr/0001-toolchain-vs-supply.md) says it
    is not. The file's existence is checked when the plan is made, like a
    tool's; absent, it is `missing-prebuilt` naming the path and saying
    dowel does not run the build that produces it
  - This gives the ABI label its first edge worth checking. A `staticlib`
    built against musl, declared `abi = { libc = "musl" }` and linked into a
    gnu build, is refused before the link — the check
    ([ADR-0042](adr/0042-abi-label-components.md)) was designed for the case
    where one side is not built here, and until now there was no way to have
    one. The extra system libraries these toolchains want (`-lpthread`,
    `-ldl`, `-lm`) go in `public.link_flags`, which already propagates
  - `sources` and `prebuilt` together is `prebuilt-with-sources`: with both,
    which file is the artifact has no answer. Only a `lib` may be prebuilt
    (`prebuilt-not-a-library`) — what is named is something to link against
  - Nothing verifies that the file is a library, or that it is for this
    triple. `exports` and `soversion` describe how dowel *builds* a shared
    library and do not apply, so ADR-0039's export check does not run on one
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
- A toolchain also has an **argument style**: `gnu` or `msvc`
  ([ADR-0027](adr/0027-toolchain-style.md)). Declaring a tool's *name* was
  not enough to use it — the arguments dowel assembles were spelled Unix-only,
  so naming `cl` produced a command line no `cl` can read, and `-MD` (a
  request for a dependency record) is a valid MSVC flag meaning "link the
  dynamic CRT" — a choice of ABI, and the very flag `00-overview.md` cites
  under "no single ABI" (issue #113). The style is derived from the triple
  (`*-msvc`) and `[toolchain] style` overrides it; it also decides the
  tools' defaults (`ar` → `lib`) and adds `link`, which is empty under GNU
  (the driver links) and `link.exe` under MSVC. **Only what dowel assembles
  is spelled per style** — a user's `flags` pass through untranslated,
  because a table of flag equivalences would be knowing the compiler.
  Header dependencies differ in mechanism: MSVC prints `/showIncludes`
  lines instead of writing a record, so whoever runs the compiler folds
  them into the same `.d`, and everything that reads the record stays
  style-agnostic. The cost is that under MSVC the record is not shared
  across backends (ninja keeps its own), so switching backends costs one
  recompile. Whether an MSVC build then *succeeds* is unverifiable here —
  there is no Windows CI — so the checks put a fake `cl` on the path and
  read the assembled command
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
- **A toolchain can be fetched and pinned**
  ([ADR-0044](adr/0044-toolchain-acquisition.md)). `url` + `sha256` in a
  toolchain table names an archive; the tools' relative paths are then
  resolved against what is unpacked. Everything else in a cross build was
  already pinned — the manifest, the sources, the dependencies — and the
  compiler, which decides the object code, was whatever the machine had
  under that name.
  - `url` requires `sha256` (`unpinned-toolchain`), for ADR-0029's reason:
    a URL is a name, and the bytes behind a name can change. A failed fetch
    or a digest mismatch is `unfetchable-toolchain` and **stops the build**
    — falling back to PATH would hide the machine's compiler behind a
    declaration that says otherwise
  - It is unpacked into the **user's** cache
    (`$XDG_CACHE_HOME/dowel/toolchains/<hash12>/`), not the tree. ADR-0028's
    reasoning, only stronger: the same archive is the same bytes in every
    tree, and a toolchain is an order of magnitude larger than a dependency
  - An absolute path and a bare name are left alone — those already mean a
    place on this machine and a PATH lookup
  - `toolchain-mismatch` now compares the **resolved** command. Comparing
    the declaration against what the build uses made every package with a
    fetched toolchain warn about itself
  - The language server never fetches: it resolves against a toolchain that
    is already unpacked, and otherwise leaves the command as written
  - **`[toolchain] sysroot` and the `sysroot()` function**
    ([ADR-0047](adr/0047-sysroot.md)). `PathBase::Sysroot` existed in the
    value type and every path that reached it was refused; for a cross build
    the target's headers and libraries live in the sysroot, so a tree either
    hardcoded an absolute path — the thing pinning was for — or picked up
    the host's headers and failed later in the compiler's words.
    `sysroot()` is the one builtin taking no argument, since the root itself
    is the common case; `sysroot("usr/include")` names a path under it. A
    relative declaration resolves against a fetched toolchain, and unlike a
    tool a bare name resolves too — a sysroot is never a PATH lookup.
    `flags` / `c_flags` / `cxx_flags` joined `link_flags` as `List<Word>`,
    so `["-I", sysroot("usr/include")]` reaches the compile line without the
    string concatenation the language does not have. Writing it with none
    declared is `missing-sysroot`; there is no default
- **Offline is a mode, not an accident** ([ADR-0045](adr/0045-offline.md)).
  Every acquisition already wrote a completion marker and reused it, so a
  fully-fetched tree built without the network — by accident. Nothing said
  so, nothing checked it, and a missing input read as curl's exit status
  rather than as "this has not been fetched".
  - `--offline` (or `DOWEL_OFFLINE=1`) forbids acquisition and reports
    `needs-fetch` for anything absent, naming where it would come from and
    that `dowel fetch` is the way to get it. A separate code from
    `unfetchable-*`: nothing was tried, and the fix is different
  - `dowel fetch` acquires dependencies and the toolchain and **stops**, so
    "ready to go offline" is visible rather than inferred
  - The mode is process-wide, set once from argv like the logging level.
    Threading it through each fetch function would mean wiring up every new
    acquisition path, and the one that is forgotten is the one that reaches
    the network
  - `pkg-config` is unaffected: it starts a local process and reads local
    files, and offline is about the network. Nothing sandboxes the
    compiler either — the guarantee covers what dowel does
- Transfer for `[runner.<triple>]` (`transfer` / `remote_dir` / `host`):
  when the target machine cannot see the build machine's file system,
  artifacts are carried over before launch. Paths are not written in the
  manifest; the implementation appends them
  ([ADR-0008](adr/0008-runner-transfer.md)).
  - **The same bytes are sent once per destination**
    ([ADR-0046](adr/0046-transfer-once.md)). Over SSH to a board, or a
    serial link, the copy is frequently longer than the test — and dowel was
    careful not to recompile what had not changed, then re-sent the result
    every run. `<build-dir>/transfers` records the artifact's fingerprint
    against the transfer's full command line; the command line is the key
    because two destinations are two transfers
  - **A run that could not start drops the record**, so the next one sends
    again. dowel cannot see the target machine and so cannot know it was
    wiped; the launch failing is the only evidence available, and using it
    makes the skip self-healing
  - **Artifacts are left behind on purpose.** Cleaning them up would undo
    the skip — the two cannot both be defaults. The record lives in the
    build directory, so `cache gc --older-than` or removing `.dowel/build`
    resets the assumption; there is no separate switch
- `[runner.<triple>]` — an execution wrapper per target triple.
  `dowel test --target=<triple>` launches through the wrapper transparently.
  If the triple differs from the host and no runner is declared, launch is
  refused with a diagnostic beforehand — afterward it would surface as
  `Exec format error`, reporting a configuration mistake as a test failure
- `dowel debug <target>[/<case>]` — builds the target and starts the
  declared debugger on its artifact, package root as the working directory
  ([ADR-0024](adr/0024-debug-command.md)). `debug` is a toolchain tool
  (default `gdb`), so a cross build names its own the way it names its
  compiler, and it is probed only here. The positional argument reaches a
  **case** as well, resolved as `dowel test` resolves it, carrying its
  `args` / `env` / `cwd` (and a harness's `run` plus the discovered name)
  into the session — a debugger is wanted for passing cases too, and
  routing every path through the failure record meant failing on purpose to
  create one (issue #110). Debugging another triple's artifact needs a
  stub, and the runner **declares** it — `debug_args` hosts the program,
  `debug_connect` says where to attach. dowel does not parse the runner's
  flags, so neither is derivable from the other; a cross target declaring
  neither is refused (`missing-debug-stub`) rather than pointing a host gdb
  at a foreign binary, and one declaring only half is told which half
  (issue #109). `debug_args` goes **before** the runner's `args`, since
  `args` may end with the flag that takes the artifact (`-kernel`) and
  anything inserted after it is eaten as that flag's operand (issue #107).
  `--dap` writes the launch configuration to stdout and starts nothing. No
  `substitute-path` is emitted: nothing is remapped yet, so there is
  nothing to compensate for (docs/30-devexp.md 2.1)
- `dowel test --debug-failed` — reopens the failing case under the debugger
  with its declared `args`, `env`, and `cwd` (docs/30-devexp.md 2.3). The
  join it was described as: the test job becomes the debug launch, through
  the same `prepare` as `dowel debug`. Needs the selection to come to
  exactly one case (a debugger attaches to one process; several failures
  are listed with a note to name one), narrows like `--failed` does, and
  composes with `--dap`, which then carries the case's arguments and
  environment. The verdict record is not updated — a debugger session is
  interactive, not a judgment
- `dowel bench` — builds `bench` targets and measures whole-process
  wall-clock time, min/median over `--iterations` runs (default 10;
  [ADR-0025](adr/0025-bench-wall-clock.md)). No framework is imposed and
  none is read — there is no C convention for measurement output.
  `[bench.<name>.cases]` reuses the test-case shape minus `should_fail`
  (a benchmark is measured, not judged; the property is refused). Always
  sequential — parallel measurements are each other's noise. Speed has no
  verdict: a failure is a run that could not be completed, and a failed
  measurement reports no numbers at all. JSON (`bench-result`) carries
  times as integer microseconds
- `dowel test` — launches tests and judges pass/fail by exit status.
  There is no test harness; the C convention ("exit status 0 means success")
  applies. The working directory is the package root unless a case declares
  `cwd`. Only failing tests' output is shown
  - `[test.<name>.cases]` registers several tests from one binary
    ([ADR-0022](adr/0022-test-cases.md)): `args` distinguish them, and each
    carries its own `env`, `timeout`, `should_fail`, `labels`, and `cwd`
    (issue #95 — the default, the package root, is a promise now, not an
    observation). A case's
    label is `<package>:<target>/<case>`, and selection (`--label`,
    `--failed`) and reporting operate on cases. A target with no cases is one
    test, unchanged. Nothing is imposed on the binary — dowel never asks it
    what cases it contains, so which C test framework a project uses stays
    the project's decision
  - A case may itself be conditional — `match` / `when` apply to the case,
    not only to the values inside it (issue #92). The strong use is a case
    that must not exist for some target at all; expressing that by splitting
    the `[test.<name>]` instead would add translation units, which is what
    cases exist to avoid. Every arm is validated, since the condition is not
    resolved until specialization
  - The declaration is checked: a case name that breaks the
    `<package>:<target>/<case>` label grammar is `invalid-name` (issue #97),
    a non-positive `timeout` is `invalid-value` — it would silently mean
    "wait forever" (issue #96) — an empty `cases` block is `empty-block`
    rather than one bare run (issue #99), a type error underlines the key
    that is wrong rather than the whole case (issue #101), and writing a case
    as its own table says what the right shape is (issue #98)
  - `[test.<name>.harness]` is the other shape
    ([ADR-0023](adr/0023-harness-protocol.md)): `list` arguments make the
    binary print its case names, one per line, and `run` arguments precede
    the name when running one. dowel knows no test framework — only those
    two argument lists — so a framework whose listing differs needs a
    wrapper in the project that chose it. The listing runs at test time
    through the same runner as the tests; failing, timing out, or listing
    nothing is a failure of that target, never a silent zero. A listed name
    is held to the same label grammar as one written in the manifest, and a
    name that breaks it is reported like any other listing failure: the
    contents of the line stay uninterpreted, but the grammar of an
    acceptable name is one whichever entrance it came through — and the
    entrance the user cannot edit is the likelier source of a broken one
    (issue #108). `cases` and `harness` together is
    `conflicting-declaration`
  - `timeout` polls `try_wait`; the standard library has no wait with a
    deadline and the core takes no dependencies. The kill reaches the test
    process only, so a test that spawns grandchildren leaks them
  - `--fail-fast` stops at the first failure. The default keeps going (the
    full picture matters); when cut short, the summary reports how many were
    not run
  - `--failed` reruns only what failed last time. Verdicts persist in the
    build directory; verdicts of targets not run are kept
  - Selection works on cases: a positional argument names a target or a
    case (`app:unit/parse`, the spelling the output prints), `--label` picks
    by declared label, and `--failed` reruns what failed. A selection that
    matches nothing **fails** — the report goes to stderr where a CI log
    buries it, so the exit status has to carry it (issues #89 / #91 / #93).
    A tree with no tests, and `--failed` when nothing failed, stay successes:
    neither contradicts what was asked
  - `--no-run` builds and then lists the cases that would run, with their
    labels, `should_fail`, and `timeout` — the only way to see what exists
    without running it, and what makes the label vocabulary discoverable
    (issue #94). It launches nothing, so a cross target needs no runner
  - `--test-jobs=<n>` runs tests in parallel. The default is sequential: C
    tests may use shared resources (the same working directory, fixed ports,
    output files), and a parallel default produces order-dependent failures.
    Display is always in request order. A case with its own `cwd` no longer
    shares the first of those
  - A case killed by a signal fails, `should_fail` or not: what that
    declares is a nonzero **exit**, and a crash is not one (issue #88).
    `should_fail` is written where broken input is fed in, which is also
    where a crash is most likely — treating the two alike turns the defect
    most worth catching green
  - `--no-run` / `--nocapture`, and `--message-format=json` for one
    `test-result` per line. The target and the case are separate fields, so
    grouping by target does not mean splitting a string, and the three ways
    to end without an exit status each have their own field: `timed_out`,
    `signal`, `launch_error` (issue #100)

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
  header, builtin function signatures, configuration key domains, and the
  tables that are not property blocks — `cases`, `harness`, `artifacts`,
  `inspect`, `[runner.<triple>]`. The source is the same table
  `dowel schema dump` reads, and the words naming those tables now live
  there too: keeping them in the type checker alone is what let `cases` be
  known to the checker while the dump and the editor said nothing
  (issue #90)
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
- `stable` cannot resolve until a release tag appears upstream
- A release specifier takes a **published binary** from the release assets,
  verified against the `.sha256` beside it
  ([ADR-0036](adr/0036-prebuilt-distribution.md), closing Q10); everything
  else builds from source, and `--from-source` forces the build. That
  removes the Rust-toolchain requirement, which was the point. The checksum
  catches a truncated download or a stale mirror, not a compromised
  release — whoever can replace the tarball can replace the checksum. What
  separates the two paths is not the hash but what they prove: a source
  build shows the binary came from *this* commit, and nothing in a
  published asset carries that. `install` says which path it took, and the
  record keeps it: `origin` carries `from=asset` / `from=source` and the
  verified `asset_sha256`, and `dowelup list` marks each version. Without
  that, a version installed months ago cannot be told apart from one that
  quietly fell back — and the fallback is silent by design, so "meant to
  fetch, actually built" happens without anyone noticing (issue #146). The
  path is not accumulated the way specifiers are: one file on disk arrived
  one way, and re-installing an already-present sha keeps what is recorded.
  Assets are produced by `.github/workflows/release.yml`, whose naming is
  the same decision written in a second place, so the e2e constructs the
  same layout to keep them from drifting
- A failed fetch says why, naming the tool that actually ran. `wget` was
  run with `--quiet`, which silences errors along with progress, so the
  reason came out empty; and only the last attempt's reason was kept, so
  the name printed could be a tool that is not even installed while the
  real failure (curl's) was discarded (issue #145). Every attempt's reason
  is now collected, and the reason the asset path was abandoned is repeated
  if the source build then fails — otherwise the last words a user reads
  are about `cargo` rather than about the asset

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
- `dowel migrate import <old-build-dir>` drafts `dowel.toml` /
  `dowel.build` into the old system's source directory, refusing to
  overwrite. Two sources are read: CMake's File API `codemodel-v2` reply and
  Meson's `meson-info/` introspection. Which one it is, is decided by
  looking at the directory rather than by a `--from=` flag — what gets
  passed is the old build directory, and what made it is written there; if
  neither is found, the error names both recipes. The draft is marked
  **UNVERIFIED** in a header comment (the favored shape of Q6; a machine
  gate on unverified targets remains open) and points at `migrate verify`.
  Everything lands in `private` blocks — the public/private intent is
  unknowable from what either system reports — and sources are listed
  explicitly, not globbed, so the draft stays faithful to the extracted
  projection. Configuration-level flags from the build type (`-O` / `-g` /
  `-DNDEBUG`) are not copied: dowel's `--config` supplies them, and copying
  them unconditionally would make a draft imported from Release produce
  optimized `NDEBUG` debug builds (issue #54)
- The two differ in what they hand over. CMake reports compile arguments
  already sorted into `defines` / `includes` / fragments and names
  in-project `dependencies`, which become `target(...)`. Meson hands over
  one `parameters` array per target, so the sorting is dowel's (one rule,
  shared by both readers). That array mixes in **link inputs**: the
  archives the target linked, and the `ar` argument string of a static
  library (`csrDT`). Left in `flags` they reach the compiler as input
  files and the draft does not build (issue #135), so `-Wl,` / `-l` / `-L`
  move to `link_flags` and anything that is not a flag at all is dropped
  and named in a comment. Its introspection does not say which targets
  link against which — `deps` is therefore left empty for Meson imports
  rather than guessed from output filenames, which would put wrong edges
  into a draft that is already unverified. Meson's generated sources are
  listed as skipped comments instead of being dropped silently, and
  subproject targets are not imported: they are a different package, and
  merging them into one `dowel.build` would erase where they came from

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

Current breakdown (736 tests):

| Stage | Contents | Count |
|---|---|---|
| `fmt` / `clippy` | formatting check and lints (`-D warnings`) | — |
| `unit-*` | per-crate unit tests | 363 |
| `syntax-robustness` | no panics and losslessness on broken input | 5 |
| `model-integration` | manifest loading through interface merging | 10 |
| `model-incremental` | counting what a reload did not recompute | 11 |
| `e2e` | compile real C, C++, and assembly, run it, check the output | 275 |
| `scenario` | operation sequences over time (edit and rebuild, configuration switches, cross-process change detection and restore) | 28 |
| `fixture` | real-shaped projects (`tests/projects/`) end to end | 11 |
| `diagnostics` | diagnostics reaching the CLI (80 cases), applying fix suggestions, location presence, `check` scope, coverage tracking | 12 |
| `example` | build the real `examples/hello` and run its tests | 3 |
| `up` | `dowelup` resolution, acquisition, and switching against an upstream fixture | 10 |
| `docs` | link resolution, index consistency, and reference completeness | 8 |
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
| the `toolchain` kind in `dowel.build` | still reserved. [ADR-0044](adr/0044-toolchain-acquisition.md) did not need it: the toolchain belongs to the build ([ADR-0031](adr/0031-toolchain-is-the-builds.md)), and `dowel.toml` is where the build is described |
| language-server diagnostics that need fetching, `--target`, or external processes | the editor session is read-only and host-targeted by design; the remaining exclusions are listed with reasons in `dowel_lsp::UNSUPPORTED` |
| cleaning up artifacts left on target machines | deliberately not done ([ADR-0046](adr/0046-transfer-once.md)): it would undo the transfer skip, and the two cannot both be defaults. If wanted, it is an explicit command whose cost is that the next run transfers again |
| a native registry | Phase 5; the sources that exist are `path` / `git` / `url` (archive, [ADR-0029](adr/0029-tarball-dependencies.md)) and `version`, which delegates to pkg-config ([ADR-0015](adr/0015-version-deps-pkgconfig.md)) with `dowel.lock` recording its resolutions — a dowel-run registry, if ever wanted, is a separate future decision |
| automatic ABI label computation | Phase 6; today only `must_equal` verification of a hand-written `abi`. Nothing verifies that a surface declaring `abi = "c"` really is `extern "C"` — the claim is narrower and more checkable than a language label, and is what an IDL or a header scan would confirm ([ADR-0019](adr/0019-c-abi-label.md)) |
| computing an ABI label rather than reading a declared one | Phase 6. The **shape** is decided: a label is a set of components compared one by one ([ADR-0042](adr/0042-abi-label-components.md)), so a computed label can be matched against a declared one component by component. Two components exist (`libc`, `cxx_stdlib`); the rest of Q2's candidates — sanitizers, LTO, exception model, `_GLIBCXX_USE_CXX11_ABI`, MSVC runtime kind — each need their own evidence and domain |

## Divergences from the design documents

Points where the implementation departed from the documents, made explicit.
Whether to amend the documents is decided separately.

| Where | Document | Implementation | Reason |
|---|---|---|---|
| the consequence in [ADR-0003](adr/0003-manifest-split.md) | "there will be two parsers" | one parser; `dowel.toml` strictness is imposed by validation | the ADR's rationale (third-party tools read it without a custom parser) is equally satisfied by validation, and a single tree keeps provenance and diagnostics paths simpler |
| types | `defines : Map<Ident, Val>` | `Val` implemented as a type | the document's notation was adopted as-is |
| `abi` | ABI labels are computed | currently a hand-written string | computation is Phase 6; only the `must_equal` path is wired up |
| [30-devexp.md](30-devexp.md) section 1 | `args = ["-L", sysroot()]` in a `[runner]` | `sysroot()` is written in `flags` / `c_flags` / `cxx_flags` / `link_flags`, which are `List<Word>` ([ADR-0047](adr/0047-sysroot.md)); a runner's `args` is still `List<Str>` | the sysroot's use is on the compile and link lines, and that is where `Word` already was. A runner's `args` is a command line for a program on the build machine; widening it needs the runner's own package as a path base, which is a separate question |
| [50-development.md](50-development.md) section 3 | CI runs in a `--network none` container built from dotfiles | GitHub Actions runners (staying so for now) | the path for evaluating the dotfiles flake from this repository's CI is not set up, and there is no present need. The checks are defined solely in `scripts/verify.sh`, so a migration later swaps only the workflow's internals |
