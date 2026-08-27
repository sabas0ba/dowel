# ADR-0059: A directory shipped as an interface is reported when it holds sources, not filtered

**Status**: Accepted

## Context

`public.includes` names "the directories a consumer compiles against", and
`dowel install` ships their contents under `include/`
([ADR-0041](0041-install.md)). Where headers live in a directory of their
own — the layout this repository's own fixtures use — that works exactly as
written. Where they sit beside the sources, it does this:

```console
$ dowel install --prefix=out
installed: out/include/core.c
installed: out/include/main.c
installed: out/lib/libcore.a
installed: out/lib/pkgconfig/core.pc
```

`core.c` is the library's own source. `main.c` is a *binary's* source, in the
same directory by accident of layout and no part of the library's surface.
Both are now under the `include/` that `pkg-config --cflags` points at.

The declaration is doing two jobs at once. As a **search path** it is
correct: a consumer compiling against `src/` finds `core.h`. As a statement
of **what to ship** it is far too wide, and nothing said so.

## Decision

**dowel reports it and ships the directory unchanged.**

Filtering by extension is the obvious move and it is wrong. `public.includes`
puts the whole directory on the consumer's `-I` path; shipping a subset
breaks a single-file library that does `#include "impl.c"`, which is a real
if uncommon shape. Deciding which files are the interface from their names is
guessing, and the install path already refuses to guess — it copies the
search path *because* the search path was declared.

Recognising a source is not guessing, though. That question is closed
([ADR-0051](0051-source-language-is-closed.md)), and the same predicate that
decides what dowel compiles decides what to name here — one answer, in one
place, so that a spelling added later is added once.

So the warning names the declaration, not the files' fate:

```
warning[source-among-headers]: `src` holds 2 files that dowel compiles, and install ships them as the interface
 --> dowel.build:5:13
  |
5 | includes = [dir("src")]
  |             ^^^^^^^^^^ a consumer compiles against this directory
   = note: they land under `include/`: core.c, main.c
   = note: the whole directory is shipped, unfiltered: a header-only library may
           `#include` a `.c`, and dowel does not guess which files are the interface
   = note: put the headers in a directory of their own if that is not what you meant
```

One diagnostic per declaration, not per file — a deep tree would otherwise
say the same thing once per source (the judgement issue #158 already made).

**It points at the declaration.** `public_include_dirs` now carries the site
each directory was written at. A message naming only the path cannot say
*which* `public.includes` produced it once a package has several, and the
fix is an edit to that line.

## Consequences

- The install still produces the same bytes it did before. This decision
  adds a sentence, not a behaviour change, because the behaviour was
  declared and the declaration was the thing that was wrong.
- A project that deliberately ships a `.c` to be `#include`d gets a warning
  it does not need. It is a warning, the install succeeds, and the note says
  what dowel could not tell apart. The alternative — staying silent — leaves
  every accidental case silent too, and those are the common ones.
- `uninstallable-headers`, the neighbouring warning for a `public.includes`
  entry that is not a directory, now points at its declaration as well. It
  had the same gap for the same reason.
- Nothing checks the *other* direction: a header outside every
  `public.includes` directory is not shipped, and dowel does not notice that
  a consumer will fail to find it. That is a question about what the
  interface omits rather than what it over-includes, and it needs its own
  evidence.
