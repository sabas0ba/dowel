# ADR-0044: A toolchain is fetched and pinned the way a dependency is, and it lives in the user's cache

**Status**: Accepted

Delivers the first half of Phase 5's "toolchain acquisition and hash
pinning" ([90-roadmap.md](../90-roadmap.md)). Rests on
[ADR-0031](0031-toolchain-is-the-builds.md): the toolchain belongs to the
build, not to a target.

## Context

`[toolchain.<triple>]` names commands, and those commands have to already be
on the machine:

```toml
[toolchain.aarch64-unknown-linux-gnu]
c  = "aarch64-linux-gnu-gcc"
ar = "aarch64-linux-gnu-ar"
```

The manifest is pinned, the source is pinned, dependencies are pinned by
`rev` or `sha256` — and then the compiler is whatever the machine happens to
have under that name. A cross build is therefore reproducible everywhere
except in the one input that decides the object code. Two developers
following the same README get different binaries and nothing in the tree
says so.

dowel already knows how to solve this. A `url` dependency is fetched and
verified against a declared `sha256`
([ADR-0029](0029-tarball-dependencies.md)); `dowelup` fetches a release asset
and checks it against a published digest
([ADR-0036](0036-prebuilt-distribution.md)). A toolchain is an archive with
a digest. Nothing about it needs a new mechanism.

## Decision

**A toolchain declaration may name a `url` and a `sha256`, and the tools are
found inside what is unpacked.**

```toml
[toolchain.aarch64-unknown-linux-gnu]
url    = "https://example.org/gcc-13-aarch64.tar.xz"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
c      = "bin/aarch64-linux-gnu-gcc"
ar     = "bin/aarch64-linux-gnu-ar"
```

**The toolchain's root is the base for its tools' relative paths.** A
command with a path separator is resolved against the unpacked root; an
absolute path and a bare name (`cc`) are left alone. Those two already mean
something — a place on this machine, and a PATH lookup — and neither is a
statement about the archive.

**`url` requires `sha256`**, for ADR-0029's reason: a URL is a name, and the
bytes behind a name can change. Missing or malformed is
`unpinned-toolchain`. A fetch that fails, or an archive whose digest does not
match, is `unfetchable-toolchain`, and the build stops rather than falling
back to whatever is on PATH — a silent fallback would put the machine's
compiler behind a declaration that says otherwise, which is the problem this
decision exists to remove.

**It is unpacked into the user's cache**, not the tree:
`$XDG_CACHE_HOME/dowel/toolchains/<sha12>/`. The reasoning is
[ADR-0028](0028-probe-facts.md)'s, only stronger. The same archive is the
same bytes in every tree, so keeping it per-tree re-downloads the most stable
thing in the system into the most volatile place — and a toolchain is an
order of magnitude larger than a dependency. Fetching happens once; later
runs find the completion marker and touch no network.

**Verification comes before unpacking**, and the completion marker is written
before the directory is moved into place, so an interrupted fetch leaves
nothing that looks finished. This is ADR-0029's procedure unchanged; only the
destination differs.

## Consequences

- A cross-compiling tree can now pin every input it depends on. That is what
  makes `dowel.lock`, the pinned `rev`, and the pinned archive worth
  anything — the compiler was the hole in the middle of them.
- `toolchain-mismatch` compares the **resolved** command now. It used to
  compare the declaration against what the build uses, which for a fetched
  toolchain always differs, so every such package would warn about itself.
  The note about fetching being Phase 5 went with it.
- The language server never fetches. It resolves against a toolchain that is
  already unpacked and otherwise leaves the command as written, so a tree
  whose toolchain has not been fetched yet can show `missing-toolchain` in
  the editor until the first build. That is the same boundary
  [ADR-0002](0002-no-daemon.md) draws for every other network operation.
- The archive's shape is not declared. One top-level directory is stripped
  when there is exactly one, matching ADR-0029, so vendors' usual
  `gcc-13-aarch64/bin/...` layout works and a flat archive works too.
- Nothing collects old toolchains. `cache gc`
  ([ADR-0037](0037-store-gc.md)) reaches the store and the build directories,
  both inside the tree; the toolchain cache is outside it and shared, so
  removing one is a decision about other trees as well.
- The `toolchain` **kind** in `dowel.build` is still reserved and still
  unimplemented. This decision does not need it: the declaration belongs to
  the build, and `dowel.toml` is where the build is described.
- What is fetched is not verified to be a toolchain. The digest says the
  bytes are the ones declared; whether `bin/…-gcc` exists is found out when
  it is probed, with `missing-toolchain` naming the path that is missing.
