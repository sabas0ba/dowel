# ADR-0061: Report rebuild reasons and evaluation reuse without running a build

**Status**: Accepted

## Context

Users need to inspect rebuild reasons and evaluation-cache reuse before running a build. Neither was available as a dedicated report.

A project was built, then one source was touched, then the same build was run
under `--log-level=trace`, the highest setting there is:

```console
$ dowel build --log-level=trace 2>&1 | grep exec
      5.6ms info  exec   ninja -f .../build.ninja
    138.7ms debug exec   loaded 4 recorded commands
```

Ninety-five lines of trace, and not one of them says why anything ran. Under
the default backend dowel hands the graph to ninja, and ninja's own
`-d explain` output is not asked for and not passed on. Under `--backend=direct`
the answer exists:

```console
      5.8ms trace direct   stale: .../src/core.c is newer than the output
     25.6ms trace direct   stale: .../obj/app/core/src_core.c.o is newer than the output
```

These reasons were mixed with other trace output and were only available from the direct backend. The default ninja backend did not expose them. Build ordering and progress reporting had already been made consistent across backends in [ADR-0056](0056-direct-backend-parallelism.md) and [ADR-0057](0057-progress-is-shown-while-it-runs.md).

Evaluation statistics were also unavailable to users. `dowel_query::Stats` has counted
`computed` / `cut_off` / `verified` / `hit` / `skipped` since the query layer was
written, and nothing outside the crate's own tests has ever read it.
`cache info` reports bytes and record counts, which answers how big the store
is, not what this run did with it.

Execution logs require a completed or ongoing build. A separate command is needed to inspect the current state before deciding to build.

## Decision

**`dowel status` reports planned file preparation, rebuild reasons, and evaluation statistics without executing the build.** It uses the same planning stage as `check`, then inspects existing files. It does not run a build step or backend, or write to the build directory.

Startup still reads manifests and updates the evaluation store, as `check` does. It also queries the compiler's target triple when the fact cache has no entry. The triple is needed to locate the configuration's build directory. Persisting evaluation results allows the next command to reuse them.

```console
$ dowel status
evaluation  1 recomputed, 8 unchanged after recomputing, 14 verified, 26 answered again, 0 skipped
preparation 1 planned, 1 would change
steps       4 planned, 3 would run

would prepare
  WRITE lib/core.map

would run
  CC obj/app/core/src_core.c.o  src/core.c is newer than the output
  AR lib/libcore.a              obj/app/core/src_core.c.o is rewritten by an earlier step
  LINK lib/libcore.so           the planned input changed (lib/core.map is rewritten)

up to date
  CC obj/app/app/src_main.c.o
```

The report separates manifest evaluation, preparation of files and symbolic links, and build actions. Planning describes preparations without writing them. This keeps the shared planning stage safe for `check` and `status`, which must not modify the build directory.

**Reuse the direct backend's freshness check.** `exec::staleness` returns the reason a step must run. Both the direct backend and the status report call it, so their rules cannot diverge through separate implementations. The function reads the command log maintained by `backend::run` after every backend, allowing status to inspect builds made with any backend.

**Three reasons belong to the report alone**, and they describe changes a
build has not performed yet:

- A tool stamp whose contents changed will be rewritten first
  ([ADR-0055](0055-tool-identity-in-freshness.md)), so every step reading it is
  already stale. The runner never sees this: by the time it judges, the stamp
  is written and the ordinary "newer than the output" catches it.
- A generated input whose planned contents changed will also be rewritten
  first. Export maps are the current case. They are written only when their
  contents differ, so an unchanged plan does not move their timestamp and
  relink a shared library on every build.
- A step is stale if another planned step will rewrite one of its inputs. During execution, the updated timestamp provides this information. Status must propagate the planned change because it does not execute the earlier step.

**A missing record is not a changed command.** The command log distinguishes
*no record for this output* from *a different command*, because they read
differently even though both rebuild: on a build tree that has never been
built, "the command changed since the last run" blames an edit nobody made.

## Consequences

- `dowel status` answers for dowel's own freshness rule. Ninja and make judge
  for themselves and could in principle disagree — ninja's `.ninja_log` and
  restat handling are its own. In the ordinary case they agree, because all
  three read the same file times and the command log is dowel's for all of
  them. Where they diverge, the divergence is worth knowing about and this is
  the tool that shows it.
- The report's propagation pass follows files a step actually reads, not every
  `deps` ordering edge. An order-only predecessor does not make a fresh step
  stale ([ADR-0056](0056-direct-backend-parallelism.md)). The pass repeats
  until nothing moves and converges in as many passes as the graph is deep —
  three for compile, archive, link — not in as many as there are steps.
- Reasons name a file relative to the build directory or the package root, and
  a path under neither is printed whole. Trimming a path against a root it is
  not under would name a different file.
- `--format=json` carries the same three stages, so a CI job can assert on "how
  many steps would run" rather than parsing build output. It is the same
  spelling `why` and `graph` already use.
- What this does *not* do is run anything, so it cannot report a failure a
  compiler would find. It answers what a build would attempt, not what it would
  produce.
- Reuse is reported as more than one number. `verified` (dependencies walked,
  nothing changed) and `answered again` (asked twice in one revision, answered
  from the memo) are both reuse and are not the same thing; on a small project
  the second is ten times the first, so folding them together would hide which
  one is carrying the run. `skipped` — durability said not to walk at all — is
  a third.
- The command exposes the existing query-layer counters without changing their definitions. These counts can be used to investigate unexpected recomputation on unchanged reloads.
