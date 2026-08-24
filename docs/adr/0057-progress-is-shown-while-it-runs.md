# ADR-0057: Progress is output, shown while the build runs, one line per step

**Status**: Accepted

## Context

The same build looked like three different things depending on which backend
ran it:

```console
$ dowel build --backend=ninja     $ dowel build --backend=make     $ dowel build --backend=direct
[1/10] CXX obj/p/p/src_f3.cc.o    CXX obj/p/p/src_f1.cc.o          built: …/bin/p
[2/10] CXX obj/p/p/src_f6.cc.o    CXX obj/p/p/src_f2.cc.o
…                                 …
[10/10] LINK bin/p                LINK bin/p
built: …/bin/p                    built: …/bin/p
```

The direct backend said nothing at all. It announced each step with
`log_info!`, and the default log level is `warn`, so the announcement never
reached anyone. That backend is the fallback wherever ninja is absent
([ADR-0018](0018-backend-layer.md)) — the state of a machine right after
`dowelup install` — so the case where dowel is most likely to be new to
someone is the case where it builds in silence.

Measuring the other two turned up something worse. `drive` ran the generator
with `Command::output`, which waits for the process to exit, and only then
replayed its captured stdout. Timestamping each line of a 1.3-second build:

```
03:53:33.963  [1/10] CXX obj/p/p/src_f3.cc.o
03:53:33.965  [2/10] CXX obj/p/p/src_f2.cc.o
…
03:53:33.982  built: …/bin/p
```

Eleven lines inside nineteen milliseconds, at the end. **No backend showed
progress while the build was running.** For a build of any size, `dowel
build` was indistinguishable from a hang — and the comment above `drive`
claimed the opposite ("進捗は stdout に出るのでそのまま見せる").

[ADR-0056](0056-direct-backend-parallelism.md) made this matter more: steps
now finish out of order, which is exactly when a reader needs a count to
follow along.

## Decision

**Progress is output, not a log.** It is written to stderr as it happens,
without passing the level filter. `--log-level=off` silences it, because that
is the only knob a user has for silence; every other level shows it. stderr
rather than stdout keeps `dowel graph --format=dot | dot` clean, which is the
split [60-cli.md](../60-cli.md) already promises.

**It is live.** `drive` pipes the generator's stdout and forwards it line by
line while the process runs, instead of collecting it and replaying it after.
The child's stderr is read on its own thread: reading both in sequence
deadlocks the moment one pipe fills.

**Every backend that builds prints one line per step, in the same shape.**
The direct backend prints `[n/m] <description>`, matching ninja's spelling,
where `n` counts the steps it has run and `m` is the number of steps in the
graph. The number is assigned when a step *finishes*, under the same lock
that records it, so the numbers arrive in order and match the lines.

**The count belongs to whoever schedules the steps.** dowel supplies it for
`direct`, ninja supplies its own, and `make` has none — make does not expose
a running index, and the usual `$(eval)` counter trick is GNU-only, which a
generated Makefile should not require. What is unified is the line: one per
step, live, on stderr, description-last.

**A failure does not repeat what already scrolled past.** A `Failure` from
`drive` carries an empty `stdout`, because those lines were shown as they
arrived. It still carries the child's stderr, which was not.

## Consequences

- An incremental build under `direct` stops short of `m`: only the steps that
  ran are printed, so a two-step rebuild of a ten-step graph reads
  `[1/10]`, `[2/10]`. `m` is the size of the build, not a forecast of how
  much of it needs doing. Predicting the latter means deciding freshness
  before running, which ADR-0056 deliberately does not do.
- The three backends still differ in *ordering* and in whether a counter
  appears. That is a property of who is scheduling, and pretending otherwise
  would mean inventing a number dowel cannot stand behind.
- Warnings from a command that succeeded are still not shown under `direct`
  — it captures each step's output and prints it only on failure. Under
  ninja and make they now appear live, because those runners pass them
  through. That difference predates this decision and is left where it is.
- The generator's output is no longer available to dowel as a string, so
  nothing downstream can parse it. Nothing did; `/showIncludes` folding is
  the direct backend's own path ([ADR-0027](0027-toolchain-style.md)) and is
  untouched.
