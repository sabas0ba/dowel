# ADR-0045: Offline is a mode the build is told to be in, not a state it happens to be in

**Status**: Accepted

Delivers the second half of Phase 5's "vendoring and offline builds"
([90-roadmap.md](../90-roadmap.md)), on top of the acquisition that
[ADR-0029](0029-tarball-dependencies.md) and
[ADR-0044](0044-toolchain-acquisition.md) put in place.

## Context

Everything a build acquires is fetched once and then reused: a git
dependency by `rev`, an archive by `sha256`, a toolchain by `sha256`. Each
writes a completion marker and later runs read it instead of the network.

So a build in a tree that has everything already *does* run without the
network — by accident. Nothing says so, nothing checks it, and nothing tells
you when it stops being true. Three things follow:

- **A missing input reads as a network failure.** On a machine with no route
  out, a build that needs one more dependency fails with curl's exit status,
  which describes the symptom and not the cause.
- **There is no way to prepare.** Making a tree ready for an isolated build
  means running a full build first, which also compiles everything — and if
  it fails for an unrelated reason you cannot tell whether the acquisition
  part finished.
- **There is no guarantee.** An air-gapped or audited build wants "this did
  not touch the network" as a property, not as a thing that was probably
  true.

## Decision

**`--offline` forbids acquisition; what is already present is used.** A
dependency or toolchain that is not there is `needs-fetch`, naming what is
missing, where it would come from, and that `dowel fetch` is how to get it.
It is a separate code from `unfetchable-dependency` deliberately: the cause
is different (nothing was tried) and so is the fix.

**`DOWEL_OFFLINE=1` does the same.** An isolated container or a CI job sets
it once; adding a flag to every command is how one command gets missed.

**`dowel fetch` acquires everything and stops.** Acquisition already happens
while the model is loaded (dependencies) and while the configuration is
assembled (the toolchain); this command is those two steps *without
building*. It lists what is now present, so "ready to go offline" is
something you can see rather than infer.

**The mode is process-wide, set once from argv**, like the logging level. It
is not threaded through the fetch functions: a new acquisition path would
then need to be wired up, and the one that was forgotten is the one that
reaches the network anyway.

## Consequences

- A tree plus its `.dowel/deps/` and the user's toolchain cache is a complete
  input set. That is what makes an image or an archive of a prepared
  workspace meaningful — and it is the substance of vendoring, without
  needing a second copy of the sources under a different name.
- `--offline` says nothing about `pkg-config`. Resolving a system dependency
  ([ADR-0015](0015-version-deps-pkgconfig.md)) starts a local process and
  reads local files; offline is about the network, and refusing a local
  lookup would make the flag mean "do less" rather than "do not reach out".
- The editor is unaffected. It never fetched
  ([ADR-0002](0002-no-daemon.md)), and it stays silent about what is missing
  rather than reporting `needs-fetch` — a diagnostic the reader cannot clear
  from inside the editor is worse than none.
- Nothing verifies the claim. `--offline` refuses dowel's own acquisition
  paths; it does not sandbox the compiler, and a build script — which dowel
  does not have — would not be covered either. The guarantee is about what
  dowel does, which is the part dowel can speak for.
- True vendoring, in the sense of committing dependency sources into the
  repository under a path the manifests resolve against, is not addressed.
  It is a different decision: it changes where a dependency comes from, not
  whether it may be fetched.
