# ADR-0060: Check installed headers with the consumer's preprocessing options

**Status**: Accepted

## Context

[ADR-0059](0059-an-interface-directory-holds-the-interface.md) added a warning for source files included in installed header directories. It left a separate problem unresolved: an installed header can depend on another header that was not installed. The following case demonstrates that problem:

```console
$ dowel install --prefix=out
installed: out/include/core.h
installed: out/lib/libcore.a
installed: out/lib/pkgconfig/core.pc
```

Installation succeeded without a warning, but compiling a consumer failed:

```console
$ cc m.c -Iout/include -Lout/lib -lcore
out/include/core.h:1:10: fatal error: core_types.h: No such file or directory
    1 | #include "core_types.h"
```

`core.h` is in `public.includes`, while its dependency `core_types.h` is in `private.includes`. The library builds because both directories are on its include search path. The consumer only has the installed public headers, so it cannot find `core_types.h`.

The install command needs a check that detects this missing dependency before a consumer attempts to use the installed library.

## Decision

**After installation, dowel preprocesses each installed header with the target's compiler.** The explicit include directory is the installed `include/` directory. The check also uses the public compile options and language described below.

The compiler handles conditional inclusion and header names constructed by macros. This avoids implementing C preprocessing in dowel, consistent with [ADR-0001](0001-toolchain-vs-supply.md). As with the `exports` check in [ADR-0039](0039-exports-are-checked.md), dowel includes the tool's diagnostic in its warning. It quotes the first nonempty line of compiler stderr.

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

The warning points to the `public.includes` declaration that selected the header directory, so the provider can identify the declaration to edit.

The check uses three parts of the consumer's preprocessing configuration:

- **Public compile options.** Use the merged `interface(T)`, including macros and flags from public dependencies. These options also reach pkg-config consumers through `Cflags` and `Requires` ([ADR-0043](0043-pkgconfig-generation.md)). Omitting a propagated macro could skip a conditional `#include` that the consumer uses.
- **Header language.** Treat `.hh`, `.hpp`, and `.hxx` as C++. For `.h`, use C++ if the providing target compiles any C++ source, including outputs declared by `generate` ([ADR-0054](0054-generated-sources.md)). Use the C++ driver for C++ headers so its standard library search paths are available. Apply `cxx_std` and `cxx_flags` for C++, or `c_std` and `c_flags` for C.
- **Explicit language selection.** Pass `-x c-header` or `-x c++-header`, or `/TC` or `/TP` for MSVC. In the observed case, `cc -E t.HH` returned exit status 0 without opening the file because it did not recognize the extension as a header. Explicit selection prevents such a skipped check from being treated as successful preprocessing.

**Check preprocessing only, for a fixed list of header extensions.** The check detects preprocessing failures such as missing includes. It does not validate types or declarations. It accepts `.h`, `.hh`, `.hpp`, and `.hxx`, ignoring case, and skips other files such as READMEs and licences.

If the compiler is unavailable or cannot start, skip the check without failing installation. This is the same policy used for the export check.

## Consequences

- The check launches the preprocessor once per installed header after copying. Installation time therefore increases with the number of public headers.
- A header that requires another header to be included first may produce a warning. Installation still succeeds. The check does not distinguish an intentional include-order requirement from an accidental missing dependency.
- The check uses the *target's* compiler, so a cross install is read the way
  its consumer would read it.
- A C library's `.h` headers are checked as C. The check does not cover the `__cplusplus` branch used by a C++ consumer. Checking both languages would require a second preprocessing pass per header.
- The check does not attempt to link a consumer. `exports` checks a shared library's declared public symbols (ADR-0039); there is no equivalent check for a static archive's declared interface.
- Compile options come from the merged interface even if a public dependency is not installed. A pkg-config consumer unable to resolve that dependency through `Requires` may therefore receive different options. This check does not verify that all public dependencies have usable pkg-config metadata.
- `install::entries` returns a struct containing file operations, header metadata, and diagnostics. This preserves the installed paths and declaration locations until the post-copy check runs.
