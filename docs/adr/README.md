# Architecture Decision Records

Decisions made along the way, recorded together with their rationale.
To overturn a decision, mark its ADR as Superseded and add a new one;
existing ADRs are never rewritten. Each record is kept in the language it
was originally written in.

| ADR | Decision | Status |
|---|---|---|
| [0001](0001-toolchain-vs-supply.md) | Own the toolchain, delegate dependency supply | Accepted |
| [0002](0002-no-daemon.md) | No resident daemon | Accepted |
| [0003](0003-manifest-split.md) | Split the manifest into `dowel.toml` and `dowel.build` | Accepted |
| [0004](0004-syntax.md) | The syntax is a TOML-style dialect; borrow elements whose semantics match existing languages | Accepted |
| [0005](0005-migration.md) | Migration is dynamic extraction only; no static translation | Accepted |
| [0006](0006-naming.md) | Use `dowel` as a provisional name | Superseded by [0014](0014-name-final.md) |
| [0007](0007-implementation-language.md) | Implement in Rust; the core uses the standard library only | Accepted |
| [0008](0008-runner-transfer.md) | Runner transfer paths are positional; no string interpolation | Accepted |
| [0009](0009-file-identity.md) | `FileId` is the hash of the normalized path | Accepted |
| [0010](0010-check-scope.md) | `check` runs through the planning stage | Accepted |
| [0011](0011-cutoff-and-provenance.md) | Derived fingerprints exclude spans; provenance reads bypass the memo | Accepted |
| [0012](0012-store-contents.md) | The store holds evaluation results only; files with diagnostics are not stored | Accepted |
| [0013](0013-self-acquisition.md) | dowel acquires itself via a separate binary; references are pinned to commit shas | Accepted |
| [0014](0014-name-final.md) | Adopt `dowel` as the official name | Accepted |
| [0015](0015-version-deps-pkgconfig.md) | Resolve `version` dependencies through pkg-config, record them in `dowel.lock` | Accepted |
| [0016](0016-language-standard-property.md) | The language standard is a typed property merged by maximum | Accepted |
| [0017](0017-feature-forwarding.md) | Features belong to the package that declares them; `dep/feature` forwards | Accepted |
| [0018](0018-backend-layer.md) | The output stage is a backend layer over one neutral build graph | Accepted |
| [0019](0019-c-abi-label.md) | `abi = "c"` names a boundary, not a language | Accepted |
| [0020](0020-package-constants.md) | `pkg.name` / `pkg.version` are package constants, readable in value position | Accepted |
