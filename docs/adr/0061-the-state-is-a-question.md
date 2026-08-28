# ADR-0061: What a build would do is a question, asked without doing it

**Status**: Accepted

## Context

Two questions get asked of a build system more than any other: *why did that
rebuild?* and *why is this not getting faster?* Measured against the
implementation, dowel could answer neither.

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

but it is a trace line in the middle of everything else, and it is only there
because that backend happens to make the decision itself. What a reader sees
depended on which backend ran — the thing
[ADR-0056](0056-direct-backend-parallelism.md) and
[ADR-0057](0057-progress-is-shown-while-it-runs.md) each closed one field over,
for ordering and for progress.

The incremental side is the same shape. `dowel_query::Stats` has counted
`computed` / `cut_off` / `verified` / `hit` / `skipped` since the query layer was
written, and nothing outside the crate's own tests has ever read it.
`cache info` reports bytes and record counts, which answers how big the store
is, not what this run did with it.

Both answers were also being asked for at the wrong moment. A log is what a
run left behind. The question is asked *before* deciding whether to run.

## Decision

**`dowel status` reports what a build would do, without doing it.** It plans
exactly as `check` does, then reads. No step runs, nothing in the build
directory is written, and no backend is consulted.

That is the claim, and it stops there deliberately. The command shares the
ordinary startup with every other one: the manifests are read, the evaluation
store is written as `check` writes it, and the compiler is asked for its triple
when the facts cache is cold. Suppressing those would not make the command more
of a question — the build directory's name is derived from the configuration,
so a `status` that will not probe cannot find the directory it is asked about,
and one that will not persist makes the next command re-evaluate what this one
just read. The line worth drawing is around the build: no step, no backend,
nothing written where the products live.

```console
$ dowel status
evaluation  1 recomputed, 8 unchanged after recomputing, 14 verified, 26 answered again, 0 skipped
steps       4 planned, 3 would run

would run
  CC obj/app/core/src_core.c.o  src/core.c is newer than the output
  AR lib/libcore.a              obj/app/core/src_core.c.o is rewritten by an earlier step
  LINK bin/app                  lib/libcore.a is rewritten by an earlier step

up to date
  CC obj/app/app/src_main.c.o
```

Two stages, because the two questions are about two different machines: the
manifest evaluation that produced the plan, and the actions the plan holds.

**The judgment is borrowed, not copied.** `exec::staleness` returns *why* a
step must run rather than a bare `bool`, and both readers call it: the direct
backend just before running a step, and this report instead of running one. A
report that carried its own copy of the rule would drift into naming reasons
nothing acts on and acting on reasons nothing names — the same defect
[ADR-0058](0058-a-command-a-backend-cannot-spell.md) found when a check and an
emission each carried their own copy of a condition. It sits in `exec`, beside
the command log, because it is dowel's rule and not one backend's: the log it
reads is written by the shared `backend::run` after *every* backend, so what
this reports does not depend on which one ran.

**Two reasons belong to the report alone**, and they are the two things a
build does before any judgment happens:

- A tool stamp whose contents changed will be rewritten first
  ([ADR-0055](0055-tool-identity-in-freshness.md)), so every step reading it is
  already stale. The runner never sees this: by the time it judges, the stamp
  is written and the ordinary "newer than the output" catches it.
- A step whose input another step is about to rewrite is stale too. The runner
  never has to find this either — the earlier step writes, the clock moves, and
  the next step notices by itself. A report that does not take that step
  forward says "up to date" about a file that is about to be overwritten.

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
- The report's propagation pass repeats until nothing moves. It converges in
  as many passes as the graph is deep — three for compile, archive, link — not
  in as many as there are steps.
- Reasons name a file relative to the build directory or the package root, and
  a path under neither is printed whole. Trimming a path against a root it is
  not under would name a different file.
- `--format=json` carries the same two stages, so a CI job can assert on "how
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
- The evaluation counts are reported as the query layer has always kept them.
  They make an unexpected number visible for the first time; what the numbers
  ought to be on an unchanged reload is a separate question, and one this makes
  askable.
