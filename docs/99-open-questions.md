# Open questions

In priority order. The higher an item, the more it constrains later design.

## Q1. The `cfg` namespace vocabulary

**Status**: decided. The target's own words by
[ADR-0026](adr/0026-target-os-arch.md), predicate composition by
[ADR-0032](adr/0032-predicate-composition.md), and extensibility by
[ADR-0034](adr/0034-closed-vocabulary.md).

The shared foundation referenced by `when` predicates in `dowel.toml`,
`match` / `when` in `dowel.build`, toolchain selection, and ABI labels.

**The vocabulary is closed** and grows only by an ADR, one key at a time,
with a domain ([ADR-0034](adr/0034-closed-vocabulary.md)). What holds it
closed is exhaustiveness checking, findable misspellings, and a
configuration identity that does not depend on which toolchain was
selected. A project's own axes go in `[features]`, which is the second
layer: dowel declares what it knows, the package declares the rest.

**Which further dimensions belong in it is Q2's question.** The candidates
— standard library, CRT kind, `_GLIBCXX_USE_CXX11_ABI`, sanitizers, LTO,
exception model — are exactly the candidate components of the ABI label.
Adding them before knowing which ones the label needs, and at what
granularity, would fix the granularity by accident.

### The vocabulary

The live version is available from `dowel schema dump`.

| Namespace | Implemented keys | Domain |
|---|---|---|
| `cfg` | `opt` | `debug` / `release` |
| `cfg` | `target` | target triple (free-form string) |
| `host` | `os` / `arch` | build host values |
| `target` | `os` / `arch` | derived from the target triple; finite ([ADR-0026](adr/0026-target-os-arch.md)) |
| `feature` | `<name>` | boolean (only names declared in `[features]` of `dowel.toml`) |
| `tc` | `c` | identifier of the selected C toolchain |
| `tc` | `cxx` | identifier of the selected C++ toolchain |

Predicates compose with `and` / `or` / `not`
([ADR-0032](adr/0032-predicate-composition.md)). Exhaustiveness checking of
`match` applies to keys with finite domains (`cfg.opt` / `host.*` /
`target.*`); `cfg.target` has an unbounded domain and requires a `_` arm.
That asymmetry is what ADR-0026 addressed for the target: the triple stays
open, and the distinctions that recur have finite words beside it.

One domain question is left open rather than settled: whether `cfg.opt`
should hold more than `debug` / `release` (`relwithdebinfo` and friends).
Extending a domain is a smaller question than extending the vocabulary,
and no one has asked for it.

## Q2. ABI label composition

**Status**: deferred. Depends on the outcome of Q1.

The counterpart of Conan's `package_id`. Granularity dominates the design.

- **Too coarse** → verification becomes meaningless (the vcpkg triplet limit)
- **Too fine** → cache hit rates collapse

Candidate components: toolchain ID, C++ standard version, standard library
implementation, `_GLIBCXX_USE_CXX11_ABI`, MSVC runtime kind, sanitizers, LTO,
exception model, floating-point model.

The Phase 0 verification (how many real mismatches are detected) will indicate
the required granularity.

## Q4. Store format details

**Status**: skeleton only ([20-architecture.md](20-architecture.md) section 5).

- Record structure and index layout
- What goes into the fingerprint (what is hashed)
- GC policy (generations / size cap / reachability)
- Migration across version changes (when a format change discards the store)

## Q6. What to do when `import` output is rejected

Configurations extracted from an existing project may fail this system's
verification (ABI mismatch, `error_on_conflict`, and so on).

Options:

- A mode that downgrades to warnings (limited to a migration window)
- Fail and require fixes
- Mark extracted output "unverified" and enable verification incrementally

The third is favored. The current implementation carries the mark as an
UNVERIFIED header comment on the generated files (human-facing, pointing at
`migrate verify`); whether a machine-readable mark should gate verification
per target — and what clears it — remains undecided.

## Q7. C++20 modules

The plan is to make scan actions first-class in the graph, but parts of this
depend on the state of clangd support. Re-survey when Phase 2 starts.

## Q8. Verifying the current state of Meson

The statements about Meson in [00-overview.md](00-overview.md) are based on
prior knowledge. The wrap and lock behavior in particular may have changed in
recent releases; confirmation against the official documentation has not been
done.

## Q10. Prebuilt distribution for dowelup

**Status**: not started. [ADR-0013](adr/0013-self-acquisition.md) defined
source builds only.

Building from source assumes a Rust toolchain. Widening the audience requires
distributing prebuilt binaries. To decide:

- Where to publish (GitHub Releases or a separate endpoint)
- How to verify (SHA-256 comparison; whether signatures are required)
- How binaries map back to the sha source of truth (recording and checking
  which commit a binary was built from)

Fetching itself can be delegated to `curl`, but verification has to be owned
here.
