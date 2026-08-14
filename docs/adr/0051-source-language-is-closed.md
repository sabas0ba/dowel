# ADR-0051: A source's language is a closed question, and a tool that writes nothing has failed

**Status**: Accepted

## Context

The language of a source file was decided by extension with a fallback:
C++ extensions got the C++ driver, assembly extensions the assembler, and
**everything else compiled as C**
([ADR-0048](0048-assembly.md)). The fallback was never a decision about
unknown spellings; it was a decision about `.c`, written in a way that also
swallowed everything else.

What that costs came out of measuring the C driver (issue #157):

```console
$ cc -c /tmp/t.asm -o /tmp/t.o
cc: warning: /tmp/t.asm: linker input file unused because linking not done
$ echo $?
0
$ ls /tmp/t.o
ls: cannot access '/tmp/t.o': No such file or directory
```

**A warning, exit 0, and no object.** dowel does not show a successful
command's warnings, so nothing surfaces. The failure lands one stage later,
in the linker's words, about a path inside the build directory:

```
/usr/bin/ld: cannot find .../obj/app/src_note.txt.o: No such file or directory
```

No source name, no line, no diagnostic code. `dowel check` passes. And
because the compile declared an output that never appears, the step is
permanently stale — an unchanged tree recompiles it on every run and never
converges — the shape ADR-0048 refused for depfiles, left in place for
objects.

Two decisions the project already made say what to do here. Issue #50: a
missing compiler is reported when the plan is made, not carried to the link
where it comes back in another tool's words. ADR-0048: never declare an
output that does not appear.

## Decision

**The set of source spellings dowel compiles is closed.** C is `.c` and
`.i`, C++ and assembly are the lists ADR-0048 and
[ADR-0050](0050-separate-assembler.md) already gave. Anything else is
`unknown-source-language`, reported where the source is declared, when the
plan is made:

```
error[unknown-source-language]: `note.txt` is not in a language dowel can compile
 --> dowel.build:2:32
  |
2 | sources = [file("src/main.c"), file("src/note.txt")]
  |                                ^^^^^^^^^^^^^^^^^^^^ declared as a source here
   = note: sources are C (`.c` `.i`), C++ (`.cc` `.cp` ...), or assembly (`.s` `.S` `.asm`)
   = note: the C driver takes an unknown spelling with a warning, writes no object, and exits 0
```

A glob that sweeps up a `README` is reported at the glob. The check is in
`collect_sources`, which is where a file becomes a source and where the
site to point at is still in hand.

**A command that exits 0 without writing what it was asked for has
failed.** The extension check covers what dowel can see in the manifest; it
cannot cover a declared tool that silently does nothing. Two nets, at the
two levels dowel actually observes:

- The direct backend checks a step's outputs after it succeeds, and reports
  the command, its exit status, and its own stderr — which usually contains
  the tool explaining itself.
- After **any** backend, dowel checks that the artifacts it is about to
  print as `built:` exist. This is where ninja and make are covered: neither
  fails on a missing output, so a build under them "succeeded" and then
  recompiled the same file on the next run, forever.

The second net sits beside the export check ([ADR-0039](0039-exports-are-checked.md)),
after the build, for the same reason: what dowel wants to know is not
expressible as a build edge, and running it afterwards works identically
under all three backends.

## Consequences

- A source with no extension is now refused. It compiled as C before, and
  under a fallback that also accepted `note.txt` there was no way to tell
  the two apart. Renaming the file is the fix; there is no `-x c` escape
  hatch, and adding one is a separate decision from *this* one, which is
  about not guessing.
- `.i` is accepted because it is the C the preprocessor produces and the
  driver takes it. `.ii`, `.m`, and `.mm` are not: preprocessed C++ never
  appears in a hand-written `sources` list, and Objective-C is a language
  dowel does not otherwise know — accepting the extension would imply it
  handles the frameworks and runtime that come with it.
- The `built:` line now means the file is there. It did not before: with a
  tool that wrote nothing, dowel printed a path to something that did not
  exist, which is worse than any error message.
- The post-build check costs one `stat` per artifact, not per object. A
  missing *object* under ninja still surfaces as the linker's complaint —
  the plan-time check is what keeps that from happening for the reason this
  ADR is about.
- Nothing checks that the artifact is *correct*, only that it is there. A
  tool that writes an empty file passes.
