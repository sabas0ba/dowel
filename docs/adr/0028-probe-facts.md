# ADR-0028: What was asked of a tool is recorded outside the project, keyed by the tool's identity

**Status**: Accepted

## Context

[20-architecture.md](../20-architecture.md) section 9 decomposes cold
configure into probe execution, discovery sweeps, and manifest evaluation,
and names the countermeasure: treat probe results not as an implicit cache
(`CMakeCache.txt`-style) but as an **independent fact database** — keyed by
toolchain identity, shareable across projects, and the top of the durability
hierarchy (section 5). Section 3's query catalog lists `probe(toolchain,
check)`.

Nothing of it existed. Two things followed.

**An unrecorded input.** dowel's host triple was assembled from the OS and
architecture it was compiled for — `x86_64-unknown-linux-gnu`, always. What
the machine's compiler *calls itself* was never asked. A user whose `gcc`
says `x86_64-pc-linux-gnu`, passing that spelling to `--target`, is treated
as cross-compiling and asked for a runner that cannot exist. The comment on
`default_triple` said as much: provisional, to be replaced by the probe-fact
DB.

**Repeated questions.** Every run re-asked whether ninja answers
`--version` — a process launch, on every `dowel build`, in every tree.

## Decision

Facts learned about a tool are recorded in the **user's cache area**, not
the project's:

```text
$XDG_CACHE_HOME/dowel/facts/v1/facts     one record per line, <key>\t<value>
```

- **Outside the project, because a fact belongs to the tool.** As long as
  the same compiler is used the answer is the same in every tree; under
  `.dowel/cache/` the same question is re-asked once per tree. The thing at
  the top of the durability hierarchy would be living in the most volatile
  place.
- **The key carries the tool's identity** — path, size, mtime — alongside
  the question. There is deliberately no invalidation mechanism: replace the
  tool and the key changes, so the old fact is simply never asked for again.
  `dowel cache gc` collects what is no longer reachable.
- **Only questions that start a process are recorded.** Scanning `PATH` is
  a few `stat` calls; recording it would cost more to keep honest than it
  saves. What gets recorded is `-dumpmachine` and `--version`.
- **A recorded resolution is still checked for existence.** A tool can be
  removed without `PATH` changing, and reporting an absent tool as present
  turns a clear diagnostic into an exec failure later.
- **"It did not answer" is a fact too.** `cl` has no `-dumpmachine`;
  without recording the silence, every run asks again.
- **Unwritable is not an error.** No lock, no failure — the same judgment as
  the store ([20-architecture.md](../20-architecture.md) 5.3): what is lost
  is the saving, not the answer.

The first reader is the host triple. `configure` asks the **host** C
compiler what it calls itself and uses that as `Config::host`; a target
matching either that name or the assembled approximation counts as the host.

## Consequences

- The host triple becomes a recorded input rather than a property of "the
  machine dowel was compiled on". A user's own spelling of their triple now
  works with `--target`.
- Cold configure loses a process launch per run once the facts are warm, and
  the effect compounds across projects — which is the point of putting them
  outside the tree.
- A fact file shared between dowel versions is possible; the format version
  in the path is what keeps a future format from being misread.
- mtime is the identity, not content. A tool rebuilt with identical content
  looks new (a re-probe, which is cheap); a tool replaced with the timestamp
  preserved looks old (a stale fact). The second is the real risk, and it is
  the same bet every build system makes about source files.
- This is not yet `try_compile`. The catalog's `probe(toolchain, check)`
  wants "does this flag work", which needs a compile in a temporary
  directory; the mechanism here — key, storage, sharing, invalidation by
  identity — is what that would be built on.
- The facts are not consulted by the language server, which starts no
  external processes at all ([20-architecture.md](../20-architecture.md) 6).
