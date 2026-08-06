# Test-suite design

[50-development.md](50-development.md) covers the environment and
conventions. This document covers what is checked and how.

## Approach

Each layer is assigned exactly one subject, and no subject is checked by more
than one layer. Overlapping checks multiply the edits a change requires
without widening what is detected.

| Layer | Question | Location | Stage name |
|---|---|---|---|
| unit | does each part meet its spec | `mod tests` in `crates/*/src/**` | `unit-*` |
| robustness | tolerance of broken input; invariants held | `crates/dowel-syntax/tests/robustness.rs` | `syntax-robustness` |
| integration | is the cross-layer assembly correct | `crates/dowel-model/tests/` | `model-*` |
| e2e | do the generated graphs execute correctly | `crates/dowel-cli/tests/e2e.rs` | `e2e` |
| scenario | behavior across sequences of operations over time | `crates/dowel-cli/tests/scenario.rs` | `scenario` |
| fixture | building and running real-shaped projects | `tests/projects/` | `fixture` |
| diagnostics & coverage | do diagnostics reach the user; is every feature checked | `crates/dowel-cli/tests/diagnostics.rs` | `diagnostics` |
| example | do the documented examples work | `crates/dowel-cli/tests/example.rs` | `example` |
| acquisition | can `dowelup` resolve, fetch, and switch versions | `crates/dowel-up/tests/dowelup.rs` | `up` |
| docs | do links resolve; are the indexes complete | `crates/dowel-cli/tests/docs.rs` | `docs` |
| measurement | is the budget met | `scripts/measure-startup.py` | `startup` |

One entry point (`make verify`); local runs and CI execute the same thing.

## The three layers added later

### Fixtures (`tests/projects/`)

The synthesized two-package project suits checking semantics in isolation,
but lacks the dependency shapes real projects have:

- Three or more dependency levels, with public and private mixed
- Diamonds (two paths reaching the same library)
- The convention that every package puts public headers in `include/`

The third point is what surfaced a defect on this layer's first run: merging
deduplicated by relative path alone, so once dependencies exceeded two
levels, another package's include directory was dropped. With a two-package
synthetic project the path depth is one, so it never showed.

The configured fixture (`configured`) also caught one on its first run:
writing `match` as a list element makes the specialized result a list inside
a list. Merging only flattened one level, so the value survived and was
silently skipped downstream. Both `check` and `dowel why` passed; only the
compile arguments were missing it. Under a single configuration there is
little reason to write `match`, and even then only one branch runs — the
shape only appears when configurations are switched.

Fixtures check themselves. Checks against the build system's semantics are
written, wherever possible, as `#error` and exit statuses in the fixtures' C
code — the same form users actually write, with no expected values duplicated
into the harness. Only what C cannot observe (the contents of
`compile_commands.json`, checks that something does *not* propagate, which
targets rebuilt) lives on the harness side.

Conventions are in [`tests/projects/README.md`](../tests/projects/README.md).

### Scenarios (`scenario.rs`)

A build system's main feature is the second run onward. e2e checks a single
run, and lining up single runs cannot exercise the edit-and-rerun path.

This layer's first run caught the direct backend omitting the command line
from its freshness check: after a flag change, the rebuild did not run,
because neither the inputs nor their mtimes had changed. Artifacts produced
under the old flags were being reported as success.

Observation is via the verdict reasons of
`--backend=direct --log-level=debug`. Watching artifact mtimes would also
work, but it depends on clock resolution and leaves no record of *why* a
re-run happened.

### Docs (`docs.rs`)

Documentation inconsistencies break neither the build nor the tests, so they
go undetected unless checked.

The subjects are limited to what can be judged mechanically; the prose itself
is not validated.

- Relative link targets exist
- Documents named from code and scripts exist. When a document number
  changes, these references break before the Markdown links do; they carry no
  markup, so they are scanned separately
- The index in `docs/README.md` matches the contents of `docs/`
- The table in `docs/adr/README.md` matches the ADRs on disk (both
  directions)
- The crate table in `docs/91-implementation-status.md` matches `crates/`
  (both directions)

The last item was added after the language-server crate was left out of the
table. The status document doubles as the index of what exists; a layer that
is not listed reads as absent. A name appearing somewhere in prose is not
enough — the check requires a table row.

### Diagnostics and coverage (`diagnostics.rs`)

Unit tests check that a diagnostic is generated. But reaching the user takes
a path through evaluation, validation, rendering, and output — and a
diagnostic dropped along the way still passes unit tests.

This layer's first run found two unreachable diagnostics: `invalid-source`
and `unresolved-path`. Writing a directory in `sources` produced the linker's
`input file unused`; naming a nonexistent file produced ninja's
`no known rule`. Neither points at the causing manifest line.

Coverage tracking lives in the same file. Diagnostic codes are the target
because they are the one user-visible interface with stable identifiers that
can be enumerated mechanically. A code found by scanning the sources but
missing from the case table fails the check. Items for which no case can be
written go in `UNCOVERED` with a reason.

What this guarantees is a lower bound on coverage; whether each case is a
sensible input is not checked — that judgment lives in each case's `why`
text.

Code coverage does not look at diagnostic content. Merge conflicts
(`merge-conflict` / `abi-mismatch`) report the pair of values arriving from
two packages, but the human rendering only showed the file of the primary
label. The machine-readable form has both locations, so structural unit tests
passed. The case table now carries a two-package input and checks that both
file names appear in the rendering.

Likewise, the case table only attests that a fix suggestion exists. Span
errors only appear on application, so a companion check applies the
suggestion and runs `check` again. The verdict is twofold: the original
diagnostic disappears, and no new diagnostic appears. Because of the latter,
a case with a suggestion must be an input that passes once the suggestion is
applied.

Location presence is tracked separately. `missing-manifest` and
`missing-build` carried no labels and did not point at the causing
declaration; with multi-level dependencies, the path in the message alone
does not say *which* `dowel.toml` declared it. Diagnostics that cannot carry
a location go in `WITHOUT_LOCATION` with a reason.

## Where a new test belongs

| What you want to check | Where it goes |
|---|---|
| behavior of a function or data structure | `mod tests` of the crate |
| tolerance of broken input | `robustness.rs` |
| assembly across layers | `crates/dowel-model/tests/` |
| arguments reaching the compiler; artifacts that actually run | `e2e.rs` |
| rerun after editing, configuration switches, cross-process change detection | `scenario.rs` |
| properties that only appear in real dependency shapes | a new fixture in `tests/projects/` |
| a new diagnostic | the case table in `diagnostics.rs` (omission fails the coverage check) |
| a new document | the index in `docs/README.md` (omission fails the docs check) |
| a new crate | the crate table in `docs/91-implementation-status.md` (ditto) |

## Conventions

- Test names describe what is checked: not `test_build` but
  `editing_one_source_recompiles_only_that_object`. On failure, the name
  alone should identify the subject
- Incrementality is checked by counting. Value correctness is not enough —
  what was *not* recomputed is observable only through execution counts. A
  "does not recompute" check is paired with its "does recompute"
  counterpart; the former alone also passes when nothing was queried at all
- Temporary files are created under `target/`, never outside the repository
  (why `/tmp` is avoided: [50-development.md](50-development.md) section 5)
- The real files in `examples/` and `tests/projects/` are never modified;
  they are copied under `target/` before building. Leftover artifacts in the
  repository are caught by `every_fixture_is_left_clean_in_the_repository`
- Definitions in code are written in English; explanations may be in Japanese
  ([50-development.md](50-development.md) section 5)

## Future

| Item | Standing |
|---|---|
| a scale fixture (many sources, tracking planning time) | together with the startup-budget measurements; also measures the break-even scale of [ADR-0012](adr/0012-store-contents.md) |
| toolchain variation (pass under both gcc and clang) | together with CI matrixing |
| golden outputs (full-text comparison of `build.ninja`) | determinism is already checked; full-text comparison is deferred, being sensitive to toolchain differences |
