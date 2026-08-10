# ADR-0025: `bench` measures whole-process wall-clock time; no framework is imposed

**Status**: Accepted

## Context

The `bench` table kind was reserved from the start
([12-build-reference.md](../12-build-reference.md) section 2) but undefined.
Defining it forces a question that `test` never had to answer: **what does
running one mean?** For tests, C has a convention — exit status 0 is a pass —
and dowel could adopt it and impose nothing else
([ADR-0022](0022-test-cases.md), [ADR-0023](0023-harness-protocol.md)). For
measurement there is no such convention. Every C benchmarking framework
(google/benchmark, nanobench, hand-rolled loops) defines its own iteration
scheme, its own statistics, and its own output, and none of them is a
standard.

So "impose no framework" is not enough to define `dowel bench`; something
still has to be measured, and dowel has to choose what.

## Decision

`dowel bench` measures the **wall-clock time of the whole process**, from
start to exit, and reports **min and median** over a declared number of runs
(default 10, `--iterations`).

- `[bench.<name>]` builds an executable exactly as `test` does, and
  `[bench.<name>.cases]` registers several measurements of the same binary
  distinguished by arguments — the shape of ADR-0022, minus `should_fail`,
  which is refused: a benchmark is measured, not judged, so there is no
  verdict to invert. A harness (ADR-0023) is not accepted either; listing
  cases is a test-framework protocol, and no benchmarking convention
  matches it.
- Runs are always sequential. Measurement assumes a quiet machine, and two
  benchmarks run in parallel are each other's noise — there is deliberately
  no `--bench-jobs`.
- min and median are the reported statistics, the same pair
  `scripts/measure-startup.py` settled on: min approximates "what the code
  does when the machine does not interfere", median "what a user sees".
  The mean is not reported; it follows outliers.
- **Speed has no verdict.** A measurement fails only when a run could not
  be completed — nonzero exit, signal, timeout, launch failure — and then no
  numbers are reported at all: statistics over a partial series read as a
  finished measurement and are worse than none. Thresholds and regression
  gates are the user's policy, applied downstream on the JSON
  (`bench-result` lines, times in integer microseconds).

## Consequences

- Any binary is measurable with the same yardstick, framework or none. A
  project using google/benchmark can still run under `dowel bench` — the
  process-level number stays comparable across projects even then.
- The granularity is the process. Per-function timing, warm-cache loops,
  and statistical stopping rules stay inside the binary, where the
  framework that implements them lives. dowel does not read a framework's
  own numbers; that would mean parsing one output format per framework,
  which is the entanglement this ADR exists to refuse.
- Startup cost is part of every sample. For micro-benchmarks that is
  overhead to subtract; the practical shape is a loop inside the binary
  sized so the work dominates, which is what every framework does anyway.
- Cross execution measures the runner as well (qemu's translation, ssh's
  round trip). The number is honest for "how long does this take here" and
  meaningless as hardware time; the docs say so rather than pretending
  otherwise.
