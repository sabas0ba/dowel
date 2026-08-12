# ADR-0043: An installed library describes itself in pkg-config, because dowel already reads that notation and could not write it

**Status**: Accepted

Completes [ADR-0041](0041-install.md), which put files in a prefix and left
them undiscoverable.

## Context

`dowel install` copies a library and its headers into a prefix. Nothing then
tells anyone where they are. A consumer has to be given `-I<prefix>/include
-L<prefix>/lib -lcore` by hand, and has to know that it needs `-pthread` and
`-DCORE_SHARED=1` besides — facts the library already declared in its
`public` block and that the installed tree does not carry.

The consequence is the opposite of what [90-roadmap.md](../90-roadmap.md)
Phase 3 exists for. A library can move to dowel only if every consumer moves
at the same time, because a consumer that stays on CMake, Meson, or a
Makefile has no way to find it. Incremental adoption is the whole premise,
and this was the step that broke it.

There is a sharper version of the same point. dowel **reads** pkg-config: a
`version` dependency is resolved by running `pkg-config --cflags --libs`
([ADR-0015](0015-version-deps-pkgconfig.md)), and the resolutions are
recorded in `dowel.lock`. So dowel consumes the notation the whole C world
publishes interfaces in, and publishes nothing in it. The asymmetry is not
neutral: it means dowel can take from that ecosystem and not give back to it.

## Decision

**Installing a library writes `<prefix>/lib/pkgconfig/<name>.pc`.**

```
prefix=/opt/myapp
exec_prefix=${prefix}
libdir=${prefix}/lib
includedir=${prefix}/include

Name: core
Description: a small hashing library
Version: 1.2.3
Cflags: -I${includedir} -DCORE_SHARED=1 -pthread
Libs: -L${libdir} -lcore
```

**Nothing new is declared to get it.** The file is the target's `public`
block in another notation: `includes` becomes `-I${includedir}`, `defines`
and `flags` become the rest of `Cflags`, `link_flags` join `Libs`. What a
dowel consumer receives and what a pkg-config consumer receives are the same
interface, because they are generated from the same declaration. This is the
same reasoning ADR-0041 used for headers — `public` is already the statement
of what a consumer compiles against.

**`prefix` is the real prefix, never the staging directory.** `--destdir`
moves where the file is written and not what it says, which is what makes a
staged package work after it is unpacked somewhere else.

**`Requires` names only what is certainly there.** System dependencies go in
by their pkg-config module name and minimum version — dowel already knows
both, since that is how it resolved them. A dowel package dependency goes in
only when this same run installed it, so the named `.pc` exists. A
`Requires` line pointing at a missing file makes `pkg-config` fail outright,
which is worse than a line that is not there.

**`Description` needs somewhere to come from**, since pkg-config requires it
and a file without it does not validate. `[package]` gains an optional
`description`; when it is absent the package name stands in, so a tree that
declares nothing still produces a valid file.

## Consequences

- A consumer outside dowel builds against an installed dowel library with no
  dowel involved: `cc main.c $(pkg-config --cflags --libs core)`. That is the
  property Phase 3 needs and it is now verified end to end.
- `-lcore` resolves through the unversioned symlink that
  [ADR-0040](0040-shared-library-version.md) places beside a versioned
  library. The two decisions hold each other up: without the alias this
  `Libs` line would find the archive instead.
- Only `lib` targets get a file. A `bin` is not something to compile against.
- CMake package config files (`<name>Config.cmake`) are still not generated.
  CMake reads pkg-config through `FindPkgConfig`, so this covers the common
  case; a native config file additionally carries imported-target semantics
  and belongs with whatever decides how dowel describes targets to CMake.
- The generated file is not installed as a build artifact and is not in the
  graph. It is assembled at install time from declarations already in hand,
  like the export list that ADR-0030 generates at plan time.
- Nothing verifies the file against a consumer. The e2e does — it runs
  `pkg-config --validate` and then compiles a program with the flags it
  prints — but there is no check on an arbitrary tree that what is published
  is what a consumer needs.
