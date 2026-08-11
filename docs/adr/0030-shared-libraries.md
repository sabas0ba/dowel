# ADR-0030: A shared library declares what it exports; the linker's form of that list is generated

**Status**: Accepted

## Context

`lib` produced a static archive and nothing else. Every import of an
existing project had to record the loss: CMake's `SHARED_LIBRARY` and
Meson's `shared library` both became a `lib` with a comment saying dowel
builds static archives today. Shared libraries are not a niche — they are
how C is distributed on every platform dowel targets, and a build system
that cannot produce one cannot be adopted by the projects that ship them.

Adding the output is easy. Giving it a *portable meaning* is the whole
problem, and the reason this needs a decision rather than a patch.

**The exported surface of a shared library is not the same thing on two
platforms.** On ELF and Mach-O, every non-`static` symbol is exported by
default: the library's interface is "whatever the sources happened to leave
external." On Windows, nothing is exported unless the source says
`__declspec(dllexport)` or a `.def` file names it: the interface is empty
until it is declared. The same sources, the same manifest, and the same
build command therefore produce two artifacts whose interfaces have nothing
to do with each other.

This is not a corner case; it is the ordinary experience of porting a
library to Windows, and it is the same class of problem
[00-overview.md](../00-overview.md) names when it says there is no single
ABI. A build system that adopts each platform's default reproduces the trap
and calls it portability.

The alternatives to adopting the defaults:

- **Export everything, everywhere.** Not implementable: there is no
  "export everything" on Windows short of scanning the objects and
  synthesizing a `.def`, which means reading object files — knowing the
  toolchain from the inside, which [ADR-0001](0001-toolchain-vs-supply.md)
  keeps dowel out of.
- **Require the sources to carry export macros.** This is what projects do
  today, and it works, but the macro must be defined differently per
  platform and per whether the library is being built or consumed. It is
  precisely the boilerplate that gets it wrong, and it puts the interface
  in the sources where no tool can see it.
- **Declare the list in the manifest.** The interface becomes a thing the
  build system knows, and each platform's form of it is generated.

## Decision

**Linkage is a property of `lib`, not a separate kind.**

```toml
[lib.core]
sources = glob("src/*.c")
linkage = "shared"
exports = ["core_open", "core_close", "core_version"]
```

`linkage` is `"static"` (the default) or `"shared"`. A finite domain, so
`match` over it is exhaustiveness-checked like any other
([ADR-0026](0026-target-os-arch.md)), and a library that is shared on one
target and static on another is written the ordinary way.

It is not a new table kind because nothing else about the target changes.
The sources, the dependencies, and the interface it publishes to dependents
are identical; a `shared` kind would duplicate the entire property surface
so that dependents could then be made not to care about the difference.
Dependents already do not care: `target("core")` names the library, and how
it is linked is the library's business.

**A shared library must declare `exports`.** Not defaulting to the
platform's behavior is the point of this ADR: a declaration that means
something different on each platform is not a declaration. Omitting it is
`missing-exports`, an error at planning time.

The cost is real and is accepted: the author writes the list. What is
bought is that the list *is* the interface, it is the same interface
everywhere, and it is visible to anything that reads the manifest — which
is what makes the later work possible at all
([00-overview.md](../00-overview.md) section 6: diff checking against a
previous version, and generation for other languages, both need to know the
surface).

**dowel generates the linker's form of the list, never a source-level
macro.** From one `exports` list:

The form follows the **object format**, not the argument style: mingw
spells its arguments the GNU way but produces PE, where a version script
means nothing.

| Object format | Generated file | Passed as | Link spelling |
|---|---|---|---|
| ELF | version script | `-Wl,--version-script=` | `-shared`, `-Wl,-soname,` |
| Mach-O | symbol list, names prefixed `_` | `-Wl,-exported_symbols_list` | `-dynamiclib`, `-Wl,-install_name,@rpath/` |
| PE (mingw) | `.def` | given as an input file | `-shared` |
| PE (MSVC) | `.def` | `/DEF:` | `/DLL` |

**The generated file is the only mechanism; `-fvisibility=hidden` is not
added.** It is the obvious companion and it is wrong: a symbol hidden at
compile time cannot be brought back by a version script's `global:` list,
so the two together produce a library that exports *nothing*. Measured, not
reasoned about — the same source with and without the flag exports one
symbol and zero symbols respectively. The script alone does the whole job:
what is listed is global, and `local: *` closes the rest.

Objects that may end up inside a shared library are compiled `-fPIC`. That
is every object of the shared library **and of every target in its link
closure**: a static library linked into a shared one contributes its
objects to a position-independent output, and non-PIC objects are rejected
there. Linkage is therefore not purely local to the target that declares
it — declaring one library shared changes how its dependencies are
compiled.

The generated file is written where the object files go and is an input of
the link action, so a changed `exports` list relinks. It is generated, not
authored: [ADR-0027](0027-toolchain-style.md) already holds that the
arguments dowel builds itself are dowel's to spell, and an export list is
the same kind of thing one level up.

**Names in the list are not mangled.** `exports` names symbols, and a
symbol is what the linker sees. For C that is the function name; for C++ it
is the mangled name, which the author must write. dowel does not mangle,
because mangling is the ABI it does not implement — the `_` prefix added on
Mach-O is not mangling but the platform's uniform symbol prefix, applied to
whatever was written.

**Dependents find the library at run time through an rpath.** A binary
linking a shared library gets `-Wl,-rpath,<build>/lib` and the library gets
a soname (`-Wl,-soname,libcore.so`, or `-install_name @rpath/libcore.dylib`
on macOS). Without the soname the executable records the path it linked
against and the rpath is dead weight; with it, the recorded name is the
plain one and the rpath resolves it.

The rpath is the absolute build directory. The build tree is already not
relocatable — every path in an action is absolute — so this adds no
constraint that was not there.

On Windows there is no rpath. Executables are found next to the binary or
on `PATH`, and dowel's layout puts binaries and libraries in sibling
directories, so `dowel test` and `dowel run` add the library directory to
`PATH` for the child process. An executable started by hand from a Windows
build tree will not find its DLLs; that is a property of the build tree,
not of the artifact, and installation is where it stops being true.

## Consequences

- `lib` targets with `linkage = "shared"` compile their objects `-fPIC`,
  and so does everything they link. Objects are already per-target, so a
  static library that is consumed both directly and through a shared one
  is compiled once, position-independently, rather than twice. That costs
  a little on x86-64 and nothing on most other architectures; building it
  twice would mean object paths keyed by who consumes them, which is a
  larger change than the saving justifies.
- The import of an existing project can stop apologizing for shared
  libraries, but cannot infer the export list: neither CMake's File API nor
  Meson's introspection reports one. An imported shared library therefore
  arrives as a static `lib` with a note, as before. Turning it into a
  shared library is a human decision, and the list is what the human
  supplies.
- Symbol versioning is not addressed. The version script generated here
  carries no version nodes, and `libcore.so` is not `libcore.so.1`.
  Versioned sonames belong with the ABI diff checking of Phase 6, which is
  what would decide when the version must change; guessing at it now would
  produce a number nothing verifies.
- Installation is not addressed. Everything here concerns the build tree.
- Nothing checks that a name in `exports` actually exists. A misspelled
  name is silently absent from an ELF version script; the `.def` form
  makes MSVC's linker complain. Checking it means reading the objects, and
  that is the line ADR-0001 draws — but the check that matters is
  reachable from the other side, by asking the produced library what it
  exports, and that is left for the ABI work.
