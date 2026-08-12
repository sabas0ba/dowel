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

Which further dimensions belong in it is settled the same way, one key per
ADR. `target.env` joined for the ABI label's `libc` component
([ADR-0042](adr/0042-abi-label-components.md)); the rest — standard library,
CRT kind, `_GLIBCXX_USE_CXX11_ABI`, sanitizers, LTO, exception model — wait
for their own evidence.

### The vocabulary

The live version is available from `dowel schema dump`.

| Namespace | Implemented keys | Domain |
|---|---|---|
| `cfg` | `opt` | `debug` / `release` |
| `cfg` | `target` | target triple (free-form string) |
| `host` | `os` / `arch` | build host values |
| `target` | `os` / `arch` / `env` | derived from the target triple; finite ([ADR-0026](adr/0026-target-os-arch.md), [ADR-0042](adr/0042-abi-label-components.md)) |
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

## Q4. Store format details

**Status**: decided.

- Record structure and index layout — implemented: fixed-length records
  (key hash, fingerprint, offset, length, durability) over an append-only
  value log ([20-architecture.md](20-architecture.md) section 5)
- What goes into the fingerprint — the value's bytes; a matching
  fingerprint means the file is neither lexed, parsed, nor evaluated
- GC policy — [ADR-0037](adr/0037-store-gc.md): growth is reported by
  default and collected on request, with the budget following the graph
  (over budget = dead bytes exceed live ones). `DOWEL_CACHE` picks
  notify / gc / off, and `gc --older-than=<days>` collects build
  directories by age
- Migration across version changes — a format change moves `FORMAT`, the
  new version starts empty in its own directory, and `gc` removes the old
  one. Nothing converts an older format: misreading an old layout is worse
  than recomputing

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

**Status**: decided by [ADR-0036](adr/0036-prebuilt-distribution.md).

Release assets on the upstream repository, named
`dowel-<tag>-<triple>.tar.gz`, verified against a `.sha256` published
beside them. Release specifiers take a published binary by default;
everything else builds from source, and `--from-source` forces it.

The checksum catches corruption, not tampering, and nothing checks that a
published binary was built from the sha it is installed as. That is the
substantive difference between the two paths, and it is why the source
build stays available rather than being replaced.
