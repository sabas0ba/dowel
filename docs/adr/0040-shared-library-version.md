# ADR-0040: A shared library's ABI generation is declared, and it names the file

**Status**: Accepted

Narrows what [ADR-0030](0030-shared-libraries.md) deferred. ADR-0030 stands
in full; this decides only who supplies the number.

## Context

ADR-0030 ended with:

> Symbol versioning is not addressed. The version script generated here
> carries no version nodes, and `libcore.so` is not `libcore.so.1`.
> Versioned sonames belong with the ABI diff checking of Phase 6, which is
> what would decide when the version must change; guessing at it now would
> produce a number nothing verifies.

That reasoning is about **dowel** guessing. It is right, and nothing here
changes it: dowel still cannot tell that an interface broke, and still will
not bump a number on its own. Phase 6 keeps that job.

What the reasoning does not cover is the author saying it. A soname is not
a derived fact; it is a promise about compatibility, made by the person who
knows what the library guarantees. dowel does not verify `[package]
version` either, and no one expects it to.

Leaving the promise unspeakable has a cost that shows up only later. A
consumer records the soname at link time, so a binary built today against
`libcore.so` carries `libcore.so` forever. When an incompatible `libcore.so`
appears in the same place, that binary loads it and misbehaves — the exact
failure sonames exist to prevent. The name cannot be corrected after the
fact, because the wrong name is already inside every consumer.

## Decision

**A shared library may declare `soversion`, an integer, and it becomes part
of the library's name.**

```
[lib.core]
sources   = glob("src/*.c")
linkage   = "shared"
soversion = 2
exports   = ["core_open", "core_close"]
```

The file becomes `libcore.so.2`, and because the soname is taken from the
output's file name, consumers record `libcore.so.2`.

**The number is the ABI generation, not the release.** It changes when the
interface stops being compatible, which is rarer than a release and is a
different fact. This is why `[package] version` is not used: `1.2.3` and
`1.2.4` are two releases of one interface, and deriving the soname from the
package version would relink every consumer for a patch release. One number
also means there is no `libcore.so.1.2.3` beneath the soname; the three-name
layout distributions use encodes the release as well, and that belongs to
packaging, not to the declaration.

**Where the number goes is the format's convention, not a choice.** ELF
appends it (`libcore.so.2`), Mach-O puts it before the extension
(`libcore.2.dylib`), PE joins it to the stem (`libcore-2.dll`). Readers
identify a library by looking at a directory, so following each platform's
habit is the whole point.

**The unversioned name is placed beside the versioned file as a symlink.**
`-lcore` resolves through it, which matters more here than elsewhere: a
shared library also produces an archive in the same directory
([ADR-0038](0038-shared-inside-its-package.md)), so without the symlink
`-lcore` silently finds `libcore.a` and links statically. Measured, not
assumed. The symlink is created when the plan is made rather than by a build
action — it depends on no content, a dangling symlink is legal until the
library appears, and dowel passes through that code on every run, so a
deleted symlink comes back.

**Declaring nothing keeps the plain name.** dowel does not invent a
generation for a library whose author did not state one. A default of `1`
would be a promise nobody made, and every existing tree would change the
name of its artifact on upgrade.

**A negative number is `invalid-soversion`**, reported where it is written.
Zero is allowed: it is the ordinary spelling for an interface that is not
yet stable.

## Consequences

- Within the build tree nothing observable changes for a tree that declares
  no `soversion`, and a tree that declares one links by absolute path
  anyway. The value of the declaration is realized when the library leaves
  the build tree, which is the next thing to build.
- Changing `soversion` changes the output's path, so the previous file stays
  in the build directory until it is collected. That is how the build
  directory already behaves when a target is renamed, and `cache gc`
  ([ADR-0037](0037-store-gc.md)) reaches it.
- Symbol versioning inside the library — version nodes in the script, so one
  file can carry two generations of a symbol — is still not addressed. It is
  the harder half of ADR-0030's paragraph, it is ELF-only, and it needs the
  ABI diffing to say anything true.
- macOS's `-compatibility_version` and `-current_version` are not set. They
  are a second, independently checked number, and setting them from the same
  declaration would state a compatibility range that nothing here decides.
  The install name carries the generation, which is what makes two
  generations coexist.
- Windows gets the versioned file name and nothing else, because PE has no
  soname: an import library records the DLL's file name, so the name is the
  whole mechanism there. This part is spelled but not measured — there is no
  Windows in the verification.
