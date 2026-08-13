# ADR-0049: A `lib` may name a library that already exists, so what another toolchain built becomes a first-class dependency

**Status**: Accepted

## Context

A C or C++ program frequently has to link something dowel did not build: a
Rust `staticlib`, a Zig `build-lib`, a Go `c-archive`, or a vendor's binary
SDK. dowel produces the other direction already — `dowel install` plus the
generated `.pc` ([ADR-0041](0041-install.md),
[ADR-0043](0043-pkgconfig-generation.md)) let any of those consume a dowel
library. Coming back the other way, the only spelling was:

```toml
[bin.app.private]
link_flags = ["-L", dir("target/release"), "-lengine"]
```

Written in every consumer, once each. It is not a dependency, so it does not
propagate an include path, does not carry an ABI label, does not appear in
`dowel why`, and does not participate in the checks the rest of the system
is built out of. A file that is central to the build is described by two
flags that dowel passes through without understanding.

The thing being described *is* a library target. It has a surface, it has
requirements, consumers depend on it. The only thing it lacks is sources.

## Decision

**A `lib` may declare `prebuilt` instead of `sources`.**

```toml
[lib.engine]
prebuilt = file("target/release/libengine.a")

[lib.engine.public]
includes   = [dir("include")]
abi        = { libc = "gnu" }
link_flags = ["-lpthread", "-ldl"]
```

It is an ordinary target from there on: `deps = [target("engine")]` links it,
its `public` block propagates, `dowel why` traces it, and `abi` is compared
the way every other label is. Nothing in the merge algebra or the graph
learns a special case; a prebuilt target simply produces no compile and no
archive action.

**dowel does not run the build that produces it.** Running cargo, zig, or go
would make dowel a general build system, which
[ADR-0001](0001-toolchain-vs-supply.md) says it is not. The file has to be
there, and if it is not, that is `missing-prebuilt` naming the path it looked
for and saying who was supposed to produce it. This is CMake's `IMPORTED`
target, and it is imported for the same reason.

**`sources` and `prebuilt` together is `prebuilt-with-sources`.** A target is
built here or it was built elsewhere; with both, which file is the artifact
has no answer. Only a `lib` can be prebuilt (`prebuilt-not-a-library`) —
what is being named is something to link against, not a program to run.

**Its existence is checked when the plan is made**, like a tool's. Left to
the link, a missing file comes back in the linker's words one stage later
(issue #50 made the same argument for compilers).

## Consequences

- The ABI label finally has an edge worth checking. A Rust `staticlib` built
  against musl, declared `abi = { libc = "musl" }` and linked into a gnu
  build, is refused before the link
  ([ADR-0042](0042-abi-label-components.md)) — the check was designed for
  exactly the case where one side is not built here, and until now there was
  no way to have one.
- The extra system libraries these toolchains need (`-lpthread`, `-ldl`,
  `-lm` for a Go archive) go in the target's `public.link_flags`, which
  already propagates. Nothing new was needed.
- Freshness is by file identity, like any other input: replace the archive
  and the link re-runs. dowel does not know *why* it changed, and cannot warn
  that the other build system was not re-run — it can only report what it
  sees.
- `dowel install` treats it as a library of this package and installs it. A
  package that ships a vendor blob ships it; a package whose prebuilt is a
  build artifact of a sibling project probably does not want that, and has no
  way to say so.
- A shared library can be named too, but `exports` and `soversion` do not
  apply — those describe how dowel *builds* one. What a prebuilt library
  exports is whatever it exports, and ADR-0039's check does not run on it.
- Nothing verifies that the file is a library at all, or that it is for this
  target triple. The digest-and-pin machinery of
  [ADR-0044](0044-toolchain-acquisition.md) is not applied here: a prebuilt
  is usually produced inside the same working tree by a sibling build, not
  fetched, so there is nothing stable to pin to yet.
