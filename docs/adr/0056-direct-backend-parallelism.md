# ADR-0056: The direct backend runs steps concurrently, ordered by both edges and files

**Status**: Accepted

## Context

`dowel build` takes `-j, --jobs <n>`, documented as "parallelism, passed to
the backend" ([60-cli.md](../60-cli.md)). The ninja and make backends pass it
through. The direct backend took it and dropped it on the floor:

```rust
fn run(&self, g: &BuildGraph, _jobs: Option<usize>) -> Result<(), Failure> {
```

That backend is not a curiosity. `backend::select` falls back to it whenever
ninja is not on `PATH` ([ADR-0018](0018-backend-layer.md)), which is exactly
the state of a machine right after `dowelup install` — dowel's own point is
that a first build should not require a toolchain to be assembled first
([ADR-0036](0036-prebuilt-distribution.md)), and it was then handing that
machine a single-threaded build.

Measured on four cores, nine translation units with heavy C++ headers:

| | first build |
|---|---:|
| `--backend=direct -j 1` | 4144ms |
| `--backend=direct -j 2` | 1921ms |
| `--backend=direct -j 4` | 1028ms |
| `--backend=ninja` | 1251ms |

The concurrency mechanism was already in the repository and needed no new
dependency: `dowel test` runs its cases with `--test-jobs` over
`std::thread::scope`.

## Decision

**The direct backend schedules steps concurrently, with `--jobs` deciding how
many run at once.** The default is `std::thread::available_parallelism`;
compiling saturates the CPU, so more workers than that only lengthen the
queue. When that cannot be read, the default is one — a slow build beats a
guess on a machine that cannot say.

**A step waits for both the edges the graph declares and the files it
reads.** The scheduler unions `deps` with "the step that writes this input".
Sequentially, either alone sufficed: `order()` topologically sorted by
`deps`, and running in that order also happened to satisfy the file
relations. Concurrently, an ordering that appears in only one of the two is a
race. It is also what makes a graph read back from `build-graph.json` safe to
run: that format's own reference says `deps` is "usually implied by `inputs`,
but not always", which promises nothing in the other direction.

**Freshness is decided when a step is about to run, not when it is
scheduled.** A predecessor may have just rewritten an input. This is what the
sequential loop already did, and it is the reason the decision cannot be
hoisted out of the worker.

**The first failure stops scheduling; steps already running finish.** Killing
them would lose the diagnostic that is being written at that moment, and the
build is failing either way. The first failure is the one reported —
concurrently, "first" means the first to be recorded, which is the one the
user is shown.

**Steps the schedule never reaches are run afterward, sequentially.** A cycle
leaves its members with prerequisites that never settle. `order()` already
chose to append such steps rather than drop them, on the grounds that a step
that silently never runs is worse than one that runs in a bad order; that
choice is preserved here rather than reopened.

## Consequences

- Output interleaves by completion rather than appearing in graph order.
  That is what every parallel build does, and the logger locks stderr per
  line, so lines do not tear. With `-j 1` the order is still `order()`'s,
  because the ready queue is seeded in that sequence.
- The direct backend is now *faster* than ninja on a small build (1028ms vs
  1251ms above): it writes no build file and starts no second process. It is
  still not the default — that decision belongs to
  [ADR-0018](0018-backend-layer.md) and rests on ninja's scheduling being
  better on large graphs, not on this measurement.
- A race in the graph now surfaces as a flaky build rather than being masked
  by sequential execution. Unioning the two orderings is what keeps dowel's
  own graphs safe; a graph from outside that declares neither kind of
  ordering was already wrong and is now visibly so.
- `--jobs` still means different things per backend — ninja and make hand it
  to their own scheduler. That was already true and is not changed here.
- Nothing limits memory. A link step and eight compiles can run together on
  a machine that cannot hold them; ninja's pool mechanism is the answer to
  that, and expressing pools is a separate decision the graph format does not
  yet carry.
