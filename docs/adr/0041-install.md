# ADR-0041: `dowel install` copies the build tree's products; artifacts are linked to find their libraries relative to themselves

**Status**: Accepted

Closes the last thing [ADR-0030](0030-shared-libraries.md) left open:
"Installation is not addressed. Everything here concerns the build tree."

## Context

Everything decided so far lives inside one build tree. A shared library is
declared in order to be shipped ([ADR-0038](0038-shared-inside-its-package.md)),
its surface is declared and checked ([ADR-0039](0039-exports-are-checked.md)),
and it can now carry an ABI generation
([ADR-0040](0040-shared-library-version.md)) — but there was no way to put
any of it anywhere. The declarations were complete and the destination was
missing.

The obstacle is the run-time search path. A binary that links a shared
library records `-Wl,-rpath,<build>/lib`, an absolute path into the build
tree. Copy that binary elsewhere and it still points at the build tree — and
it keeps working as long as the build tree exists, so the breakage is
discovered by whoever received it, not by whoever built it. Any install that
merely copies would ship exactly that.

Three ways out were considered. Relinking at install time duplicates the
link logic and rebuilds what was already tested. Rewriting the recorded path
afterward needs `patchelf` or `install_name_tool` and puts dowel back into
reading object formats, which is the line [ADR-0001](0001-toolchain-vs-supply.md)
draws. The third is to record a path that is already correct in both places.

## Decision

**Every artifact that links a shared library also records a search path
relative to itself.** `$ORIGIN/../lib` for an executable, `$ORIGIN/.` for a
shared library; `@loader_path/...` on Mach-O, which does not understand
`$ORIGIN`; nothing on Windows, which has no rpath at all.

dowel's layout puts executables in `bin/` and libraries in `lib/`, and the
prefix layout is the same, so one relative path is right in both. The
absolute build-tree path stays alongside it — this change is additive, and
the build tree's behavior is untouched.

This survives every backend, which had to be measured rather than assumed:
`$` is meaningful to ninja, to make, and to the shell that runs a make
recipe. A missed quote leaves the executable linking, running inside the
build tree, and failing only after it moves. All three backends are checked.

**`dowel install --prefix=<dir>` copies; it does not rebuild.** What was
tested and what is shipped are the same bytes. The command builds first, the
way `dowel test` does, and then copies.

**`--prefix` is required.** `/usr/local` is the Unix convention and needs
root; a writable default would be a directory nobody wants. One flag to
learn is cheaper than a default that is wrong for both the packager and the
person trying it out. `--destdir` prepends a staging root to every
destination, which is what a packager needs; because the recorded search
path is relative, staged and final trees behave identically.

**What is installed is a package's `bin` and `lib` targets.** `test` and
`bench` are instruments for checking the thing, not the thing. Naming
targets explicitly overrides the default, as with `build`.

**A library brings the headers it publishes.** The contents of each
directory in its own `public.includes` are copied under `include/`. This is
not an inference: `public.includes` is the declaration that says "a consumer
compiles against this directory", so everything reachable through it is
already the surface. Only the target's own `public` block is read — the
merged compile environment also carries what dependencies propagated, and
those belong to the dependency.

**A versioned library brings its unversioned name**, as a symlink, for the
same reason it has one in the build tree (ADR-0040).

## Consequences

- Shared libraries from *other* packages in the link closure are copied too.
  This crosses the boundary ADR-0038 drew — a package is the unit of
  distribution — but the alternative is an install that does not run. dowel
  installs what the installed artifacts need; it is not a packaging system,
  and a distribution packager will use `--destdir` and their own rules
  instead.
- Headers are installed only for libraries in the install set, so a
  dependency's headers do not come along. Installing a binary does not need
  them. A library whose public interface exposes a dependency's types does,
  and that is not addressed.
- The archive built beside a shared library for its own package's use
  (ADR-0038) is not installed. It is an internal artifact of the build, not
  something the package offers.
- Nothing is recorded about what was installed, so there is no uninstall and
  no manifest. `--destdir` into an empty directory gives the file list, which
  is what a packager reads anyway.
- The extra rpath entry is present even in trees that never install. It is
  one string in the executable and one more directory for the loader to
  consider; in the build tree it happens to resolve to the same place as the
  absolute entry.
- pkg-config files, CMake package files, and man pages are not generated. An
  installed dowel library is found by path, not by a discovery protocol.
