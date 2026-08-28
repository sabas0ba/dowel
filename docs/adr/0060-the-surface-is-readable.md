# ADR-0060: What was installed is asked whether it can be read

**Status**: Accepted

## Context

[ADR-0059](0059-an-interface-directory-holds-the-interface.md) closed one
direction — an interface directory that ships more than the interface — and
named the other as unfinished: a header the interface *reaches* but does not
ship. Measured, that direction is worse:

```console
$ dowel install --prefix=out
installed: out/include/core.h
installed: out/lib/libcore.a
installed: out/lib/pkgconfig/core.pc
```

No warning. The install succeeded. Then the consumer:

```console
$ cc m.c -Iout/include -Lout/lib -lcore
out/include/core.h:1:10: fatal error: core_types.h: No such file or directory
    1 | #include "core_types.h"
```

`core.h` sits in `public.includes`; the `core_types.h` it reads sits in
`private.includes`. **Inside the build tree it compiles**, because both
directories are on the search path. It breaks only where it was shipped, and
only for the person who received it — the side that shipped it saw a list of
`installed:` lines and nothing else.

That is the shape [ADR-0051](0051-source-language-is-closed.md) already
refused once: printing success for something that is not there. The same
answer applies, one stage later.

## Decision

**After installing, dowel asks the installed headers whether they can be
read from what was installed.** Each is preprocessed with the target's C
compiler, given the installed include directory and nothing else — the
search path a consumer actually has. A header that does not preprocess there
will not preprocess for the consumer either.

**dowel does not count `#include` lines itself.** A text scan would have to
answer conditional inclusion and names built by macros, which is reading C —
the work [ADR-0001](0001-toolchain-vs-supply.md) leaves to the toolchain.
This is the posture [ADR-0039](0039-exports-are-checked.md) took for
`exports`: ask the artifact, and let the tool's own words carry the
complaint. The first line the compiler says is quoted in the diagnostic,
because naming what was missing is its answer, not dowel's.

```
warning[unreadable-surface]: `core.h` cannot be read from what was installed
 --> dowel.build:5:13
  |
5 | includes = [dir("include")]
  |             ^^^^^^^^^^^^^^ this is what a consumer compiles against
   = note: out/include/core.h:1:10: fatal error: core_types.h: No such file or directory
   = note: preprocessed with `cc` against `out/include` alone, the way a consumer does
   = note: a header the surface reaches has to be installed too, or moved out of it
```

It points at the `public.includes` declaration, for ADR-0059's reason: the
path alone cannot say which declaration shipped it, and that line is the edit.

**Read it the way a consumer reads it, not merely from the same directory.**
Three things decide that, and each one left out turns the check into a
different question than the one it claims to answer:

- **The words on the consumer's compile line.** `public.defines` and
  `public.flags` reach a consumer through pkg-config's `Cflags`
  ([ADR-0043](0043-pkgconfig-generation.md)) — the install code says in as
  many words that what a dowel consumer receives and what a pkg-config
  consumer receives must not differ. A define that opens an `#include`
  breaks the consumer and passes a check that does not carry it.
- **The language.** `.hh`, `.hpp` and `.hxx` are C++; `.h` is both, and
  which way its `__cplusplus` branch falls is decided by the target that
  shipped it, so `.h` follows whether that target compiles C++. The C++
  driver reads the C++ ones, since the C driver does not carry the C++
  standard library's search path.
- **Saying the language out loud.** The driver is not asked to infer it from
  the spelling. Measured: `cc -E t.HH` warns, exits 0, and never opens the
  file — the exact "warning, exit 0, read nothing" shape ADR-0051 exists to
  refuse, which this check had reproduced inside itself. With
  `-x c-header` / `-x c++-header` (`/TC` / `/TP` for MSVC) the spelling stops
  deciding anything.

**Only preprocessing, and only a closed list of spellings.** The claim is
narrow on purpose: *the headers shipped can be found from what was shipped*.
Type errors and missing declarations are a different question and would need
a full parse. The spellings read as headers are `.h`, `.hh`, `.hpp`, `.hxx` —
closed for ADR-0051's reason: a README or a licence under `include/` is not
a header, and handing one to a compiler proves nothing.

**Failing to run the tool is not a failure.** As with the export check, the
absence of a check must not turn an otherwise successful install into an
error.

## Consequences

- The check runs once per installed header, after the copy. It is an install
  cost, not a build cost, and installs are rare; a library with hundreds of
  public headers pays hundreds of preprocessor runs, which is the same order
  as compiling it once.
- A header that is genuinely not meant to stand alone — one that documents
  "include `core.h` first" — is warned about. It is a warning, the install
  succeeds, and the note says exactly what could not be found. Self-contained
  headers are the widely-held convention, and staying silent would keep every
  accidental case silent too.
- The check uses the *target's* compiler, so a cross install is read the way
  its consumer would read it.
- A C library's headers are read as C, and a C++ consumer of the same
  library takes the other `__cplusplus` branch, which this does not check.
  Reading each header twice would cover it and doubles the cost; the branch
  a library's own language does not take is the weaker claim of the two.
- Nothing checks that a consumer can **link**. `exports` covers the symbols a
  shared library promises (ADR-0039); a static archive's surface is still
  taken on trust.
- `install::entries` now answers with a struct rather than a pair. What is
  shipped and what declared it have to survive to the point where the check
  runs, and that is after the files exist.
