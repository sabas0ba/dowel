# ADR-0050: A build may declare its own assembler, and `.asm` is what needs one

**Status**: Accepted

## Context

[ADR-0048](0048-assembly.md) made assembly a third language and left one
thing open, in its own words:

> Nothing chooses a different assembler. `[toolchain] c` is what assembles,
> and a project needing `nasm` has no way to say so. That is a real limit
> and a separate decision — it needs a tool in the table, an extension
> mapping, and its own argument spellings.

The projects that need it are the ones dowel is for. Crypto and codec
libraries ship NASM sources for x86 because that is what the ecosystem
produces — OpenSSL's and BoringSSL's generators emit gas syntax for the
Unix triples and NASM syntax for Windows, and the Windows half needs a
program the C driver has never heard of. MASM is the same story from the
other end.

`.asm` was deliberately not recognized, on the grounds that "recognizing it
would produce a confusing failure from a tool that was never going to
work". That reasoning holds only while the tool is fixed. Once a project can
name its assembler, `.asm` is assembly whose assembler is declared — and
when it is not declared, dowel knows exactly what is missing and can say so.

## Decision

**`asm` joins the tool table, with no default.** Empty means "the C driver
assembles", which is ADR-0048 unchanged and remains the default. This is
the same shape `link` already has, where empty means "the driver links". A
default of `nasm` would be wrong for nearly every tree; the tool exists to
be declared.

```toml
[toolchain.x86_64-pc-windows-msvc]
c   = "clang-cl"
asm = "nasm"
```

**`.asm` is an assembly extension.** The language is decided by extension,
as it was; what changed is that assembly now has more than one assembler.

**A declared assembler assembles every assembly source in that build.**
Not only `.asm` — one build, one assembly syntax. A project shipping both
spellings selects them per triple, where the toolchain declaration lives
anyway: `.S` on the gnu triples, `.asm` on the MSVC one, chosen with
`match target.os`. Routing by extension *within* one build would mean two
assemblers with one `asm_flags` between them, and `-f elf64` has no meaning
to the other one.

**dowel gives it the input, the output, and `asm_flags`. Nothing else.**
The rest of a compile line — `-g -O0`, `flags`, `-I`, `-D` — is spelled for
a C driver, and an assembler is not one. `asm_flags` is `List<Word>`, so
what an assembler does need is written there and can carry paths:
`asm_flags = ["-f", "elf64", "-I", dir("asm")]`. The input and output
spellings follow the style ([ADR-0027](0027-toolchain-style.md)): `-o out
in` under GNU, `/c /Fo<out> in` under MSVC, which is `nasm` and `ml64`
respectively.

**`.asm` with no assembler declared is `missing-assembler`**, naming the
file and the declaration to write. Handing it to the C driver produces
"file format not recognized" from the linker, two stages later, about a
file the driver silently passed along.

**Executable stack is refused at the link instead.** dowel marks its own
assembly output with `-Wa,--noexecstack` (ADR-0048) and cannot ask a tool
whose spelling it does not know. The linker's spelling it does know: when
the link closure contains objects from a declared assembler, dowel passes
`-z noexecstack`. Same claim, made in the last place dowel can still make
it.

## Consequences

- A declared assembler gets no depfile. Asking for one means declaring an
  output that may never appear, which is what ADR-0048 refused to do for
  `.s`. NASM does write dependencies (`-MD`), but the spelling is that
  assembler's, not a property of the tool slot — a `%include` edit does not
  rebuild today, and closing that needs a per-assembler notion of how to
  ask, which is a further decision.
- `flags`, `includes`, and `defines` do not reach it. A target whose C and
  assembly share an include directory writes it twice, once in `includes`
  and once in `asm_flags`. The alternative — passing `-I` in the style's
  spelling — assumes the declared assembler follows the C driver's
  conventions, which is the assumption this ADR exists to stop making.
- `-Wa,--noexecstack` disappears when an assembler is declared, and with it
  the per-object marking, so a static library assembled by NASM carries
  unmarked objects wherever it goes. `-z noexecstack` covers the links dowel
  performs; it does not travel with an installed archive. A NASM source can
  still mark itself (`section .note.GNU-stack noalloc noexec nowrite
  progbits`), and that is the only thing that survives redistribution.
- Nothing verifies that the declared assembler understands the sources it is
  given. Declaring `nasm` in a tree of `.S` files produces NASM's parse
  errors, which is the tool reporting on the file it was handed — the same
  bargain as any other declared tool.
- `tc.asm` joins the `cfg` vocabulary with the other tools, so a build can
  branch on which assembler was selected. Empty is a meaningful value there:
  it means the driver assembles.
