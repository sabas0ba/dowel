# ADR-0037: Growth is reported by default, collected on request, and the budget follows the graph

**Status**: Accepted

Closes the GC part of [Q4](../99-open-questions.md).

## Context

Two things grow, and only one of them was being looked at.

**The value log** is append-only, which is what keeps the store from
breaking when a process dies mid-write. The cost is that overwriting a key
leaves the old bytes in place. `dowel cache gc` existed and collected only
*old format versions* — whole directories left behind when `FORMAT` moved —
so within the current format it freed nothing.

**The build directories** are per configuration: `.dowel/build/<cfg-id>/`,
one per (triple, configuration) pair. Switching between debug and release,
or building for a second triple, leaves the previous one behind with its
objects and binaries in it. This is the larger number by an order of
magnitude, and nothing collected it at all.

[20-architecture.md](../20-architecture.md) section 5 sketched "collects by
generation count or size cap". Neither is right as stated: there are no
generations inside a format version, and a fixed size cap is either too
early for a small tree or too late for a large one.

The first draft of this decision said growth costs only disk space, "the
cheapest resource in the list", and left collection entirely manual. That
understates it. Repeatedly switching configurations and rebuilding is
ordinary work, not misuse, and the result accumulates in a way the user has
no reason to expect. Worse, the only way to *notice* was to run `cache
info` — which nobody does unprompted.

## Decision

**Growth is reported by default.** When a run ends and the store is over
budget, one line says so and how to collect it. Being told is the part
that was missing; a user who never looks has no way to learn that looking
would help.

**The budget follows the graph: it is the live bytes themselves.** Over
budget means the dead bytes exceed the live ones. A tree's live size is
what that tree needs, so the threshold scales with the project instead of
being a number that fits one repository. An empty store is never over
budget — twice zero is zero, and the first write would otherwise trip it.

**`DOWEL_CACHE` chooses what happens:**

| value | behavior |
|---|---|
| `notify` (default) | report when over budget; collect nothing |
| `gc` | report and compact in place |
| `off` | say nothing |

The default reports rather than collects because compaction rewrites a
file, and a build that pauses to do so spends time its user did not ask
for ([20-architecture.md](../20-architecture.md) section 5.4). `gc` exists
for those who would rather have it handled, and `off` for those who have
decided.

**`gc` compacts the store**: the reachable records — one per key, each
naming an offset and a length in the index — are copied into a fresh value
log. Everything else is dead, and dead is the normal state of most of the
file.

**`gc --older-than=<days>` removes build directories not written in that
long.** This is the date-based half, and it applies where date makes sense:
a configuration nobody has built in a month is a configuration nobody is
using, and its contents regenerate. The age is *last written*, not
created, so a configuration built daily for a year is never a candidate.

Nothing removes build directories without being asked for a number.
"Everything but the current one" would delete the release tree of someone
who alternates between two configurations every day.

**Per-record ages are not recorded.** Evicting individual entries by age
would mean writing on every read to maintain a last-used time, to manage a
resource the whole-store budget already covers. The store's granularity
for age is the store.

**`cache info` reports both**: the store's dead bytes, and every build
directory with its size and age. The numbers that motivate collection are
visible before collecting.

**Collecting old format versions stays.** That answers Q4's migration
question: a format change moves `FORMAT`, the new version starts empty in
its own directory, and `gc` removes the old one. Nothing converts an older
format — misreading an old layout is worse than recomputing.

## Consequences

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
  costs a round of recomputation.
- Compaction takes the writer lock, so it does not run against a
  concurrent build; it reports that rather than waiting. Under
  `DOWEL_CACHE=gc` a failed lock is silent — the next run collects.
- The notice is on stderr, where progress goes, and is suppressed for
  `cache info` and `cache gc` themselves.
- Not addressed: sharing a store between projects, or a global cache keyed
  by content. Both change what the store *is*; this decides only how the
  ones that exist are collected.
