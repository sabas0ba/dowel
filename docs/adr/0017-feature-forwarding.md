# ADR-0017: Features belong to the package that declares them; `dep/feature` forwards

**Status**: Accepted

## Context

`[features]` was resolved once, from the root package, into one flat set of
names. Every `feature.<name>` reference in every package's `dowel.build` was
answered from that single set.

Two things followed, neither intended.

- **A feature of one package answered for another.** `feature.zlib` in a
  dependency was true whenever *anything* in the build had enabled a feature
  spelled `zlib`. Two packages that happen to name a feature the same way
  were not two features
- **`dep/feature` did nothing.** Writing `deep = ["core/deep"]` — the
  Cargo-shaped spelling for "enable `deep` in `core`" — put the literal
  string `core/deep` in the set. Nothing translated it, so `feature.deep`
  inside `core` stayed false. It appeared to work whenever the parent and
  child feature names coincided, because the parent's own name was already
  in the shared set

The second is the worse of the two: the manifest reads as though the
dependency is being configured, and nothing says otherwise. Validation
already treated features as package-scoped — `feature.<name>` must be
declared in *that package's* `[features]` — so the reference side and the
activation side disagreed.

## Decision

A feature belongs to the package that declares it. Activation is resolved
per package, and a value in `[features]` may name a dependency's feature:

- A plain name (`fast`) enables that feature **in this package**, and closes
  transitively over this package's own `[features]`
- `dep/feat` enables `feat` **in the dependency `dep`**. It does not become
  a feature of the declaring package
- `dep` must be declared in `[[dependencies]]`; otherwise
  `undeclared-dependency`, the same code a `dep("...")` reference gets
- `feat` must be declared in that dependency's `[features]`; otherwise
  `unknown-feature`, reported at the forwarding site

The active set is carried as `<package>/<feature>` pairs, and
`feature.<name>` is answered by qualifying with the package whose manifest
the value was declared in. Specialization therefore happens per package
(`Config::for_package`), which is where the two sides are reconciled.

Because a forwarded feature can activate an `optional` dependency inside the
dependency — which changes what gets loaded — loading and feature resolution
are mutually dependent. The walk is repeated until the requested sets stop
growing. Sets only grow, so this terminates; loading, git fetching, and
pkg-config resolution are memoized, so later rounds have no external
effects.

## Consequences

- Two packages may use the same feature name for unrelated things. This was
  already what the documentation implied and what `feature.<name>`
  validation enforced; only activation had to catch up
- A manifest that relied on the old leakage — a dependency's
  `feature.<name>` being satisfied by the root's identically-named
  feature — changes behavior. That reliance was not expressible on purpose,
  and the fix is to forward explicitly
- Forwarding is one level deep per declaration, but composes: a dependency
  may itself forward onward with its own `dep/feat` entries
- The configuration identifier now carries qualified names. It stays one
  path component — the `/` is folded, as issue #68 required — and it
  distinguishes configurations that the flat set could not
- The build directory name changes for any build that enables a feature —
  `-zlib` becomes `-app--zlib`. It is an opaque identifier, and the change
  costs one rebuild
