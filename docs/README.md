# dowel documentation

The index of user-facing documents (how-to and reference) and internal
documents (design, development, planning).

## How-to

| Document | Contents |
|---|---|
| [62-getting-started.md](62-getting-started.md) | From installation to building, testing, and running your first project |
| [63-guides.md](63-guides.md) | Task-oriented how-tos: building, testing, configurations and feature flags, tracing provenance, cross execution, editors, the cache, CI integration |
| [61-acquisition.md](61-acquisition.md) | Acquiring dowel itself and switching versions (`dowelup`) |

A working example lives at [`examples/hello`](../examples/hello).

## Reference

| Document | Contents |
|---|---|
| [10-manifest.md](10-manifest.md) | The manifest reference: everything `dowel.toml` / `dowel.build` accept, types and merge semantics |
| [60-cli.md](60-cli.md) | The command reference: every option, output contract, exit status, machine-readable diagnostics |
| [91-implementation-status.md](91-implementation-status.md) | What works today: the not-yet-implemented list, measurements, divergences from the design documents |

The reference includes the full design, so parts of it are not implemented
yet. Unimplemented items are marked in place and listed in 91. Where the two
disagree, 91 describes the current state.

## Design (background and internals)

| Document | Contents |
|---|---|
| [00-overview.md](00-overview.md) | Goals, non-goals, positioning against existing systems |
| [20-architecture.md](20-architecture.md) | The incremental query engine, the persistent store, language-server internals |
| [30-devexp.md](30-devexp.md) | The design of runners, debugger integration, editor integration |
| [40-migration.md](40-migration.md) | Migration from existing build systems (design; the commands are not implemented) |
| [adr/](adr/README.md) | Decisions and their rationale (ADRs) |
| [90-roadmap.md](90-roadmap.md) | Implementation order and verification plan |
| [99-open-questions.md](99-open-questions.md) | Open questions |

## Developing this repository

| Document | Contents |
|---|---|
| [50-development.md](50-development.md) | The development environment (Nix / container) and conventions |
| [51-testing.md](51-testing.md) | Test-suite design: what each layer answers, and where a new test belongs |

## Publishing with GitHub Pages

This repository can be published as-is with GitHub Pages
(Settings → Pages → Deploy from a branch → `main` / `/ (root)`).
The configuration lives in [`_config.yml`](../_config.yml). When published,
relative links between Markdown files resolve to HTML, and each directory's
README becomes that directory's index page. Documents are written with
relative links only, so they read the same on the repository and on the site
without a separate build step.

## Numbering convention

The tens digit is the subject; the ones digit distinguishes documents within
a subject.

| Band | Subject |
|---|---|
| `0x` | Overall positioning |
| `1x` | The manifest language reference |
| `2x` | Internals |
| `3x` | Developer experience (runners, debuggers, editors) |
| `4x` | Migration from existing build systems |
| `5x` | Developing this repository |
| `6x` | User-facing documents (reference and how-to) |
| `9x` | Planning and current state |
| `99` | Open questions |

A new document goes into an existing band when it fits; a new band is added
only when a subject fits none. Numbers are never reassigned: document numbers
are referenced both from Markdown links and from comments in the source code,
and changing one breaks those references.

## Index

| Document | Contents |
|---|---|
| [00-overview.md](00-overview.md) | Goals, non-goals, positioning against existing systems |
| [10-manifest.md](10-manifest.md) | The manifest reference (`dowel.toml` / `dowel.build`), types and merge semantics |
| [20-architecture.md](20-architecture.md) | The incremental query engine, the persistent store, language-server internals |
| [30-devexp.md](30-devexp.md) | Runners, debugger integration, editor integration |
| [40-migration.md](40-migration.md) | Migration from existing build systems |
| [50-development.md](50-development.md) | The development environment (Nix / container) and conventions |
| [51-testing.md](51-testing.md) | Test-suite design: what each layer answers, and where a new test belongs |
| [60-cli.md](60-cli.md) | The command reference, output contract, logging and debugging |
| [61-acquisition.md](61-acquisition.md) | Acquiring dowel itself and switching versions (`dowelup`) |
| [62-getting-started.md](62-getting-started.md) | How-to: from installation to your first build |
| [63-guides.md](63-guides.md) | Task-oriented how-to guides |
| [90-roadmap.md](90-roadmap.md) | Implementation order and verification plan |
| [91-implementation-status.md](91-implementation-status.md) | Implementation status, measurements, divergences from the design documents |
| [99-open-questions.md](99-open-questions.md) | Open questions |
| [adr/](adr/README.md) | Decisions and their rationale |

## Document conventions

- Decisions are recorded as [ADRs](adr/README.md). To overturn a decision,
  mark its ADR as Superseded and add a new one; existing ADRs are never
  rewritten
- Open questions are collected in [99-open-questions.md](99-open-questions.md).
  Once decided, an item moves to an ADR and is deleted from the list
- Planning and current state are kept separate: [90-roadmap.md](90-roadmap.md)
  is the plan, [91-implementation-status.md](91-implementation-status.md) is
  the current state. Where they disagree, the latter wins
- When the implementation diverges from a design document, the divergence is
  recorded in the "Divergences from the design documents" section of 91
- User-facing documents (10 / 6x) describe what works; anything not yet
  implemented is marked as such in place

## What is machine-checked

Documentation inconsistencies break neither the build nor the tests, so they
go undetected unless checked. `crates/dowel-cli/tests/docs.rs` covers what can
be judged mechanically.

| Target | Failure condition |
|---|---|
| Relative links | The target does not exist |
| Documents named from sources and scripts | A document number changed while a non-link reference remained |
| The index above | A document was added but not listed, or removed while its entry remained |
| The table in [adr/README.md](adr/README.md) | An ADR was added but not listed, or the reverse |
| The crate table in [91-implementation-status.md](91-implementation-status.md) | A crate was added but not listed, or the reverse |

The correctness of the prose itself is not checked. The design is described in
[51-testing.md](51-testing.md).
