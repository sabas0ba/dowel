# ADR-0039: `exports` is checked against the library that was built, by asking it

**Status**: Accepted

Closes the last of the three things [ADR-0030](0030-shared-libraries.md)
left open, and the one with teeth.

## Context

ADR-0030 ended with:

> Nothing checks that a name in `exports` actually exists. A misspelled
> name is silently absent from an ELF version script; the `.def` form
> makes MSVC's linker complain. Checking it means reading the objects, and
> that is the line ADR-0001 draws — but the check that matters is
> reachable from the other side, by asking the produced library what it
> exports, and that is left for the ABI work.

Leaving it there was wrong. A misspelling in `exports` costs nothing at
build time and everything later:

```toml
exports = ["core_open", "core_opne"]   # builds, says nothing
```

The library links, the wrong name is simply absent from the dynamic symbol
table, and the failure surfaces in someone else's build — as an undefined
reference to a function they can see in the header. The declaration that
exists to *be* the interface is the one thing nothing validates.

The linker will not do it. `-Wl,-u` and `--no-undefined` were both tried:
a shared library may legitimately have undefined symbols, so neither turns
a missing export into an error. GNU `ld` has no equivalent of Solaris's
`-z guidance`.

## Decision

**After a build, dowel asks each shared library what it exports and
compares.** A name in `exports` that is not in the answer is
`unexported-symbol`, pointing at the line that declared it.

**Asking is delegated to the toolchain's symbol lister** — `tc.nm`, which
already exists per style (`nm` under GNU, `dumpbin` under MSVC). dowel
parses the output enough to collect names and no further. This is the
"other side" ADR-0030 pointed at: no object file is read, no format is
decoded, and the line [ADR-0001](0001-toolchain-vs-supply.md) draws stays
where it is.

**The check runs after the build, not as a build step.** A step would need
an output file to sit in the graph, and the thing being produced is a
verdict, not a file. Running it after means it works the same whether the
build went through ninja, make, or the direct executor — the backend does
not have to know about it.

**It runs on every build that has a shared library, including one that had
nothing to do.** Listing symbols is a few milliseconds and happens once
per shared library. The startup budget
([20-architecture.md](../20-architecture.md) section 5.4) is about runs
with nothing to build; a tree that declares a shared library is asking for
its interface to be correct, and paying `nm` for that is the cheapest
verification in the system.

**A symbol lister that cannot be started is not a failure.** The tool is
probed where it is used, like every other; if it is absent the check is
skipped with a note rather than failing a build that otherwise succeeded.
The check adds confidence, and its absence should not remove a working
build.

## Consequences

- The mistake this catches is the one that is otherwise found by a
  consumer, in a different repository, as an undefined reference. Moving
  it to the build that declared it is the whole point.
- Only presence is checked. Nothing verifies that an exported symbol has
  the signature its header claims, or that it is `extern "C"` where `abi =
  "c"` says so ([ADR-0019](0019-c-abi-label.md)). Those need the
  declaration to carry more than a name.
- Names are compared as the linker sees them, which is how `exports` is
  already specified: for C the function name, for C++ the mangled name,
  with Mach-O's `_` prefix applied by dowel. The comparison therefore
  strips that prefix back off before matching.
- A library whose exports are all correct pays one `nm` per build. If that
  ever shows up in a measurement, the fix is to remember the verdict
  keyed by the artifact's identity — the same shape as the probe facts
  ([ADR-0028](0028-probe-facts.md)) — not to drop the check.
