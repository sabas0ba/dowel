# Semantics: how a manifest becomes a build

What happens between saving a manifest and running an artifact. Syntax and
the property tables are in [12-build-reference.md](12-build-reference.md);
this page defines what the declared values *do*.

## 1. The pipeline

```
parse → evaluate → specialize → propagate & merge → plan → execute
```

| Stage | Input | Output | Runs when |
|---|---|---|---|
| parse | file text | lossless syntax tree | file content changed |
| evaluate | tree | typed values with provenance | file content changed |
| specialize | values + configuration | concrete values (no `Cfg<T>` left) | per configuration |
| propagate & merge | per-target blocks + dependency graph | one merged property map per target | when the span-free summary changed |
| plan | merged maps | action graph, `build.ninja`, `compile_commands.json` | per build |
| execute | action graph | artifacts | per build |

Two consequences worth knowing:

- Switching `--config` / `--target` / `--features` re-runs specialization
  onward, never evaluation. Editing a comment re-parses the file but stops
  there: the per-target summary is fingerprinted without spans, so merging
  and planning are not re-run (early cutoff)
- `dowel check` runs the same pipeline through planning and stops before
  executing, which is why it reports the same configuration diagnostics as
  `build` ([ADR-0010](adr/0010-check-scope.md))

## 2. Loading and the dependency graph

Loading starts at the package in `--directory` and follows dependencies
transitively. For each package, `dowel.toml` is read
([11-toml-reference.md](11-toml-reference.md)), then `dowel.build` defines
its targets.

- `path` dependencies resolve relative to the declaring package
- `git` dependencies are fetched during loading (so `check` fetches too —
  it needs the dependency's manifests). Fetching delegates to the `git`
  command and lands in `.dowel/deps/<name>-<rev12>/`, marked complete
  atomically; because the rev pins the content, later runs reuse the
  checkout without touching the network. Deleting `.dowel/deps` is safe but,
  unlike the cache, rebuilding it needs the network again
- `version` dependencies resolve during loading through the system
  pkg-config ([ADR-0015](adr/0015-version-deps-pkgconfig.md)): the module
  must exist and satisfy the declared **minimum** version, and its
  `--cflags` / `--libs` appear as the public interface of a synthetic
  library node. Each resolution is reconciled against `dowel.lock` — a
  changed environment warns with `lockfile-drift` instead of being silently
  used. Editor sessions skip this: the language server starts no external
  processes

- Feature flags are resolved **per package**
  ([ADR-0017](adr/0017-feature-forwarding.md)). The root starts from
  `--features` plus its `default` (unless `--no-default-features`) and
  closes over its own `[features]`; a value spelled `dep/feat` enables
  `feat` in that dependency instead. `feature.<name>` inside a package's
  `dowel.build` asks whether that name is active **in that package**, so two
  packages may use one name for unrelated things. An inactive `optional`
  dependency is not loaded at all — no node, no edge. Because a forwarded
  feature can activate an optional dependency, and that changes what gets
  loaded, loading and feature resolution repeat until the requested sets
  stop growing
- Edges come from `deps` properties: `dep("name")` points at a package
  dependency's library target, `target("name")` at a sibling target.
  Referencing a package not declared in `dowel.toml` is
  `undeclared-dependency`
- The graph must be acyclic; a cycle is reported with the path that closes
  it
- Targets are ordered topologically; every downstream stage walks that order

## 3. Specialization

Specialization turns `Cfg<T>` values into `T` for one configuration:

- `match` looks up its key in the configuration and picks the matching arm
  (or `_`). The chosen arm is recorded in the value's provenance
- A `when` whose predicate is false makes the value disappear: an element
  vanishes from its list or map; a whole conditional value contributes
  nothing. A true `when` unwraps to the inner value, recording the predicate
- Specialized values carry no conditions; downstream stages never see a
  `match` again

`dowel why` shows both sides: which arm was chosen, and what a false
predicate dropped (`DOWEL_LOG=trace` logs each decision as it happens).

## 4. Propagation and merging

Each target's declared blocks combine with its dependencies' interfaces.
Two derived maps exist per target:

```
interface(T)   = public(T)  +  interface of each target in public(T).deps
compile_env(T) = public(T)  +  private(T)  +  interface of every dependency of T
```

This pair is the meaning of `public` / `private`:

- Everything `public` — includes, defines, flags, `deps` — is visible to
  dependents, transitively
- Everything `private` affects only the target's own compilation. A
  `private` dependency's interface reaches *this* target but is invisible to
  targets that depend on it

**Order**: values arrive self-first, dependencies after — the order include
search and linking expect. Within dependencies, the graph's topological
order applies.

**Merging** happens per property, under the rule declared in the schema
([12-build-reference.md](12-build-reference.md)):

| Rule | Behavior |
|---|---|
| `union` | duplicates dropped, arrival order kept. Two equal-looking paths from *different packages* are **not** duplicates — a path's base point is the package that declared it, so `dir("include")` in two packages names two directories |
| `append` | concatenation, duplicates kept |
| `error_on_conflict` | per map key: the same value may arrive many times, but two different values for one key fail (`merge-conflict`) with both provenance chains in the diagnostic |
| `must_equal` | all arriving values must be identical or the build fails (`abi-mismatch`). This is the whole ABI check today: `abi` labels are compared before linking, turning a would-be runtime ODR breakage into a build failure |
| `replace` | last arrival wins (used by runner properties, which do not propagate) |
| `max` | the highest value in the vocabulary's order wins. Used by `c_std` / `cxx_std`: a library requiring C++17 consumed by a C++20 binary is correct, and a library requiring C++20 raises a consumer that asked for less ([ADR-0016](adr/0016-language-standard-property.md)) |

Nested lists are flattened completely during merging — a `match` written as
a list element produces a list-in-a-list when specialized, and one level of
flattening would silently drop it downstream.

Every merged value keeps the full provenance chain, which is what
`dowel why <target> <property>` prints:

```
include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

## 5. Planning

Planning turns merged property maps into an action graph.

- **glob expansion happens here**, not during evaluation — expanding earlier
  would make "the file system at evaluation time" an unrecorded input.
  The walk starts at the declaring package's root, prunes dot-directories
  and `target/`, and sorts results lexicographically so the outcome does not
  depend on traversal order. A pattern matching nothing, or a `sources`
  entry that is a directory or missing, is diagnosed (`empty-glob` /
  `invalid-source` / `unresolved-path`) instead of surfacing as a linker
  error
- **Paths resolve** against their base point (the declaring package's
  root). Path values never concatenate as strings
- **Actions**: one compile per source (with `-I` / `-D` / flags from
  `compile_env`), one archive per `lib` (`ar`), one link per `bin` / `test`
  (own objects, dependency archives in graph order, `link_flags`), and one
  transform per entry of an `artifacts` block, run after the artifact it
  derives from (`<tool> <args...> <input> <output>`, issue #60)
- **`link_flags` ride the link closure**, `private` included: a static
  archive cannot carry its own link requirements, so the flags a library
  declares (or a `version` dependency's `--libs` brings in) reach the final
  link the same way its archive does — across `private` edges. The
  `public` / `private` split controls **translation** propagation
  (`includes` / `defines` / `flags`); it does not control link
  reachability. A library can keep a system dependency's headers private
  and still be linkable (issue #56)
- **The compiler is chosen per source by extension**: C++ extensions
  (`.cc` `.cp` `.cpp` `.cxx` `.c++` `.CPP` `.C`) compile with the C++
  toolchain (`[toolchain] cxx`, default `c++`), everything else with the C
  toolchain (`[toolchain] c`, default `cc`). `flags` apply to both
  languages; `c_flags` / `cxx_flags` follow them and reach only their own
  language, so a per-language flag can override a shared one
- **The linker follows the closure**: if any translation unit in a
  binary's link closure is C++ — even deep inside a dependency library —
  the link runs through the C++ driver, so the C++ runtime is present.
  A pure-C closure links with the C driver
- A toolchain absent from PATH is `missing-toolchain` at plan time, not a
  cryptic exec failure later. The C++ toolchain is only probed when C++
  sources are actually present, so pure-C builds never require one
- Everything lands in a per-configuration build directory
  `.dowel/build/<triple>-<opt>[-<features>]/` (`obj/`, `lib/`, `bin/`), so
  switching configurations never clobbers artifacts. The identifier is
  folded to a single path component — any character that is not
  `[A-Za-z0-9_.+-]` becomes `--`, so a feature name containing `/` cannot
  split one configuration across two directory levels (issue #68). The
  folding is not reversible and does not need to be; it only has to keep
  distinct configurations distinct, which the two-character replacement
  does (`a/b` and `a-b` do not collide). `compile_commands.json` is
  written on every build unless `--no-compdb`

## 6. Execution

- Planning ends at a build graph; who runs it is a backend
  ([ADR-0018](adr/0018-backend-layer.md)). The default generates
  `build.ninja` and runs ninja. `--backend=direct` runs the steps
  sequentially in-process, judging freshness by mtime, depfiles, and the
  command line itself (a flag change reruns the step even though no input
  file changed). `--backend=make` generates a `Makefile`, and
  `--backend=graph` writes the graph itself
  ([14-build-graph.md](14-build-graph.md)) without building
- Every backend receives the same graph and nothing else, so which one runs
  is not a semantic choice. The record of which command produced each output
  is kept outside the backends and therefore survives switching between them
- `dowel test` runs each test binary with the package root as working
  directory and judges by exit status; verdicts persist in the build
  directory to serve `--failed`
- With `--target=<triple>` different from the host, the launch goes through
  the declared `[runner.<triple>]`
  ([12-build-reference.md](12-build-reference.md)). With `transfer` /
  `remote_dir` declared, the artifact is copied first; the implementation
  appends source and destination:

  ```
  scp -q <build>/bin/unit_test board.local:/tmp/dowel/unit_test
  ssh board.local /tmp/dowel/unit_test
  ```

  The exit status of the launch command is the verdict. No runner declared
  for a foreign triple is a diagnostic *before* launch — afterwards it would
  be an `Exec format error` blamed on the test

## 7. Incrementality and the store

Evaluation results are memoized in-process and persisted in
`.dowel/cache/`; unchanged files (judged by stat, then content fingerprint)
are not even re-lexed on the next run. All of this is invisible to
semantics: deleting the store, losing the writer lock, or corrupting the
cache changes speed only, never results. Details are in
[20-architecture.md](20-architecture.md); observable behavior (verdicts,
restore counts) appears under `DOWEL_LOG=debug` / `trace`
([91-implementation-status.md](91-implementation-status.md)).

## 8. Diagnostics as part of the semantics

Any rule on this page that says "fails" produces a located diagnostic with a
stable code (`merge-conflict`, `abi-mismatch`, `undeclared-dependency`,
`missing-toolchain`, …), notes carrying the provenance of the offending
values, and — where a fix is mechanical — a fix suggestion that can be
applied from `--message-format=json`. The full code list, with the minimal
input that triggers each, is the case table in
`crates/dowel-cli/tests/diagnostics.rs`, and the coverage check keeps it
complete.
