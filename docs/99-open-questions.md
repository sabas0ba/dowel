# Open questions

In priority order. The higher an item, the more it constrains later design.

## Q1. The `cfg` namespace vocabulary

**Status**: not started. The next item to pin down.

The shared foundation referenced by `when` predicates in `dowel.toml`,
`match` / `when` in `dowel.build`, toolchain selection, and ABI labels.

Working draft:

```
cfg.opt        configuration (debug / release / …)
cfg.target     target triple
host.os        build host OS
host.arch      build host architecture
feature.<name> feature flag
tc.c           the selected C toolchain
```

To decide:

- Which dimensions belong in `cfg` (these become candidate components of the
  ABI label)
- Predicate composition rules (allow anything beyond implicit AND?)
- Whether the vocabulary is fixed or extensible by toolchains

### The provisional vocabulary used by the implementation

To make progress, the implementation adopts the draft above verbatim as a
**closed vocabulary** (`crates/dowel-eval/src/config.rs`). This is a
placeholder until Q1 is decided, not the decision itself. The live version is
available from `dowel schema dump`.

| Namespace | Implemented keys | Domain |
|---|---|---|
| `cfg` | `opt` | `debug` / `release` |
| `cfg` | `target` | target triple (free-form string) |
| `host` | `os` / `arch` | build host values |
| `feature` | `<name>` | boolean (only names declared in `[features]` of `dowel.toml`) |
| `tc` | `c` | identifier of the selected C toolchain |

Predicate composition is implicit AND only. Exhaustiveness checking of `match`
applies to keys with finite domains (`cfg.opt` / `host.os` / `host.arch`);
`cfg.target` has an unbounded domain and requires a `_` arm. This asymmetry
will be revisited when Q1 is decided.

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

The third is favored, but the granularity of the mark and the conditions for
clearing it are undecided.

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
