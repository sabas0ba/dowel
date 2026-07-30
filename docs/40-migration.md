# Migration from existing build systems

> This is a design document. `dowel migrate verify` is implemented
> ([60-cli.md](60-cli.md)); `import` is not
> ([91-implementation-status.md](91-implementation-status.md)).

The decision is [ADR-0005](adr/0005-migration.md). In short: no static
translation; dynamic extraction only.

## 1. Why static translation cannot work

Migration tools in the Node ecosystem work because the source of truth is
**declarative data** — `package.json` and lock files. CMake's source of truth
is a program, and the actual configuration exists only after running it in a
particular environment.

Attempts to read `CMakeLists.txt` syntactically and translate it break down
without exception on `if(WIN32)`, `find_package`, and user-defined macros;
the cost of fixing the output exceeds a rewrite.

## 2. Dynamic extraction paths

The hooks for extracting execution results as structured data already exist.

| Source | Extraction point | What you get |
|---|---|---|
| CMake | File API (codemodel v2, JSON) | targets, sources, includes, defines, links, per configuration |
| Meson | `meson introspect` | the same, in tidier form |
| Bazel | `aquery --output=proto` | the action graph itself |
| autotools | none (`compile_commands.json` only) | per-translation-unit flag lists; target structure is lost |

The CMake File API is the first-class interface IDEs actually use; reading
from it is the only sensible path.

## 3. The essential limitation

What can be extracted is **one projection of a program**: a snapshot under a
specific OS, configuration, and dependency-resolution result. Conditionals
are lost. The output is a draft, not a finished artifact.

Moreover, this system deliberately rejects things existing systems allowed
(ABI mismatches become failures, and so on). A faithful migration can
therefore produce a manifest that is legitimately rejected, and how that is
handled needs to be settled as UX.

## 4. Put the weight on `verify`

A crude migration result can do harm. The worst path: a manifest is generated
with every flag flattened and intent lost, gets committed as-is, and becomes
a maintenance burden.

```
dowel migrate import   # generate a draft from the File API (marked unverified)
dowel migrate verify   # compare the existing system's compile_commands.json
                       # against our action set, and report the differences
```

`verify` is cheap to implement — the action graph already exists — and high
value. Migration becomes not a one-shot conversion but **a continuous
equivalence check during incremental porting**.

Being able to confirm mechanically that "this target is ported and generates
compile arguments identical to the original environment" removes much of the
psychological barrier to migrating.

## 5. The unit of migration

Because dependency supply is delegated externally
([ADR-0001](adr/0001-toolchain-vs-supply.md)), unported parts remain on the
existing system and are consumed as external dependencies.

The unit of migration is therefore the **target**, not the whole project —
which is what makes it incremental.

## 6. Priorities

| Item | Cost | Value |
|---|---|---|
| `verify` (compile_commands comparison) | low | high |
| CMake File API import | medium | high |
| Meson introspect import | low | medium (Meson users have weak motivation to migrate) |
| Bazel aquery import | high | low (motivation exists but the scale assumptions differ) |
| Static translation | high | negative |

Start with `verify`; limit `import` to CMake.
