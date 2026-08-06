# Overview

> This is a design document covering the project's motivation and
> positioning. For usage, see [62-getting-started.md](62-getting-started.md);
> for the feature reference, see [10-manifest.md](10-manifest.md) and
> [60-cli.md](60-cli.md).

## 1. Motivation

Cargo's usability rests on four premises: a single compiler, a single
language, a single registry, and a single ABI. C/C++ lacks all four.

| Missing premise | How it shows up |
|---|---|
| No single compiler | gcc / clang / msvc, plus version differences within one lineage |
| No single language | C / C++ / asm / Fortran / generated code / cross-language FFI |
| No single registry | apt, vcpkg, Conan, system layouts, tarballs |
| No single ABI | `_GLIBCXX_USE_CXX11_ABI`, MSVC `/MD` vs `/MT`, sanitizers on/off, C++ standard version |

The missing ABI weighs the most. Linking translation units that compiled the
same header under different flags is an ODR violation — yet **the link
succeeds and the program breaks at run time**. Existing systems do not detect
the mismatch.

## 2. Goals

- **Cut reconfiguration latency** — a change to one manifest file must not
  re-run the whole evaluation
- **Expressiveness and diagnosability** — typed values, located diagnostics,
  provenance tracking
- **Detect ABI mismatches** — treat them as failures, not as something to
  propagate
- **Reproducibility** — lock the toolchain too, and eliminate unrecorded
  inputs
- **Developer experience** — a language server, cross execution, and debugger
  configuration as one unit
- **Incremental adoption** — existing package sources keep working, and
  migration proceeds target by target

## 3. Non-goals

- Fully automatic migration of existing CMake projects
  ([ADR-0005](adr/0005-migration.md))
- Monorepo scale, where the whole repository is one graph (the assumption is
  10^3–10^4 targets)
- Embedding arbitrary build steps. The escape hatch is limited to declared,
  sandboxed custom rules
- Designing a companion programming language

## 4. A classification of existing systems

| System | toolchain | dependency supply | ABI consistency | execution isolation |
|---|---|---|---|---|
| CMake | delegated (discovery) | delegated | propagation only, no verification | none |
| Meson | delegated (cross file) | delegated + wrap supplement | propagation only | none |
| Bazel / Buck2 | owned (registered) | owned (re-declared) | configuration in the key | sandbox + CAS |
| vcpkg | delegated | owned (ports) | triplet + ABI hash | binary cache only |
| Conan | half-owned (profiles) | owned (recipes) | explicit via package_id | none |
| Spack | owned (DAG nodes) | owned | variants fully expanded | prefix separation |
| Nix | owned (from libc up) | owned | everything in the input hash | isolation + CAS |
| Cargo | owned (rustup) | owned | language fixed to one | none |

The empty quadrant is **Bazel-class execution modeling and ABI awareness at
Cargo-class usability**. The main cost of adopting Bazel is not its execution
model but re-declaring every dependency in BUILD files — avoidable by
delegating dependency supply externally.

## 5. Positioning

The concept contains three independent parts; each stands on its own.

| # | Part | Realizable as | Difference from existing work |
|---|---|---|---|
| A | dependency resolution + locking + supply-chain policy + toolchain acquisition | possible as a layer on top of existing systems | competes with Conan / vcpkg; cooldown and approval flows are unclaimed territory |
| B | isolated execution keyed by ABI labels + CAS caching | requires full replacement | the Bazel / Buck2 quadrant; Cargo-class UX is unclaimed |
| C | an IDL for ABI boundaries and bidirectional FFI export | possible as a standalone tool | partially exists (meson-python, pybind11, cbindgen, abidiff) |

A alone stays a frontend over existing systems, with reproducibility bound to
the lower system's behavior. B is the only part demanding full replacement.

## 6. A note on FFI

FFI toward Python is already practical with Meson + meson-python (SciPy /
NumPy run it in production), so "callable from Python" is not a
differentiator.

What can differentiate:

- **Declared ABI boundaries with diff checking** — detect breaking changes by
  comparison against the previous version, and fail on versioning-rule
  violations
- **Generated symbol visibility** — derive version scripts / `.def` files
  from the ABI declaration, preventing unintended public symbols
- **Uniform export to multiple languages** — C ABI, CPython extensions,
  N-API, JVM Panama

## 7. Prior art worth studying

| Subject | What to take from it |
|---|---|
| Salsa (rust-analyzer) | incremental queries, early cutoff |
| Buck2 (DICE) / Bazel (Skyframe) | query graphs, configurations as first-class |
| Meson | non-Turing-complete DSL, wrap-based source supplement, pkg-config first |
| Conan `package_id` | making the ABI space explicit, compatibility rules |
| Zig | bundled toolchain, the pragmatism of `build.zig.zon` |
| podman | daemonless architecture, unprivileged execution |
| ninja | used as-is for the default backend of the execution layer |
