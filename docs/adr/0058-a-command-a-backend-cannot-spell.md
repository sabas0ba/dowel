# ADR-0058: Reject line terminators that a backend cannot represent

**Status**: Accepted

## Context

[ADR-0018](0018-backend-layer.md) says every backend receives the same build
graph, so which one runs is not supposed to change what gets built. One
declaration broke that:

```toml
[bin.app.generate]
g = { command = "sh", args = ["-c", "printf '#define A 1\n#define B 2\n' > cfg.h"],
      outputs = ["cfg.h"] }
```

| Backend | Result |
|---|---|
| `direct` | `1 2` — the header holds both macros |
| `ninja` | the build **succeeded** and `cfg.h` held `#define A 1 #define B 2`: one macro, on one line |
| `make` | `Makefile:13: *** missing separator.  Stop.` |

The ninja backend's escape for a variable value was
`s.replace('$', "$$").replace('\n', " ")`. A ninja variable value is a single
line, so the newline had to go somewhere; turning it into a space produced a
*different command* that ran successfully. The make backend wrote the newline
through, so the recipe spilled onto a second line and make reported a
syntax error in a file dowel generated — a line number in the build
directory, with nothing pointing back at the manifest.

`direct` was right in both cases: it passes `argv` to `exec` and never
assembles a shell line at all.

The make backend already rejects paths that its output format cannot represent ([14-build-graph.md](../14-build-graph.md)). The same validation is needed for line terminators in commands and other generated fields.

## Decision

**Reject a build when a backend cannot represent its command without changing its meaning.** The diagnostic identifies the unsupported character and the affected field.

Check `\n` and `\r` in every field written on a single line. This includes commands, ninja's `depfile` and `default` fields, and make's step descriptions. Make's existing whitespace check covers its paths. Both backends validate all fields before writing, preserving the previous build file if validation fails.

**The message names the fix, because most of the time this is a typo.** The
declaration above does not want a newline at all: it wants `printf` to
receive the two characters `\` and `n` and produce the newline itself. The
manifest's string escape turned `\n` into the character before `printf` ever
saw it. Spelled `\\n`, the same declaration builds under all three backends
and writes the same file:

```
ninja cannot spell a newline inside a build edge, and `GEN generated/app/app/g`
contains one. if the program is meant to receive the two characters `\n`,
write `\\n` — the manifest turns `\n` into the character itself.
`--backend=direct` runs the command without a shell
```

`--backend=direct` rather than `--backend=ninja`: unlike make's path limits,
this one is not a limit only make has. Both shell-line backends have it and
only the one that execs `argv` does not. That distinction has to be kept in
make's *existing* path check too: it classified a newline as whitespace and
sent the reader to ninja, which now refuses the same path — advice that ends
in a second refusal is worse than none. A path that make cannot name for its
own reasons still points at ninja.

Remove newline-to-space replacement from ninja's `value`; it only escapes `$`. If an unchecked newline reaches the output, ninja will report a syntax error instead of executing a command whose meaning was changed.

## Consequences

- A project that needs a newline inside a command builds under `direct` and
  not under ninja or make. Measured against the cases that actually arise,
  this is narrow: every tool that takes a `\n` escape — `printf`, `sed`,
  `awk`, `echo -e` — is spelled `\\n` and works everywhere, and that spelling
  is what the diagnostic points at. Writing the script to a file and running
  `sh script.sh` avoids it entirely, which is what the repository's own tests
  do.
- If a case does turn up that genuinely needs a newline in an argument, the
  shape of the answer is to stop building a shell line for it: write the
  `argv` beside the build file and have the step launch it directly, the way
  `direct` already does. That is machinery, and it is not written for a case
  no one has hit — but it is the direction, not a wider escape.
- `build-graph.json` carries such a command without trouble — JSON strings
  hold newlines, and `arguments` is already an array a reader must not
  re-split. A reader that joins the array into a shell line inherits this
  problem; [14-build-graph.md](../14-build-graph.md) already tells it not to.
- The check is per line terminator, not per "control character". A tab or an
  escape inside a command is spellable by both backends and is left alone;
  refusing more than is broken would reject working builds. The same cut
  applies to fields a backend does not always write: ninja emits
  `depfile = <path>` only for a compile step under the depfile style
  ([ADR-0027](0027-toolchain-style.md)), so the check consults the depfile
  only then — and both sides read one predicate, because a check and an
  emission that each carry their own copy of the condition drift into
  refusing what is never written, or writing what was never checked.
- Nothing checks descriptions or paths for the *other* things a backend
  cannot name — make's existing path check is still make's own, and ninja
  still escapes spaces and `:` in paths rather than refusing them. This
  decision is about the case where a backend was quietly succeeding at
  building the wrong thing.
