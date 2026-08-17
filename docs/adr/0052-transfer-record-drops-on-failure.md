# ADR-0052: A run that did not pass drops the transfer record, not only a run that could not start

**Status**: Accepted

Revises the second decision of [ADR-0046](0046-transfer-once.md). Everything
else in that record stands: the record, its key, where it lives, and the
choice to leave artifacts behind.

## Context

ADR-0046 made the transfer skip self-healing, and named the case it was
healing:

> dowel cannot see the target machine, so it cannot know that someone wiped
> it; what it *can* see is the launch failing, and that is the only evidence
> available. Using it makes the skip self-healing at no cost: **a machine
> that lost the artifact recovers on the run after the one that noticed.**

Measured against a real target directory, that case does not produce a
launch failure (issue #160). dowel starts the *local* launcher — `ssh`, a
serial wrapper, `qemu` — and that starts fine. The missing artifact is
discovered on the other side, and what comes back is an exit status:

```json
{"kind":"test-result", "exit_status":1, "launch_error":null}
```

So `launch_error` fires for one thing only: the launcher named in `command`
is not on **this** machine. That is a configuration mistake — fix it once
and it never recurs. The condition the decision was written for — the target
machine's state changing — is the one that happens over and over: boards get
reflashed, `/tmp` is cleared, someone tidies up. In that case the record
survived, the next run skipped again, and the tree failed until the user
knew to delete `.dowel/build/<config>/transfers` by hand. The self-healing
worked everywhere except where it was aimed.

## Decision

**A run that did not pass drops the record for that transfer.** Not only a
run that could not start. dowel still does not look at the target machine;
it uses the strongest evidence it actually receives, and a nonzero exit is
that evidence.

The next run re-sends the same bytes, and the artifact is back — the
sentence ADR-0046 wrote is now true as written.

## Consequences

- **A test that keeps failing re-sends once per run.** This is the price,
  and it is bounded: the record is keyed on the transfer command, not on the
  case, so a target with twenty cases and one failure pays one transfer per
  run, not twenty. It is also mostly hypothetical during development —
  editing the code changes the artifact, and a changed artifact was always
  going to be sent.
- ADR-0046's `--failed` example still holds. Re-running one failing case of
  twenty sends the binary once for that run rather than not at all; the
  other nineteen cases still do not re-send, and a passing rerun writes the
  record back so the run after it skips.
- A `should_fail` test that fails as declared **passes**, and keeps the
  record. The signal is the outcome, not the exit status.
- Cleanup on the target machine stays unimplemented for ADR-0046's reason.
  This decision changes when dowel forgets, not what it leaves behind.
- dowel still cannot distinguish "the artifact is gone" from "the test
  failed". It does not try to: reading exit 127 as *file not found* would be
  a guess about a shell dowel did not run, and the two cases want the same
  thing anyway — send it again and find out.
- Nothing in the failure output says a transfer was skipped. It is in the
  debug log (`the artifact for X is already on the target`), and with the
  skip now self-correcting, the state a user could get stuck in is gone.
