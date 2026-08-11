# ADR-0036: Prebuilt binaries come from release assets, verified by hash; the source build stays the one that proves its origin

**Status**: Accepted

Closes [Q10](../99-open-questions.md).

## Context

`dowelup` builds from source ([ADR-0013](0013-self-acquisition.md)), which
requires a Rust toolchain on every machine that wants dowel. For a build
system whose users are C and C++ programmers, that is a strange thing to
demand before the first build.

Three things had to be decided: where prebuilt binaries live, how they are
verified, and how a binary relates to the commit sha that `dowelup` treats
as the source of truth.

## Decision

**Release assets on the upstream repository.**

```
<upstream>/releases/download/<tag>/dowel-<tag>-<triple>.tar.gz
<upstream>/releases/download/<tag>/dowel-<tag>-<triple>.tar.gz.sha256
```

The upstream is already a URL that `--upstream` and `DOWELUP_UPSTREAM` can
redirect, so the prebuilt location moves with it and a fork needs no extra
configuration. A separate endpoint would add availability, certificates,
and cost to operate, for nothing the repository host does not already do.

**Prebuilt binaries exist for release tags only.** `stable` and `X.Y.Z`
can find one; `nightly`, `branch:`, and a bare sha cannot, and fall back to
building. Publishing an asset per commit would mean a build farm and a
storage policy, and the specifiers that name a moving target are the ones
whose users are closest to the source anyway.

**Verification is SHA-256 against the `.sha256` beside the asset — and
this detects corruption, not tampering.** Whoever can replace the tarball
can replace the checksum next to it. The honest statement of what this
buys is: a truncated download, a proxy that mangles bytes, or a mirror
that is out of date will be caught; a compromised release will not.

Signatures are not added. A signature moves the question to "where does
the public key come from", and answering it with "the same repository"
returns to the same trust root. Doing better means a key distributed out
of band and a revocation story — real work, worth doing when there is a
release process to protect, not before there is a release.

**The source build stays the path that proves its own origin.** This is
the substantive difference between the two, and it is not about hashes:

| | what it trusts | what it proves |
|---|---|---|
| source build | the git history, pinned by sha | the binary was built from *this* commit |
| prebuilt | the release publisher, over HTTPS | the bytes match what the publisher listed |

`dowelup` cannot check that a prebuilt binary was built from the sha it is
being installed as. Nothing in the artifact carries that; the sha comes
from resolving the tag, and the asset is trusted to correspond. A
reproducible build would close this, and it is not attempted here.

**Prebuilt is the default, with `--from-source` to override.** The point
of this ADR is removing the Rust-toolchain requirement, and a default that
still demands one removes nothing. What the default changes is the trust
root, so `install` says which path it took, in one line, every time.

The pin is unaffected: `.dowel-version` holds the resolved sha either way,
so a project pinned to a sha gets the same version whether its developers
took the prebuilt or built it themselves.

**`dowel-up` gains a dependency on `dowel-support` for SHA-256.** It had
none, which kept it independent of the thing it installs. The alternative
was a second copy of the hash, and a hash implemented twice is a hash that
can disagree with itself. The crate is standard-library-only and small,
and dowelup already builds inside this workspace.

## Consequences

- A machine without a Rust toolchain can install dowel. That was the
  purpose.
- The failure mode when an asset is missing is a fallback, not an error:
  a triple with no published binary builds from source and says so. This
  keeps a new platform working before its asset exists.
- Releases now have a shape that has to be produced. A workflow builds
  the assets and their checksums on tag push; the naming above is what
  `dowelup` looks for, so the two are one decision written in two places
  — the e2e test constructs the same layout, which is what keeps them
  from drifting apart silently.
- Nothing verifies the binary's provenance. If that matters for a given
  deployment, `--from-source` is the answer, and it is the answer for the
  same reason it was the only option before this ADR.
