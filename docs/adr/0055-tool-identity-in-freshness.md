# ADR-0055: A tool's identity is an input, recorded as a file the actions depend on

**Status**: Accepted

## Context

dowel decides what is out of date from two things: the files an action reads
and the command line it runs. Neither notices that the *program* changed.
The command line names `cc`; after an upgrade it still names `cc`.

Measured with a wrapper the manifest names as its C compiler:

```console
$ dowel build                  # mycc is `exec cc "$@"`
[2/2] LINK bin/app
$ # mycc is rewritten to `exec cc -DPATCHED "$@"`
$ dowel build --backend=direct --log-level=debug
      5.0ms debug direct       ran 0 steps, skipped 2 already up to date
```

Objects built by the old compiler stay current. The same holds for the
linker, the archiver, a separately declared assembler
([ADR-0050](0050-separate-assembler.md)), the `objcopy` of a transform
(issue #60), and the program a generation runs
([ADR-0054](0054-generated-sources.md)) — ADR-0054 named this as its own
consequence, and it was never specific to generations.

The project already knows how to name a tool's identity.
[ADR-0028](0028-probe-facts.md) mixes `facts::identity` — path, size, and
mtime — into the key of every recorded probe fact, precisely so that a
replaced tool is not answered for out of the old record. That identity was
never carried into the build's own freshness.

Two shapes were available for carrying it, and ADR-0054 had just measured
which one holds:

- **An edge in the plan.** The plan carries `deps` between actions. ninja
  does not read them: it emits `build <outputs>: <rule> <inputs>` and orders
  by file relations alone. An ordering that is not a file relation is not an
  ordering under the default backend.
- **A file.** Every backend already compares inputs against outputs, because
  that is the one thing all three of them do.

## Decision

**A tool's identity is written to a file, and that file is an input of every
action that runs the tool.**

```
<build>/tools/<name>-<digest>.stamp
```

The name is readable and the digest — eight hex of the SHA-256 of the
program string as written — is what keeps two declarations apart. `cc` and
`/usr/bin/cc` get separate stamps even when they resolve to the same file;
the contents are then equal, so nothing rebuilds.

The contents are `facts::identity` of the program **as resolved on `PATH`**,
not of the name. A different `cc` appearing earlier in `PATH` is a different
tool, and resolving is what makes that visible.

**The stamp is written only when its contents change.** Rewriting it moves
its mtime, and moving the mtime rebuilds everything that uses the tool —
which is exactly right when the tool changed and exactly wrong when it did
not. Writing happens in `backend::run`, before the backend starts: the stamp
is an input, and ninja refuses a build whose input has no rule to make it.

**Every action is stamped in one place.** The pass walks the finished plan
and attaches to each action the stamp for its own `program`. Attaching them
where each action is created would mean every future action kind starts out
unstamped, which is the state this ADR exists to leave.

The stamps travel in `build-graph.json` as `tool_stamps`, so a backend
outside this repository receives both the inputs and what has to be written
to satisfy them. A document without the field is one from before this
decision and runs without stamps.

## Consequences

- The first build after this change rebuilds everything once: the stamps did
  not exist, so they are newer than every object.
- A tool replaced within the same second, at the same size, is missed. The
  identity is path, size, and mtime at one-second resolution, which is what
  ADR-0028 chose so that a probe does not read tens of megabytes of
  compiler. This decision inherits that trade rather than reopening it; the
  case it misses is a tool swapped in place during the same second as the
  build that used it.
- `dowel check` resolves the tools but writes nothing. Planning already
  stats what it decides from; only a run that intends to build writes.
- The identity is of the program dowel launches, which for `cc` is a driver.
  Replacing the compiler *behind* an unchanged driver — a new `cc1` under
  the same `gcc` — is not seen. What would see it is the version the prober
  already asks for, and making that an input means running the tool at plan
  time for every build; that is a separate decision with a startup budget
  attached ([20-architecture.md](../20-architecture.md) 5.4).
- A tool that is absent is stamped as absent. The plan already reports
  `missing-toolchain` or `missing-generator` and the build stops, but
  recording the absence is what makes the build after the tool is installed
  rebuild rather than trust objects that were never made.
- Freshness is now sensitive to a tool being reinstalled without changing —
  a package manager rewriting the same bytes moves the mtime and rebuilds.
  That is the same false positive every mtime-based build system has for
  sources, applied to tools.
