# dowel

A build system for C and C++. Positioned as an alternative to
CMake / Bazel / Meson, differentiated on three points:

1. **Incremental evaluation** — manifest evaluation is structured as a graph of
   memoized queries, cutting reconfiguration latency
2. **Types, diagnostics, provenance** — every value carries a type, a source
   location, and provenance; `dowel why` traces how a value propagated
3. **Developer experience** — a language server, runners (qemu and real
   hardware), and generated debugger configuration ship as one unit

There is no resident daemon. The source of truth lives in an on-disk store,
and the CLI process is self-contained.

## Quick start

There are no binary releases yet; build from source. You need a Rust
toolchain, a C compiler, and ninja (recommended).

```sh
git clone https://github.com/sabas0ba/dowel
cd dowel
cargo build --release
export PATH="$PWD/target/release:$PATH"

cd examples/hello/app
dowel check                  # run through planning, report diagnostics only
dowel build                  # generate ninja files and run them
./.dowel/build/*/bin/app

cd ../libgreet
dowel test                   # build and run the test targets
dowel why app:app includes   # trace how a value reached a target
```

Setting up a project of your own is covered in
[docs/62-getting-started.md](docs/62-getting-started.md).
To pin or switch versions of dowel itself, use `dowelup`
([docs/61-acquisition.md](docs/61-acquisition.md)).

## Documentation

User-facing documentation comes in two kinds: how-to guides and reference.

| What you want to know | Document |
|---|---|
| From installation to your first build | [docs/62-getting-started.md](docs/62-getting-started.md) |
| Task-oriented how-tos (testing, cross execution, editors, CI) | [docs/63-guides.md](docs/63-guides.md) |
| The manifest model (`dowel.toml` / `dowel.build`) | [docs/10-manifest.md](docs/10-manifest.md) |
| The `dowel.build` syntax and every configurable property | [docs/12-build-reference.md](docs/12-build-reference.md) |
| How declared values behave (merging, propagation, planning) | [docs/13-semantics.md](docs/13-semantics.md) |
| The command reference | [docs/60-cli.md](docs/60-cli.md) |
| What works today and what doesn't | [docs/91-implementation-status.md](docs/91-implementation-status.md) |

The full index, including the design documents (motivation, internals,
decision records), is at [docs/README.md](docs/README.md). The documentation
can be browsed as a site by publishing this repository on GitHub Pages
(`main` branch, `/ (root)`; configuration in [`_config.yml`](_config.yml)).

## Current state

Under active development. `dowel check` / `build` / `test` / `why` / `graph` /
`schema dump` / `cache` / `lsp` work today. It compiles C and C++ across
multiple packages, produces static libraries, links, and runs the result —
the compiler is chosen per source by extension, and a link involving C++
anywhere in its closure uses the C++ driver. Runners for cross execution
(`[runner.<triple>]`), incremental evaluation, persisted evaluation results,
and the language server (diagnostics and hover) all work.

Dependencies come as local `path`, sha-pinned `git`, or `version`
constraints resolved through the system pkg-config and recorded in
`dowel.lock`. The main things not implemented yet: `dowel debug` and the
`bench` kind.
Migration from CMake works: `migrate import` drafts manifests from the File
API and `migrate verify` checks them against the old build's compile
database. See
[docs/91-implementation-status.md](docs/91-implementation-status.md) for the
full list and measurements, and [docs/90-roadmap.md](docs/90-roadmap.md) for
the implementation plan.

Verification has a single entry point; local runs and CI run the same thing.

```sh
make verify      # run every stage, leaving results in .work/verify/
```

## Development

Development happens inside the Nix / direnv environment defined by
[sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles), or inside the
container environment built from it. Tools are not installed directly on the
host.

See [docs/50-development.md](docs/50-development.md) for setup,
[docs/51-testing.md](docs/51-testing.md) for the test-suite design, and
[CLAUDE.md](CLAUDE.md) for instructions aimed at Claude Code.

## About the name

A dowel is a woodworking fastener — the name reflects the project's focus on
joining: FFI and dependencies. `dowel` is the official name
([ADR-0014](docs/adr/0014-name-final.md)); the selection criteria, other
candidates, and the namespace/trademark survey are recorded in
[ADR-0006](docs/adr/0006-naming.md).

## License

[Apache-2.0](LICENSE)
