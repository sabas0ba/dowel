# ADR-0038: A shared library's exported surface is a boundary toward its consumers; inside its own package it links statically

**Status**: Accepted

Extends [ADR-0030](0030-shared-libraries.md), which stands: the exported
surface is still declared, still required, still generated per object
format. What this decides is *who the surface is a boundary against*.

## Context

Declaring `linkage = "shared"` stopped the library's own tests from
linking ([issue #134](https://github.com/sabas0ba/dowel/issues/134)):

```
undefined reference to `core_step'
```

This is correct behavior for a shared library — `core_step` is not in
`exports`, so it is not in the dynamic symbol table — and it leaves no way
out.

**Tests that reach inside are the ordinary shape of a library's own
tests.** Exercising only the public surface cannot cover the table
construction or the state machine behind it, which is why every system has
a way to link a library's tests against its implementation: CMake's
`OBJECT` libraries, Meson's `objects:`, Cargo's in-crate `#[cfg(test)]`.

The three workarounds available were each bad in a different way. Adding
internal names to `exports` breaks the surface — writing the thing you
meant to hide into the declaration that hides it. Listing the library's
sources in the test duplicates the source list, so the test silently
measures a stale implementation when someone adds a file to one and not
the other. Giving up on testing the shared configuration abandons the one
that ships.

## Decision

**Within its own package, a shared library is linked statically.**
`target("core")` from a sibling target links the archive; `dep("core")`
from another package links the shared library.

The reasoning is what a surface *is*. `exports` says what the library
offers to code that was not written alongside it — that is what a
distribution boundary means, and the package is dowel's unit of
distribution. A sibling target is not a consumer of the artifact; it is
part of the thing being built. dowel already draws this line: `private`
means "this target only", and it means it because a package's targets have
one author.

So a shared library now produces both files: the shared object it ships,
and an archive its own package links. The objects are compiled once,
position-independently, and used by both — the archive costs one `ar`
invocation and no extra compilation.

**This holds for every kind, not only `test`.** A `bin` in the same
package linking its own library statically is the same statement: what
gets distributed is the package, and a tool inside it is not a consumer of
the surface. Special-casing `test` would say the boundary depends on what
you are building rather than on who you are, and the next report would be
about `bench`.

**Nothing is declared to get this.** A tree written against a static
library keeps working when `linkage = "shared"` is added — which is the
property the issue asked for, and the reason a `link = "objects"` opt-in
was not chosen. An explicit spelling would be one more thing to know, in
service of a distinction the package boundary already draws.

## Consequences

- A binary in the same package carries the code rather than sharing it,
  so the shared library's size benefit does not apply within the package.
  That is the accepted cost: a package is distributed together, and the
  alternative is that its own tests cannot see it.
- The surface is still verified where it matters. A consumer in another
  package links the shared object and gets exactly `exports`; the e2e for
  that is unchanged.
- The archive is built whenever the shared library is, and both are
  outputs. `cache gc --older-than` and the size reporting
  ([ADR-0037](0037-store-gc.md)) count it like anything else.
- `dowel why` and the graph are unaffected: this changes which file a link
  action receives, not which targets depend on which.
- Not addressed: a package that genuinely wants its own binary to link the
  shared object — to test the deployed shape end to end, say. That is a
  real thing to want, and it wants a spelling of its own rather than
  inverting this default.
- [12-build-reference.md](../12-build-reference.md) gains the sentence
  that was missing: the shared-library section explained how the surface is
  decided but never said the library's own tests fall outside it. The
  report says it was not discoverable until stepped on.
