# Roadmap

## Approach

Phases are cut so that **each is valuable on its own and permits the decision
not to proceed**. Phase 0 in particular exists to make a "don't build it"
decision possible early.

## Phase 0: measurement and premise validation (no implementation)

All of these can be done without writing a build system, and they ground the
design decisions that follow.

| Item | Method | What it informs |
|---|---|---|
| Breakdown of cold configure | `strace -c` on real projects; process launches, stat counts, wall time | expected payoff of the probe-fact DB |
| Reconfiguration latency | time taken when one manifest file changes | expected payoff of incremental evaluation |
| Frequency of generator expressions / string manipulation | static counting | expected payoff of types and diagnostics |
| Whether ABI mismatches occur in the wild | bolt an ABI label check onto existing projects; count detections | **whether investing in an execution layer is justified** |

The last item is the important one. If the detection count is not
significant, the premise of centering ABI verification collapses — in which
case the focus narrows to incremental evaluation and diagnostics (which
stands even as a layer over existing systems).

## Phase 1: the core

The phase that pins down the constraints that cannot be retrofitted.

- A parser with a lossless CST (error-tolerant)
- The incremental query engine (early cutoff, cancellation, durability
  layers)
- The persistent store (mmap index + append-only log, `flock`, atomic swap)
- The type system and merge semantics
- Provenance tracking and `dowel why`
- `dowel check` (runs through planning without executing; scope per
  [ADR-0010](adr/0010-check-scope.md))

**Deliverable**: `dowel check` and `dowel why`, verifiable side by side with
an existing project.

## Phase 2: generation

- Action graph construction
- ninja file generation
- `compile_commands.json` output
- The probe-fact DB
- `dowel build` / `dowel test`

**Deliverable**: actually able to build — though dependency supply is
pkg-config delegation only.

## Phase 3: migration and interoperation

- `dowel migrate verify` (compile_commands comparison)
- `dowel migrate import` (CMake File API)
- Importing dependencies from vcpkg / Conan
- Emitting CMake `find_package` config files (the reverse direction)

**Deliverable**: incremental adoption in existing projects becomes possible.

## Phase 4: developer experience

- The runner abstraction (qemu / SSH / real hardware)
- `dowel debug` (auto-consistent substitute-path, DAP config generation)
- The language server (diagnostics and hover only)
- JSON diagnostics

Runners and debugger integration have the largest felt impact per investment
and can be validated independently of the rest; this phase may run in
parallel with Phase 3.

## Phase 5: dependency management

- `dowel.lock` generation and verification
- Cooldown, license allowlists, approval flow for new transitive dependencies
- Toolchain acquisition and hash pinning
- Vendoring and offline builds

## Phase 6: ABI / FFI

Started only if the Phase 0 validation comes back positive.

- ABI label computation and `must_equal` verification
- ABI boundary declaration (IDL)
- `dowel abi check` (diff against the previous version)
- Generated symbol visibility
- Export targets: C ABI / CPython extensions / N-API / JVM Panama

## The execution layer (unplanned)

Isolated execution with a CAS action cache would replace the Phase 2 ninja
generation. The persistent store's machinery is reusable as-is, so the door
to introducing it later stays open. Decide based on operational experience
through Phase 5.

## The nature of "done"

| Part | Can it finish? |
|---|---|
| query core + language + types | yes |
| generation / migration / runners | yes |
| language server | **no** (permanent maintenance cost) |
| ABI / FFI export | grows with every target language |

The language server is planned on the premise that it is permanently
unfinished; its initial feature set is kept narrow.
