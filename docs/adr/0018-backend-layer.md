# ADR-0018: The output stage is a backend layer over one neutral build graph

**Status**: Accepted

## Context

Planning ends with an action graph: a list of process launches with their
inputs, outputs, and ordering. Everything after that is a separate concern —
*who* runs those processes.

That separation was not expressed anywhere. `exec::run` matched on an
`Executor` enum with two arms; one arm wrote `build.ninja` and started ninja,
the other walked the actions in process. Both reached into `Plan` directly,
so every fact a runner needed was reachable whether or not it was part of any
stated contract, and adding a third way to run a build meant editing the
match rather than adding a file.

Two things were wanted, and neither had a place to go.

- **Runners other than ninja.** ninja is a good default and stays the
  default, but a project that already has a `make` invocation, a distributed
  execution service, or a tool of its own has no way in
- **A description a foreign tool can read.** `dowel graph --kind=action
  --format=json` already printed an action graph, but it was a *dump*: an
  unversioned convenience for reading, with no promise that it contained
  enough to actually run a build, and a second JSON shape to keep in step
  with whatever the executors happened to use

The second is the one that decides the design. A format that merely
*describes* what a build would do rots, because nothing fails when a fact
stops being written into it. A format that a real backend consumes cannot
rot, because the build stops working.

## Decision

The output stage is a layer. Between the planner and any runner sits one
neutral value, `BuildGraph`, and every backend consumes only that.

- `BuildGraph` holds the build directory, an ordered list of **steps**, the
  artifact of each target, and the default outputs. A step is one process
  launch: program, arguments, inputs, outputs, an optional depfile, and the
  steps that must complete first
- It carries no `TargetId`, no `Session`, and no planner state. Targets
  appear as their display labels — the same strings diagnostics use
- `Backend` is a trait: `emit` writes that backend's own input files, `run`
  executes them, `available` reports whether the environment has it, and
  `builds` says whether it produces artifacts at all. Backends are listed in
  one table; adding one is a new file and one row

Four backends ship:

| Name | What it does |
|---|---|
| `ninja` | writes `build.ninja` and runs ninja. The default where ninja exists |
| `direct` | runs the steps in process, sequentially, comparing mtimes and reading depfiles |
| `make` | writes `Makefile` and runs `make` |
| `graph` | writes `build-graph.json` and stops |

`graph` is the connection point for a backend that is not in this
repository. Its output is the serialization of `BuildGraph` itself, with a
`format` name and an integer `version`, and it can be parsed back into an
equal `BuildGraph`.

`dowel graph --kind=action --format=json` prints that same document. There is
one JSON description of an action graph, not two, and the one that exists is
the one the backends run on.

`--executor` is renamed to `--backend`. The old spelling is refused with a
message naming the new one rather than silently accepted, because the set of
values it takes has changed.

## Consequences

- **The format cannot quietly become insufficient.** `ninja`, `direct`, and
  `make` read `BuildGraph` and nothing else, so a fact missing from it is a
  broken build, caught by the existing end-to-end tests, not a documentation
  bug found later by whoever tried to write a backend
- **A backend outside this repository is a supported position.** It reads
  `build-graph.json` ([14-build-graph.md](../14-build-graph.md)) and needs no
  Rust, no linking against dowel, and no knowledge of manifests
- `make` is a real second generator, not a demonstration: it is what proves
  the layer is not shaped around ninja. It also has limits ninja does not —
  make cannot express a path containing whitespace, `:`, `#`, or `%` — and
  the backend refuses such a build with a diagnostic naming the path instead
  of writing a Makefile that silently builds the wrong thing
- A backend that does not build (`graph`) is a state the commands must
  handle. `dowel build --backend=graph` reports the file it wrote instead of
  claiming artifacts exist, and `dowel test` refuses it outright
- The record of "which command produced each output" is kept by the layer,
  not by a backend, so it stays consistent when backends are switched between
  runs — which was already true of `ninja` and `direct` and now holds for
  `make` for free
- The document is a compatibility surface. `version` is how it changes; a
  reader that does not recognize the version is expected to refuse rather
  than guess
