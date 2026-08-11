# ADR-0037: The store is collected by compaction when asked, never automatically, and has no size cap

**Status**: Accepted

Closes the GC part of [Q4](../99-open-questions.md).

## Context

The value log is append-only, which is what keeps the store from breaking
when a process dies mid-write. The cost is that overwriting a key leaves
the old bytes in place: a project rebuilt a thousand times carries a
thousand versions of every manifest's evaluation, and only the newest of
each is reachable from the index.

`dowel cache gc` existed, and collected only *old format versions* —
whole directories left behind when `FORMAT` moved. Within the current
format it freed nothing, so the file that actually grows was the one
nothing collected.

[20-architecture.md](../20-architecture.md) section 5 sketched "collects by
generation count or size cap". Neither survives contact with what the store
is: there are no generations inside a format version, and a size cap
requires deciding *which* entries to drop.

## Decision

**`gc` compacts: it copies the reachable records into a fresh value log
and replaces the old one.** Reachability is exactly what the index says —
one record per key, each naming an offset and a length. Everything else is
dead, and dead is the normal state of most of the file.

**Compaction happens only when asked.** No threshold, no ratio, no
compaction on write. A build that silently pauses to rewrite a large file
is a build that violates the startup budget
([20-architecture.md](../20-architecture.md) section 5.4) in a way its user
cannot predict, and the cost of *not* compacting is disk space — the
cheapest resource in the list.

**There is no size cap, and this is a decision rather than an omission.**
A cap means evicting live entries, which means ranking them, which means
recording when each was last used — a write on every read, to manage a
resource that is not scarce. The store is a cache: dropping it costs
recomputation and nothing else. A user who finds it too large can run `gc`,
or delete `.dowel/cache/` outright, and both are safe.

**`cache info` reports the dead bytes**, so the number that motivates `gc`
is visible before running it. Reporting it is cheap: the index already
carries every live extent, so dead is the file's length minus their sum.

**Collecting old format versions stays.** That is the other half of `gc`
and answers Q4's migration question: a format change moves `FORMAT`, the
new version starts empty in its own directory, and the old directory is
removed the next time `gc` runs. Nothing tries to read or convert an older
format — misreading an old layout is worse than recomputing.

## Consequences

- The store still only grows during normal use. That is intended: the file
  is under `.dowel/cache/`, is gitignored, and losing it costs nothing but
  time.
- Compaction holds the writer lock, so it cannot run against a concurrent
  build; a process that cannot take the lock reports it rather than
  waiting. The same rule already governs writing.
- **Compaction rewrites offsets, so the index and the value log have to
  change together — and `rename` is atomic per file, not across two.** An
  index that survives while the log beneath it is replaced points at the
  right offsets in the wrong file, and every record it names reads as
  plausible garbage. That is the one outcome the store must not produce.

  The order therefore removes the index *first*:

  1. delete `index` — the store now reads as empty, which is a state it
     already handles
  2. rename the compacted log over `values`
  3. write the new index and rename it into place

  A crash at any point leaves an empty or stale-but-shorter store, never a
  wrong one. Being a cache is what makes this affordable: the worst case
  costs a round of recomputation, and the invariant
  ([20-architecture.md](../20-architecture.md) section 5.3) holds without
  a transaction across two files.
- Not addressed: sharing a store between projects, or a global cache
  keyed by content. Both change what the store *is*; this decides only
  how the one that exists is collected.
