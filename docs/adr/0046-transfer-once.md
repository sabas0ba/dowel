# ADR-0046: An artifact is transferred once per destination, and a run that could not start is what sends it again

**Status**: Accepted

Extends [ADR-0008](0008-runner-transfer.md), which decided how an artifact
reaches a target machine. This decides how often.

## Context

A `[runner.<triple>]` with `transfer` copies the artifact before every run.
It runs on the second `dowel test` of an unchanged tree, on every case of a
target with twenty cases, and on `--failed` reruns of one of them.

For SSH to a board on a desk, or a serial link, the copy is frequently longer
than the test. dowel is careful about not recompiling what did not change and
then re-sends the result of that unchanged compilation every time.

The obstacle is that the destination is a machine dowel does not control.
Locally, freshness is decided by looking at the output file; remotely,
looking costs a round trip — which is the thing being avoided.

The row in [91-implementation-status.md](../91-implementation-status.md)
asked for two things together: "cleaning up artifacts left on target
machines; skipping redundant transfers". **They contradict.** Skipping
requires the previous copy to still be there. Cleaning up guarantees it is
not, so every run transfers again.

## Decision

**dowel records what it sent and where, and skips a transfer when both are
unchanged.** The record is `<build-dir>/transfers`: the fingerprint of the
artifact against the transfer's full command line. The command line is the
key because two destinations are two transfers, and a different way of
sending can produce a different result.

**A run that could not start drops the record**, so the next one sends
again. This is the honest half of the decision. dowel cannot see the target
machine, so it cannot know that someone wiped it; what it *can* see is the
launch failing, and that is the only evidence available. Using it makes the
skip self-healing at no cost: a machine that lost the artifact recovers on
the run after the one that noticed.

**Artifacts are deliberately left behind.** Cleaning them up would undo the
skip, and the two cannot both be defaults. Between "the target accumulates
files in one directory" and "every run pays the transfer again", the first is
the smaller cost, and it is the one a person can undo by hand. A cleanup step
would have to be asked for.

**The record lives in the build directory**, so it is per configuration and
it dies with it — `cache gc --older-than` ([ADR-0037](0037-store-gc.md)),
or deleting `.dowel/build`, resets the assumption. There is no separate
switch, because a stale record is already reachable through the mechanism
that exists for stale build state.

## Consequences

- The wager is stated rather than hidden: dowel assumes it is the only thing
  writing to `remote_dir`. If something else replaces the artifact there, and
  the replacement still launches, dowel will not notice. The recovery is to
  remove the build directory, which is the same recovery as for any other
  stale build state.
- A fingerprint that cannot be taken (an unreadable artifact) means no record
  is written, so the next run transfers. The failure mode is an extra copy,
  never a skipped one.
- `dowel test --failed` on one case of twenty no longer re-sends the binary
  the other nineteen already delivered — the record is keyed on the artifact
  and the command, not on the case.
- Nothing changes for a runner without `transfer`. The target machine reads
  the build tree directly, and there is nothing to send.
- Cleanup on the target machine remains unimplemented, now for a reason
  rather than by omission. If it is wanted it is an explicit command, and its
  documented cost is that the next run transfers again.
