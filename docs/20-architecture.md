# Architecture

> This is a design document about internals. As a user, the behavior you can
> rely on is covered by [60-cli.md](60-cli.md) (commands and the output
> contract) and [63-guides.md](63-guides.md) (working with the cache).

## 1. One core, multiple frontends

The CLI and language server share an incremental query engine and typed values with provenance. This lets both frontends reuse evaluation results and report source locations.

- The incremental evaluation engine = what the language server needs
  (partial re-evaluation per keystroke)
- Typed values with provenance = what the language server needs
  (go-to-definition, hover, diagnostics)
- `dowel build` and the language server pose different queries to the same
  query graph

The evaluator must support editor requirements from the start. The language server frontend can be delivered separately once those requirements are supported.

## 2. Evaluator requirements for editor support

The evaluator must support the following requirements. Adding them after implementation would require changes to parsing, value representation, and query execution:

1. **Error-tolerant parsing** — do not stop at syntax errors; keep partial
   trees and continue evaluating. The lossless CST is the source of truth;
   the AST is a projection of it
2. **Spans everywhere** — every value carries a source location, surviving
   string expansion and transformation
3. **Cancellability** — every keystroke cancels the previous query; every
   layer of the evaluator must propagate cancellation
4. **Guaranteed termination** — the reason for non-Turing-completeness is not
   simplicity of authoring but guaranteeing the language server always
   responds

## 3. Query catalog (initial draft)

| Query | Input | Output |
|---|---|---|
| `parse(file)` | file contents | CST |
| `eval(module)` | CST, parent scope | value environment |
| `resolve(target)` | name | target definition |
| `interface(target)` | target | merged propagated properties |
| `probe(toolchain, check)` | toolchain hash, check contents | fact |
| `plan(target, config)` | target, configuration | action graph |

### Required optimizations

- **Early cutoff** — if re-evaluation produces the same result as before, do
  not invalidate dependents. This is why a comment edit does not cascade into
  action-graph regeneration; it is the highest-value optimization in
  Salsa-style implementations
- **Durability layering** — manifests change often; toolchain facts almost
  never. Layers carry durability levels, and re-validation of stable layers
  is skipped entirely
- **Parallel evaluation and cancellation**

## 4. Value representation

```
Value = { type, data, provenance }
```

Each value includes its provenance. The query graph supplies the dependency relationships needed to trace how declarations contribute to a result.

## 5. Persistence (no daemon)

There is no resident daemon ([ADR-0002](adr/0002-no-daemon.md)). Query results must therefore persist on disk and be restored by subsequent processes.

### 5.1 Make restoration cost O(touched nodes)

Restore only the nodes a query needs. Serializing and restoring the entire graph would make startup cost grow with the full project size.

- An **mmap-able fixed-length record index** + an **append-only value log**
- The index holds only query-key hashes, output fingerprints, and offsets of
  dependency edges
- Node bodies are not read until needed. Validation is fingerprint comparison
  only, so most nodes are judged "unchanged" without reading their values
- mmap and the OS page cache allow subsequent processes to reuse cached index pages without keeping a daemon running

### 5.2 Change detection

File watching is unavailable, so changes are judged by a `stat` sweep over
the known input set.

- The key is `(mtime, size, inode, ctime)`; content is hashed only when they
  differ
- At a few thousand files, parallel `stat` takes single-digit milliseconds —
  the territory ninja has already proven
- mtime granularity and clock skew on network file systems are handled by
  falling back to hashing
- Validation propagates lazily: leaf changes are only marked, and dependents
  are validated when a query actually reaches them

### 5.3 Concurrent access from multiple processes

Required because the CLI and the language server touch the same store.

- The value log is append-only; in-progress writes are invisible to everyone
- Index updates are written to a temporary file and swapped in with an atomic
  `rename`
- The writer is limited to one (`flock`). A process that cannot take the lock
  computes in its own memory and writes nothing back. **Correctness is never
  lost — only the cached speedup**
- Invariant: a process dying at any point never corrupts the store

### 5.4 Startup and storage requirements

- **A startup-time budget** — target: under 10ms when there is nothing to do.
  Without a daemon, startup is paid on every run, so implementation-language
  choice, avoiding dynamic linking, and limiting files read at startup all
  matter
- **Store GC** — append-only means growth. The store lives under
  `.dowel/cache/` (gitignored) and `dowel cache gc` collects by generation
  count or size cap
- **A separate probe-fact DB** — toolchain-dependent facts are shared across
  projects, so they live in the user cache area
  (`$XDG_CACHE_HOME/dowel/facts/`), keyed by the tool's identity; the top of
  the durability hierarchy. Implemented for the questions that start a
  process ([ADR-0028](adr/0028-probe-facts.md)): what a compiler calls
  itself (`-dumpmachine`) and whether a generator answers `--version`.
  `try_compile`-style checks are not implemented; they would be built on the
  same key and storage

## 6. Where the language server stands

"No daemon" and a language server do not conflict. The distinction is who
starts it and how long it lives.

| | Started by | Lifetime | User's perception |
|---|---|---|---|
| daemon | implicit | outlives projects | unaware it exists; unclear how to stop it |
| language server | the editor | ends with the editor | something explicitly started |

Invariants:

- **The CLI never depends on the language server's existence.** Every feature
  produces the same result whether or not it is running
- The language server's in-memory graph is derived from the disk store; the
  disk is always the source of truth
- The language server does not hold the writer lock persistently, and results
  derived from unsaved buffers are never written to the store

## 7. Intended scale and limits

A daemonless design loses at extreme scale. Bazel / Buck2 adopt daemons
because at hundreds of thousands of nodes, index validation itself dominates.

The intended scale is 10^3–10^4 targets. Whole-monorepo single-graph usage is
not a goal.

## 8. Relation to a future execution layer

A future action cache can reuse content addressing, append-only logs, and fingerprint validation from the evaluation store. It would also need records describing action inputs and outputs.

## 9. Reducing cold configure

Configure time decomposes into three terms:

1. **Probe execution** — `try_compile` and friends; each one launches a
   compiler and linker process
2. **Discovery sweeps** — file-system walking by `find_package` / pkg-config
3. **Manifest evaluation and file writing**

The felt slowness comes mostly from 1 and 2, independent of implementation
language. 3 dominates only at thousands of targets.

The countermeasure: treat probe results not as an implicit cache
(`CMakeCache.txt`-style) but as an **independent fact database**.

- Keyed by toolchain hash + probe source + flags
- Shareable across projects and machines; as long as the same compiler is
  used, the same probe runs once
- Cross compilation becomes swapping the fact DB, shrinking hand-written
  cross files
- It also matters for reproducibility: today's probes depend on "the state of
  the host they ran on", an unrecorded input, which this promotes to an
  explicit one

The database is implemented ([ADR-0028](adr/0028-probe-facts.md)). Its key combines the tool's identity (path, size, mtime) with the probe request. A change to that identity produces a different key, so the old entry is no longer reused.

The first stored probe result was the compiler's target triple. Previously, dowel inferred the host triple from the OS and architecture used to compile dowel itself.
