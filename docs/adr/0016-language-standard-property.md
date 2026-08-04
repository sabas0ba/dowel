# ADR-0016: The language standard is a typed property merged by maximum

**Status**: Accepted

## Context

Until now the C/C++ standard was written as a raw flag:

```toml
[lib.foo.private]
cxx_flags = ["-std=c++20"]
```

Three things follow from that, none of them good.

- **The value is opaque.** `-std=c++2a`, `-std=c++20`, and `-std=gnu++20` are
  three different strings that dowel cannot tell apart, cannot validate, and
  cannot compare. A typo reaches the compiler and comes back in the
  compiler's words, not the manifest's
- **It does not propagate meaningfully.** `cxx_flags` merges with `append`,
  so a library requiring C++20 and a consumer asking for C++17 produce
  `-std=c++20 -std=c++17` — the last one wins, silently compiling the
  library's C++20 headers under C++17
- **It is unavailable to the ABI label.** Q2
  (`docs/99-open-questions.md`) lists the C++ standard version as a candidate
  component of the label. A flag string inside a list cannot be a component
  of anything

## Decision

`c_std` and `cxx_std` are typed properties of the `public` / `private`
blocks, each with a **closed, ordered vocabulary** (`c89 … c23`,
`c++98 … c++26`). A value outside its vocabulary is the error
`unknown-standard`, with the accepted list and an edit-distance suggestion.
The check runs on the written value — including every `match` arm and
`when` branch — so a misspelling is not deferred until the configuration
that selects it.

They merge with a new rule, **`max`**: the highest standard reached along
the closure wins. This is the correct semantics for a language standard, and
it is what `must_equal` (the `abi` rule) would get wrong:

- A library requiring C++17 consumed by a C++20 binary is **correct** —
  compiling everything at C++20 satisfies both. `must_equal` would fail the
  build over a non-problem
- A library requiring C++20 consumed by a target asking for C++14 **raises**
  that target to C++20 — otherwise the library's public headers do not
  compile in the consumer

The property becomes `-std=<value>` for its own language only, placed
**before** `c_flags` / `cxx_flags` so that an explicitly written
`-std=` still wins (last occurrence wins in both gcc and clang).

## Consequences

- GNU dialects (`gnu++20`) are deliberately outside the vocabulary. A
  dialect is a different axis from a standard version and cannot be placed
  in one total order; `cxx_flags = ["-std=gnu++20"]` remains the way to ask
  for one, and it overrides the typed property by position
- `max` is a general rule, not a special case for standards: it applies to
  any property whose domain is a closed ordered vocabulary. `PropDef` grew a
  `domain` field, which supplies both the validation set and the order
- The standard is now a first-class value the ABI label can read when Q2 is
  decided. This ADR does **not** decide that the label includes it —
  composition stays open — only that the input now exists in typed form
- `abi` keeps `must_equal`. The two rules coexist because they answer
  different questions: an ABI label must be *identical* to be compatible,
  while a standard version must merely be *sufficient*
