# ADR-0027: A toolchain declares its argument style; dowel spells what it assembles, and translates nothing else

**Status**: Accepted

## Context

[00-overview.md](../00-overview.md) names MSVC twice as part of the problem
dowel exists to address — "no single compiler" lists gcc / clang / **msvc**,
and "no single ABI" gives **`/MD` vs `/MT`** as an example. The toolchain
table let a project declare the tools' **names**, and nothing else. Naming
`cl` produced a command line no `cl` can read (issue #113):

| dowel emitted | MSVC needs |
|---|---|
| `-g -O0` | `/Zi` or `/Z7`, `/Od` |
| `-MD -MF <path>` | `/showIncludes` — a different mechanism entirely |
| `-c src.c -o out.o` | `/c src.c /Fo:out.obj` |
| objects named `.o` | `.obj` |
| `ar rcs out.a in.o` | `lib /OUT:out.lib in.obj` |
| archives named `libcore.a` | `core.lib` |
| linking as `cl … -o bin/app` | `link … /OUT:bin\app.exe` |

These are not flags a user can override through `flags`: they are the part
**dowel itself assembles**.

One line is worse than mismatched. `-MD` is a valid MSVC flag meaning "link
the dynamic CRT" — a request for a dependency record is read as a choice of
ABI, and it is exactly the flag the overview cites under "no single ABI".

## Decision

The toolchain has a **style**, and dowel spells the arguments it assembles
according to it. Two styles exist: `gnu` (gcc, clang, MinGW) and `msvc`
(cl, clang-cl); everything else follows the former.

- **The style is derived from the target triple**, and `[toolchain] style`
  overrides the derivation. `x86_64-pc-windows-msvc` already says which
  tools are meant, and `--target` already carries it — the same judgment as
  [ADR-0026](0026-target-os-arch.md). The declaration exists for the case
  the derivation cannot see: a driver that takes MSVC spellings under a
  triple that does not say so.
- **The style decides the tools' defaults too.** `ar` defaults to `ar` under
  GNU and `lib` under MSVC; a project that declared nothing gets a coherent
  set rather than half of one.
- **`link` joins the tool table.** Under GNU the compiler driver links, so
  its default is empty and means "the driver does it" — which keeps the C++
  driver selection ([ADR-0007](0007-implementation-language.md)'s reasoning
  about standard libraries) working. Under MSVC `link.exe` is a separate
  program.
- **Only what dowel assembles is spelled per style**: optimisation and debug
  defaults, `-I` / `-D`, the compile input/output pair, the archive
  arguments, the link output, the object extension, the archive name. A
  user's `flags` and `link_flags` pass through **untranslated**. Translating
  them would mean holding a table of flag equivalences, which is to say
  knowing the compiler — the thing dowel does not do.
- **Header dependencies change mechanism, not just spelling.** GNU has the
  compiler write a `.d`; MSVC has it print `/showIncludes` lines and write
  nothing. Whoever runs the compiler folds those lines into the same `.d`,
  so everything that *reads* the record stays style-agnostic.

## Consequences

- A project can declare an MSVC toolchain and get a command line `cl` can
  read. Whether it then *builds* is not something this repository can
  verify — there is no Windows CI — so the checks put a fake `cl` on the
  path and read the assembled command, which is what the report did.
- The dependency record loses its cross-backend property under MSVC. Under
  GNU the `.d` is written by the compiler and every backend reads it
  ([ADR-0018](0018-backend-layer.md), issue #41). Under `/showIncludes` the
  record is written by whoever ran the compiler: ninja folds it into
  `.ninja_deps` (`deps = msvc`), the direct backend writes the `.d` itself.
  Switching backends therefore costs one extra recompile. The alternative —
  routing every MSVC compile through a dowel wrapper so the record lands in
  one place — buys consistency at the cost of a process per translation
  unit and a command line nobody can read.
- `msvc_deps_prefix` is the English `cl`'s wording. A localised compiler
  prints something else; the direct backend then finds no lines and
  **leaves the `.d` unwritten**, which reads as "no record" and rebuilds
  conservatively. Writing an empty record would claim the translation unit
  has no headers, and that is silently wrong.
- Two styles will not cover everything forever (a compiler with its own
  third spelling). The table is small and closed; the cost of a third entry
  is a match arm per assembled argument, not a new concept.
- Cross-compiling to MSVC from Linux stays out of reach for reasons this
  ADR does not touch (the SDK, the CRT). What is settled is that the
  manifest can *express* the toolchain, which is what the report asked for:
  entering it later would have meant changing every place that assembles an
  argument, and there are now nine tools in the table.
