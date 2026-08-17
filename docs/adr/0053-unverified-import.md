# ADR-0053: An imported target says so in the manifest, and only a person clears it

**Status**: Accepted

Closes Q6 of [99-open-questions.md](../99-open-questions.md).

## Context

`dowel migrate import` writes a draft from one configuration of an existing
build, and marks it UNVERIFIED in a header comment. Q6 asked what should
happen when such a draft fails this system's verification, and listed three
options: downgrade errors to warnings during a migration window, fail and
require fixes, or mark the output unverified and enable verification
incrementally. The third was favoured, leaving open whether a
machine-readable mark should gate verification per target, and what clears
it.

Measuring an actual import answers the first half differently than the
question assumed. A Meson draft whose target linked a sibling archive:

```console
$ dowel check
check passed: 1 packages, 2 targets

$ dowel build
/usr/bin/ld: .../src_main.c.o: in function `main':
src/main.c:3: undefined reference to `len_of'
```

**The draft does not fail verification. It passes.** Meson reports link
inputs mixed into the compile arguments, so the importer drops them and
writes them as comments (issue #135) — there is nothing for `check` to
object to, because nothing false was declared. What is missing is a `deps`
edge that no one claimed existed. The failure arrives at the link, in the
linker's words, about a symbol, with nothing connecting it back to the
comment the importer left three lines above the target.

So there is nothing to downgrade. A mode that turned errors into warnings
would suppress checks the draft never trips, and would suppress them for
real declarations too — an `abi` label a person adds during the migration is
exactly the sort of claim that should still be checked.

## Decision

**`unverified = true` is a root-block property**, written by
`migrate import` on every target it drafts. It is a statement of provenance:
this target was extracted from another build system, and nothing has
confirmed it builds what that build built.

**It does not gate any check.** Nothing is downgraded, suppressed, or
deferred. The mark buys visibility, not leniency — a draft that declares
something wrong is wrong whether or not it came from an importer.

**While it is there, every plan reports it** as `unverified-import`, a
warning naming the target and what is known to be unestablished: everything
came in private, and link inputs the old build passed are not `deps` yet.
Warnings do not fail a build, so the draft still builds and runs; what
changes is that the incompleteness is stated before the link rather than
discovered inside it.

**`dowel migrate verify` counts what is still marked**, per target, beside
the source-level verdict. The unit of migration is the target
([40-migration.md](../40-migration.md) section 5), so the unit of progress
is too.

**Only a person removes the line.** `migrate verify` compares *compile
arguments*; it does not run the link, and it cannot know that the dropped
inputs were reconstructed as `deps`, that the private/public split matches
the intent, or that a conditional lost in the snapshot mattered. Clearing
the mark is the claim "I checked this", and dowel is not in a position to
make that claim on the user's behalf. What it can do — and now does — is
refuse to let the question be forgotten.

## Consequences

- The mark is machine-readable, so `--message-format=json` carries it, an
  editor underlines it, and `migrate verify --format=json` reports the
  remaining targets as a list. "How much of this project is ported" is a
  number rather than a memory.
- A tree mid-migration prints one warning per unported target on every
  `check`. That is the intent, and it is self-limiting: the noise is
  proportional to what is left, and reaches zero when the port does.
- Nothing stops a person from writing `unverified = true` by hand, and
  nothing should. A target hand-written from a reading of the old build is
  in the same position as a drafted one.
- `migrate verify` reporting "equivalent" while targets remain marked is
  not a contradiction. It is the honest state: the compile lines match, and
  the link has not been examined by anything but a build.
- There is still no way for dowel to notice that a dropped link input was
  never reinstated. The comment naming it and the mark on the target are
  both inert text; a check that read the comments would be reading a
  language dowel does not define.
