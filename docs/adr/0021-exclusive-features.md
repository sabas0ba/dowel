# ADR-0021: Features stay additive; exclusivity is declared, never inferred

**Status**: Accepted

## Context

Feature flags are additive: `--features=x11` adds to `default`, it does not
replace it. This is the Cargo convention and it is right — a dependency
enabling a feature must never be able to turn one off for someone else.

But the tool dowel offers for choosing between implementations is a
conditional element of `sources`, and dowel actively recommends splitting
implementations into separate files rather than `#ifdef`-ing one. Written
the obvious way, the two collide:

```toml
sources = [
    file("src/main.c"),
    file("src/shell_x11.c")      when feature.x11,
    file("src/shell_headless.c") when feature.headless,
]
```

Two `when`s are not a choice. `--features=x11` leaves `headless` on, both
translation units are compiled, and what happens next depends on the target
kind (issue #82):

- **`bin`** — the linker reports `multiple definition`. It fails, which is
  right, but the message is the linker's. dowel says nothing about the two
  features being on
- **`lib`** — it **succeeds**. Both objects go into the archive and the
  linker pulls whichever member first satisfies the symbol. The build is
  green, the tests pass (only one implementation is exercised), and the
  artifact is not what was asked for

The second is the serious one. Which implementation ended up in the artifact
is decided by archive member order — something the manifest cannot see and
cannot influence. That is precisely an unrecorded input
([00-overview.md](../00-overview.md) section 2).

`match feature.x11 { true => …, false => … }` is the correct spelling and
always selects one. Nothing said so, and nothing objected to the other.

## Decision

Two changes, neither of which touches additivity.

**A package may declare which of its features are mutually exclusive**, as a
reserved key of `[features]` alongside `default`:

```toml
[features]
default   = ["headless"]
headless  = []
x11       = []
exclusive = [["headless", "x11"]]
```

Each inner list is a set that must not be simultaneously active. When two or
more of a group are active for that package, loading fails with
`conflicting-features`, naming them and saying where each came from — the
`default` case is the one that gets forgotten, so it is called out by name
along with `--no-default-features`.

The names in a group must be declared in the same `[features]` table
(`unknown-feature`), and a group of fewer than two names forbids nothing and
warns. Exclusivity is per package, like the features themselves
([ADR-0017](0017-feature-forwarding.md)).

**The documentation now recommends `match` for choosing an implementation**,
and says plainly that stacking `when`s does not make them exclusive
([12-build-reference.md](../12-build-reference.md)).

Exclusivity is never inferred. dowel does not know that two files define the
same symbol, and guessing from file names or from "these two are only ever
enabled separately in the tests" would be a rule nobody could predict.

## Consequences

- The failing case fails **at load**, before compiling, with a diagnostic
  that names the features rather than the symbols. The `lib` case fails at
  all, which it did not before
- Declaring it is opt-in. A manifest that says nothing behaves exactly as it
  did, so this breaks nothing and also protects nothing until written. That
  is the price of not inferring
- `exclusive` becomes a name that cannot be a feature, joining `default`.
  The `[features]` table now has two reserved keys, which is a cost paid to
  keep the declaration next to what it constrains
- It states a constraint, not a resolution: dowel refuses rather than
  picking one. Picking would put the choice back where the reporter found
  it — outside the manifest
- A group is checked against the *resolved* set, so a conflict reached
  through `dep/feature` forwarding or through a feature that enables another
  is caught the same way as one written on the command line
