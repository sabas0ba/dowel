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
| [0021](0021-exclusive-features.md) | Features stay additive; exclusivity is declared, never inferred | Accepted |
| [0022](0022-test-cases.md) | A test target registers cases; dowel imposes no harness | Accepted |
| [0023](0023-harness-protocol.md) | The harness protocol is declared; dowel learns no test framework | Accepted |
| [0024](0024-debug-command.md) | `dowel debug` starts a declared debugger; the stub is declared, not guessed | Accepted |
| [0025](0025-bench-wall-clock.md) | `bench` measures whole-process wall-clock time; no framework is imposed | Accepted |
| [0026](0026-target-os-arch.md) | The target's OS and architecture are vocabulary of their own, derived from the triple | Accepted |
| [0027](0027-toolchain-style.md) | A toolchain declares its argument style; dowel spells what it assembles, and translates nothing else | Accepted |
| [0028](0028-probe-facts.md) | What was asked of a tool is recorded outside the project, keyed by the tool's identity | Accepted |
| [0029](0029-tarball-dependencies.md) | An archive dependency is pinned by its contents; fetching is delegated, verification is not | Accepted |
| [0030](0030-shared-libraries.md) | A shared library declares what it exports; the linker's form of that list is generated | Accepted |
| [0031](0031-toolchain-is-the-builds.md) | The toolchain is a property of the build, not of a package; the diagnostic says so | Accepted |
| [0032](0032-predicate-composition.md) | `when` predicates compose with `and` / `or` / `not`; `match` stays the way to choose | Accepted |
| [0033](0033-shared-toolchain-file.md) | A build can name a toolchain file it shares; the unit of override is one tool | Accepted |
| [0034](0034-closed-vocabulary.md) | The configuration vocabulary is closed; a project's own axes are features | Accepted |
| [0035](0035-template-kind.md) | A template shares manifest text, not a graph edge; it expands into the block it came from | Accepted |
| [0036](0036-prebuilt-distribution.md) | Prebuilt binaries come from release assets, verified by hash; the source build stays the one that proves its origin | Accepted |
| [0037](0037-store-gc.md) | The store is collected by compaction when asked, never automatically, and has no size cap | Accepted |
| [0038](0038-shared-inside-its-package.md) | A shared library's exported surface is a boundary toward its consumers; inside its own package it links statically | Accepted |
| [0039](0039-exports-are-checked.md) | `exports` is checked against the library that was built, by asking it | Accepted |
| [0040](0040-shared-library-version.md) | A shared library's ABI generation is declared, and it names the file | Accepted |
| [0041](0041-install.md) | `dowel install` copies the build tree's products; artifacts are linked to find their libraries relative to themselves | Accepted |
| [0042](0042-abi-label-components.md) | An ABI label is a set of components, so granularity is chosen per declaration instead of once for everyone | Accepted |
| [0043](0043-pkgconfig-generation.md) | An installed library describes itself in pkg-config, because dowel already reads that notation and could not write it | Accepted |
| [0044](0044-toolchain-acquisition.md) | A toolchain is fetched and pinned the way a dependency is, and it lives in the user's cache | Accepted |
| [0045](0045-offline.md) | Offline is a mode the build is told to be in, not a state it happens to be in | Accepted |
| [0046](0046-transfer-once.md) | An artifact is transferred once per destination, and a run that could not start is what sends it again | Accepted |
| [0047](0047-sysroot.md) | `sysroot()` is a path base, declared once beside the tools that need it | Accepted |
| [0048](0048-assembly.md) | Assembly is a third language, not C that happens to assemble | Accepted |
| [0049](0049-prebuilt-libraries.md) | A `lib` may name a library that already exists, so what another toolchain built becomes a first-class dependency | Accepted |
| [0050](0050-separate-assembler.md) | A build may declare its own assembler, and `.asm` is what needs one | Accepted |
| [0051](0051-source-language-is-closed.md) | A source's language is a closed question, and a tool that writes nothing has failed | Accepted |
| [0052](0052-transfer-record-drops-on-failure.md) | A run that did not pass drops the transfer record, not only a run that could not start | Accepted |
| [0053](0053-unverified-import.md) | An imported target says so in the manifest, and only a person clears it | Accepted |
| [0054](0054-generated-sources.md) | A source may be generated, and the generator runs where its output lands | Accepted |
| [0055](0055-tool-identity-in-freshness.md) | A tool's identity is an input, recorded as a file the actions depend on | Accepted |
| [0056](0056-direct-backend-parallelism.md) | The direct backend runs steps concurrently, and every backend orders by both edges and files | Accepted |
| [0057](0057-progress-is-shown-while-it-runs.md) | Progress is output, shown while the build runs, one line per step | Accepted |
| [0058](0058-a-command-a-backend-cannot-spell.md) | A command a backend cannot spell is refused, never altered | Accepted |
| [0059](0059-an-interface-directory-holds-the-interface.md) | A directory shipped as an interface is reported when it holds sources, not filtered | Accepted |
