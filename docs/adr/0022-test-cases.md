# ADR-0022: A test target registers cases; dowel imposes no harness

**Status**: Accepted

## Context

`dowel test` built each `test` target, ran the binary once, and judged it by
exit status. One target was one test. Everything a test runner is usually
asked for — running a suite's parts separately, giving one part longer than
another, expecting a failure, selecting a subset — had no place to be said.

Two shapes were on the table, named after the tools that popularized them.

**`cargo test`** discovers cases *inside* the binary: the harness enumerates
`#[test]` functions, and the runner talks to it over a protocol (list the
cases, run one by name). It gives per-case results without the manifest
naming anything. It also requires a harness, and therefore agreement between
the runner and every test binary about how to be asked.

**`ctest`** registers cases *outside* the binary: the build description says
"run this command with these arguments, call it this name, give it this
timeout, tag it with these labels". Nothing is imposed on the program.

dowel's current documentation is explicit that no harness is imposed and the
C convention (exit status) applies. C has no standard test harness — there
are several, and they do not agree on how to list or select cases. Adopting
one protocol would decide for every user which framework their tests may be
written in.

## Decision

A `test` target may register **cases**, in the shape the other per-target
blocks already use:

```toml
[test.suite]
sources = glob("tests/*.c")

[test.suite.cases]
parse   = { args = ["parse"], timeout = 10 }
emit    = { args = ["emit"], labels = ["slow"] }
rejects = { args = ["bad"], should_fail = true }
```

- A case is **another invocation of the same binary**. It adds no translation
  unit; what distinguishes one case from another is `args`
- Its label is `<package>:<target>/<case>`, which is what the summary, the
  JSON output, and `--failed` use
- `timeout` kills the case and reports it as timed out, whatever exit status
  the kill produced
- `should_fail` inverts the verdict, and a case that was supposed to fail but
  exited 0 says so rather than reporting a bare "status 0"
- `env` sets variables for that case only
- `labels` are selected with `dowel test --label <name>`
- A target with **no** `cases` block stays exactly what it was: one test,
  named after the target

Case values are ordinary manifest values, so `match` / `when` apply — a
timeout may differ per configuration, which is the case that actually needs
it (a cross build under an emulator is slower than the host).

No harness protocol is adopted. dowel does not ask a binary what cases it
contains.

## Consequences

- Per-case results, selection, and timeouts arrive without deciding anyone's
  test framework. A project using its framework's own runner writes one case
  and passes its flags through
- The cost is that cases are written in the manifest. A binary with 200
  `#[test]`-like functions is not going to have 200 entries here — for that,
  one case per *group* is the shape, with the framework's own filter in
  `args`
- The `cargo test` shape stays open. It would be a declared protocol on the
  target (`harness = "..."` naming how to list and select), resolved into the
  same `Job` list this decision introduces. Nothing here forecloses it, and
  the runner now has a place to put the discovered cases
- `timeout` polls `try_wait`, because the standard library has no wait with a
  deadline and the core takes no dependencies
  ([ADR-0007](0007-implementation-language.md)). The kill reaches the child
  only; a test that spawns grandchildren leaks them, and that is documented
  rather than worked around
- `dowel test`'s runner API now takes a planned list of jobs rather than a
  list of targets. Selection (`--label`, `--failed`) happens on that list,
  because the unit being selected is the case, not the target
