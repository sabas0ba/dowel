# ADR-0048: Assembly is a third language, not C that happens to assemble

**Status**: Accepted

## Context

dowel picks a language per source file by extension: a C++ extension gets the
C++ driver, **everything else gets the C driver**
([12-build-reference.md](../12-build-reference.md) section 3).

Hand-written assembly is common in the projects dowel is for — crypto
primitives, SIMD kernels, boot code, context switches. Passing `foo.s` to
`cc` does assemble it, so it looked like it already worked. Measuring what
actually came out found three things:

- **`-std=c17` and `-Wall` were passed to the assembler.** A target that
  declares a C standard was handing it to a file that is not C. gcc tolerates
  it, which is why nobody noticed.
- **The objects had no `.note.GNU-stack`**, so the linker warned that an
  executable stack was implied — and the warning says it will become an
  error. The C compiler marks its own output; nothing marks hand-written
  assembly.
- **`-MD -MF` was passed to `.s` files and no `.d` was written.** dowel
  declared a depfile the compiler never produces. It happened not to break
  because nothing checked.

None of this is exotic; it is what a build system is supposed to get right on
behalf of the person writing the assembly.

## Decision

**Assembly is a third language.** `.s` and `.S` select it, the same way the
C++ extensions select C++.

**It is still built by the C driver.** The driver runs the assembler, so
declaring a separate tool would be a name for something dowel already has,
and every toolchain declaration would have to grow a key for it. The progress
line says `AS` rather than `CC`, because reading `CC` next to a `.s` file is
what made the C flags look reasonable.

**`c_flags` and `c_std` do not reach it; `asm_flags` does.** Assembly gets
`flags` — the language-independent ones — plus its own. That is the same
shape `c_flags` / `cxx_flags` already had, and the reason is the same: a
language-specific flag belongs to its language.

**dowel adds `-Wa,--noexecstack`.** This is an argument dowel assembles
itself, in the same category as `-fPIC` for a shared library
([ADR-0030](0030-shared-libraries.md)): the correctness of the output depends
on it, and no one else is going to supply it. The rare case that genuinely
needs an executable stack can say so through `asm_flags`, which comes later
on the command line.

**A depfile is requested only where one can be written.** `.S` goes through
the preprocessor and has header dependencies; `.s` does not. Asking for a
depfile that is never produced means declaring an output that does not
appear, which is the shape of bug that makes an incremental build never
converge (issue #112 was the same shape).

## Consequences

- The ninja backend needed a fix to express "no depfile": its rule says
  `depfile = $depfile`, and an edge that does not bind the variable makes
  ninja resolve it to itself and refuse the file as a cycle. An empty binding
  is how ninja spells "none". The direct and make backends already handled an
  absent depfile, so this was the only place — and it is the reason the
  regression test covers all three.
- Editing a header included by a `.S` now rebuilds it. Before, the
  declaration existed but the file did not, so the dependency was not there
  to follow.
- `.asm` is deliberately not an assembly extension. That spelling is MASM and
  NASM syntax, which the C driver does not accept; recognizing it would
  produce a confusing failure from a tool that was never going to work.
- Nothing chooses a different assembler. `[toolchain] c` is what assembles,
  and a project needing `nasm` has no way to say so. That is a real limit and
  a separate decision — it needs a tool in the table, an extension mapping,
  and its own argument spellings.
- The language is still decided by extension only. A `.c` file that is
  actually assembly, or the `-x assembler` escape hatch, is not expressible.
