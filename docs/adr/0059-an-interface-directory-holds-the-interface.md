# ADR-0059: Warn about source files in installed include directories

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

**Warn when an installed include directory contains compilable source files. Copy the directory without filtering it.**

`public.includes` makes the entire directory available on the consumer's include search path. Filtering files by extension could remove an intentional dependency such as `#include "impl.c"`. File names alone cannot determine which files consumers need, so installation preserves the declared directory's contents.

Use the existing source-extension predicate to identify files for the warning ([ADR-0051](0051-source-language-is-closed.md)). Compilation and this check share the predicate, so adding support for an extension updates both.

The warning identifies the declaration and the source files that will be installed:

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

- The installed files and their contents are unchanged. This decision adds a warning about a potentially unintended include-directory declaration.
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
