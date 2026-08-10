# ADR-0023: The harness protocol is declared; dowel learns no test framework

**Status**: Accepted

## Context

[ADR-0022](0022-test-cases.md) took the `ctest` shape: a test target
registers its cases in the manifest. It also said the `cargo test` shape —
cases discovered *inside* the binary — stayed open, and that it would arrive
as a declaration resolving into the same job list.

The reason to want it is that a suite's cases live in the code. Writing them
a second time in `dowel.build` means two lists that drift, and the drift is
silent in the direction that matters: a case added to the source and not to
the manifest never runs, and nothing says so.

The reason not to build it the obvious way is that "ask the binary" needs an
agreement about *how* to ask, and C has no standard test harness. Criterion,
greatest, Unity, Check, and gtest each list and select differently, and
several cannot be driven at all without their own conventions. Teaching
dowel those conventions — `harness = "gtest"` and a table of framework
knowledge — would decide for every user which framework their tests may be
written in, which is what ADR-0022 declined to do.

## Decision

The protocol is **declared by the manifest**, not known by dowel:

```toml
[test.suite]
sources = glob("tests/*.c")

[test.suite.harness]
list = ["--list"]      # these arguments make it print the case names
run  = ["--run"]       # these, then the name, run one case
```

- `list` runs the binary and reads its **standard output: one case name per
  line**. Blank lines and lines starting with `#` are skipped. Nothing else
  is interpreted
- each name becomes a case labelled `<package>:<target>/<name>`, run as
  `<binary> <run...> <name>` — the name is **appended positionally**, as
  every other command dowel assembles is ([ADR-0008](0008-runner-transfer.md))
- `timeout`, `env`, and `labels` may be declared once and apply to the
  listing and to every discovered case
- `list` has no default. A harness that does not say how to list says
  nothing, and inventing a default would hide which question was asked
- `cases` and `harness` cannot both be declared. Both answer what the cases
  of a target are, and accepting both leaves the manifest silent about which
  one won (`conflicting-declaration`)

Discovery runs at test time, through the same launcher as the tests
themselves, so a cross build asks the binary through its runner. A listing
that fails, times out, or returns nothing is reported as a **failure of that
target**, not as zero tests: not being able to enumerate is not the same as
having nothing to run, and a silent zero is how a suite disappears without
anyone noticing.

## Consequences

- The cases come from the code. Adding one to the source is enough; there is
  no second list to keep in step
- What dowel knows is two argument lists. A framework whose listing is not
  one name per line — gtest's indented `--gtest_list_tests`, for example — or
  whose selection needs `--flag=NAME` rather than a separate argument, needs
  a few lines of wrapper. That is the deliberate cost of not encoding
  framework knowledge: the wrapper lives in the project that chose the
  framework, and dowel stays out of the business of tracking their flags
- Test listing is an external process at every `dowel test`. It is one launch
  per target, before any case runs, and it obeys the target's `timeout`
- Both shapes now exist and neither is privileged. `cases` suits a suite
  driven by arguments, or one whose parts want different timeouts and
  labels; `harness` suits a suite that already enumerates itself. The runner
  sees the same `Job` list either way, so selection, parallelism,
  `--failed`, and reporting were not touched
