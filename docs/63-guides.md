# How-to guides

Task-oriented how-tos. Everything described here is implemented. Installation
and the first build are in [62-getting-started.md](62-getting-started.md), the
complete option list in [60-cli.md](60-cli.md), and the manifest syntax in
[12-build-reference.md](12-build-reference.md).

## 1. Building

```sh
dowel build                      # build every bin / test
dowel build app                  # by name: <target> or <package>:<target>
dowel build --config=release
```

- Switch configurations with `--config` (`debug` / `release`; default
  `debug`). Build directories are separated per configuration, so switching
  never clobbers the other's outputs
- The default backend is ninja. Where ninja is unavailable,
  `--backend=direct` runs the steps in process, `-j` at a time, with no
  external generator.
  `--backend=make` generates a `Makefile`, and `--backend=graph` writes the
  build description for a tool of your own ([14-build-graph.md](14-build-graph.md))
- `-j/--jobs` is the parallelism passed to the backend
- `compile_commands.json` is written on every build; suppress with
  `--no-compdb`

## 2. Running tests

```sh
dowel test                       # every test target
dowel test app:unit              # by name
dowel test --nocapture           # pass output through (default shows failures only)
dowel test --failed              # rerun only what failed last time
dowel test --fail-fast           # stop at the first failure
dowel test --test-jobs=4         # run 4 at a time (default is sequential)
dowel test --no-run              # build only; do not run
dowel test --label=fast          # only cases tagged `fast`
dowel test app:suite/parse       # one case, by the label the output prints
dowel test --no-run              # list what would run, without running it
```

One binary can register several tests, each with its own arguments, timeout,
expected verdict, and labels:

```toml
[test.suite.cases]
parse   = { args = ["parse"], timeout = 10 }
rejects = { args = ["bad"], should_fail = true }
heavy   = { args = ["heavy"], labels = ["slow"] }
```

Or, where the suite already enumerates itself, let the binary list its own
cases:

```toml
[test.suite.harness]
list = ["--list"]      # prints one case name per line
run  = ["--run"]       # this, then the name, runs one case
```

Results, `--failed`, and `--label` then work per case either way
([12-build-reference.md](12-build-reference.md)).

- Pass/fail is the exit status (0 = success). A case killed by a signal
  fails, `should_fail` or not — that declares a nonzero exit, not a crash
- The working directory is the package root. A case that needs another one
  says so: `golden = { args = ["golden"], cwd = dir("tests/golden") }`
- The default is sequential because C tests may use shared resources (the same
  working directory, fixed ports, output files). Results are always displayed
  in request order
- The verdicts behind `--failed` persist in the build directory; verdicts of
  targets that were not run are kept

## 3. Switching configurations and feature flags

Branching on the manifest side uses `match` / `when`
([12-build-reference.md](12-build-reference.md) section 5). From the CLI:

```sh
dowel build --config=release
dowel build --features=zlib,png
dowel build --no-default-features
```

The domain of feature names is defined by `[features]` in `dowel.toml`.
Unknown names fail with a diagnostic — both when referenced from `dowel.build`
and when passed to `--features` — with suggestions. Switching `--config` /
`--target` does not re-run manifest evaluation (branch resolution is deferred
to the specialization stage).

## 4. Investigating "why"

Value provenance: where a propagated value came from, with source locations.

```
$ dowel why app:app includes

include/                          Path
  ← public.includes of target:foo       libfoo/dowel.build:18
    ← deps of target:app                app/dowel.build:7
```

Graphs: the target dependency graph and the action graph, as text / dot /
json.

```sh
dowel graph                              # target dependency graph
dowel graph --kind=action                # action graph
dowel graph --format=dot | dot -Tsvg -o graph.svg
```

Rebuild reasons and the actual command lines are in the log:

```sh
DOWEL_LOG=debug dowel build      # per-stage timing, graph sizes, freshness verdicts
DOWEL_LOG=trace dowel build      # dependency edges, the full command line of every action
```

The per-source breakdown of trace output is in
[91-implementation-status.md](91-implementation-status.md).

## 5. Cross compilation and runners

Declare the toolchain per target triple with `[toolchain.<triple>]` in
`dowel.toml`, and an execution wrapper with `[runner.<triple>]` in
`dowel.build`. `dowel build --target=<triple>` compiles with the declared
toolchain, and `dowel test --target=<triple>` launches through the wrapper
transparently. A `--target` with no declared toolchain is refused before
building (`missing-toolchain`) — the host compiler is never substituted.

```toml
# dowel.toml
[toolchain.riscv64gc-unknown-linux-gnu]
c  = "riscv64-linux-gnu-gcc"
ar = "riscv64-linux-gnu-ar"     # archives too — do not fall back to the host's ar
```

For bare-metal work, declare the tools that turn the ELF into something a
programmer can write, and the images become part of the build:

```toml
# dowel.toml
[toolchain.thumbv7em-none-eabihf]
c       = "arm-none-eabi-gcc"
ar      = "arm-none-eabi-ar"
objcopy = "arm-none-eabi-objcopy"
```

```
# dowel.build
[bin.firmware.artifacts]
bin = { tool = "objcopy", args = ["-O", "binary"] }
hex = { tool = "objcopy", args = ["-O", "ihex"] }
```

`dowel build --target=thumbv7em-none-eabihf` now produces `firmware.bin` and
`firmware.hex` next to the ELF, re-running the conversion only when the ELF
changed ([12-build-reference.md](12-build-reference.md)).

qemu:

```toml
[runner.riscv64gc-unknown-linux-gnu]
command = "qemu-riscv64"
args    = ["-L", "/usr/riscv64-linux-gnu"]
```

Real hardware over SSH. When the target machine cannot see the build
machine's file system, declare a transfer:

```toml
[runner.aarch64-unknown-linux-gnu]
host       = "board.local"
remote_dir = "/tmp/dowel"
transfer   = ["scp", "-q"]
command    = "ssh"
args       = ["board.local"]
```

This expands to the following. Source and destination paths are not written
in the manifest; the implementation appends them
([ADR-0008](adr/0008-runner-transfer.md)).

```
scp -q <build>/bin/unit_test board.local:/tmp/dowel/unit_test
ssh board.local /tmp/dowel/unit_test
```

- `transfer` and `remote_dir` are specified together
- Pass/fail is the exit status of the launch command; with `ssh`, the target
  machine's exit status is the verdict
- If no runner is declared for a triple that differs from the host, the launch
  is refused with a diagnostic beforehand (rather than surfacing as an
  `Exec format error` reported as a test failure)

### A library that supports several triples

**Each consumer declares its own toolchain, but the tree writes the table
once.** A dependency's `[toolchain.<triple>]` is read, and a mismatch is
reported, but it does not apply to the build. A toolchain is a property of
the build, not of a package
([ADR-0031](adr/0031-toolchain-is-the-builds.md)). The tool's *name* comes
from what is installed on the machine doing the build —
`aarch64-linux-gnu-gcc` on Debian, something else inside a vendor SDK —
which is not knowledge the library has.

What removes the copying is a shared file
([ADR-0033](adr/0033-shared-toolchain-file.md)): put the triple-to-tools
mapping in one place and have each consumer name it.

```toml
# cli/dowel.toml, gui/dowel.toml, fw/dowel.toml — one line each
[package]
toolchains = "../toolchains.toml"
```

A local `[toolchain.<triple>]` beside that key overrides **one tool** and
leaves the rest coming from the file, which is the shape a machine that
differs in one compiler actually has. Adding a triple is then one table in
one file, not one per consumer.

When a dependency does declare a toolchain for the requested triple and
the build has none, `missing-toolchain` reads out its value so the line
can be copied rather than looked up.

What a library *can* declare is which triples it supports, and it can do so
per target, not only per package:

```
# dowel.build of the library
[lib.core]
sources = glob("src/*.c")        # built for every triple

[test.vectors]
sources = [file("tests/vectors.c")]
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]
```

`targets` on a target takes the same spelling as `[package] targets` and
differs only in reach. The algorithm is built everywhere; its host-side
test is not built for a bare-metal triple at all — it does not appear in
that triple's plan, rather than failing `unsupported-target` there. Naming
it explicitly on a triple it does not support is still refused, because a
named target is a request.

Building or testing a consumer does not build the dependency's own tests:
the default reach of an unnamed `dowel build` / `dowel test` is this tree's
package. A dependency's tests are its author's to run — and where the
consumer targets a triple the library's tests were never written for, they
would otherwise fail a build whose manifest has nothing wrong with it.

## 6. Writing in an editor

There are three paths; they serve different files.

| Target | Path |
|---|---|
| `dowel.build` / `dowel.toml` | `dowel lsp` — a language server providing diagnostics and hover |
| C sources | clangd, fed by the `compile_commands.json` that `dowel build` writes |
| VS Code | the client in [`editors/vscode/`](../editors/vscode/README.md): launches `dowel lsp` and adds syntax highlighting |

`dowel lsp` speaks LSP on stdin/stdout; the editor is the one that starts it
(nothing stays resident). Hover shows property types and merge rules, builtin
function signatures, and configuration key domains. Diagnostics cross files:
with `dowel.toml` and `dowel.build` open, an unknown feature name, an
undeclared dependency, or a merge conflict with a dependency shows up in the
editor, against unsaved buffer contents. Plan-stage checks (glob expansion,
path resolution, toolchain probing) still belong to `dowel check`.

## 7. Managing the cache

Memoized evaluation results live under `.dowel/cache/`.

```sh
dowel cache info                 # store size
dowel cache gc                   # collect stores left by older formats
```

- `cache info` / `cache gc` do not read the manifests, so cleanup works even
  when a manifest is broken
- Deleting the store never loses correctness — only the cached speedup
- The writer is limited to one process (`flock`); a process that cannot take
  the lock reads only

## 8. Using from CI and tools

Output can be machine-readable. stdout carries artifacts and stderr carries
progress and logs — always — so piping is safe.

```sh
dowel check --message-format=json    # one diagnostic per line, with stable codes, locations, fix suggestions
dowel test  --message-format=json    # one test result per line
dowel build --log-format=json        # logs as JSON too
dowel schema dump                    # the schema and configuration vocabulary, machine-readable
```

- Exit status: 0 = success (including warnings only); anything else = an
  error. `dowel test` returns nonzero if even one test fails
- Diagnostic codes (`unknown-property` and so on) are a compatibility surface
- The output of `dowel schema dump` is also intended as context for LLM agents
  ([30-devexp.md](30-devexp.md) section 4)

## 9. Migrating from CMake

Start from a draft extracted out of the real configuration, then keep
checking against the old build until the port is equivalent:

```sh
# 1. have CMake emit its model, and draft manifests from it
mkdir -p build/.cmake/api/v1/query && touch build/.cmake/api/v1/query/codemodel-v2
cmake -B build ...
dowel migrate import build       # writes UNVERIFIED dowel.toml / dowel.build

# 2. edit the draft (promote public headers, restore conditionals), then
dowel migrate verify build/compile_commands.json
```

The draft is deliberately conservative — everything private, sources listed
explicitly — because it is a snapshot of one configuration and the intent is
lost ([60-cli.md](60-cli.md)).

Sources are matched one by one and their compile arguments compared after
normalization (spelling differences like `-DX` vs `-D X`, relative vs
absolute `-I`, and output/depfile flags don't count). A ported source with
differing arguments fails the run and lists each difference with its
direction; sources you haven't ported yet are reported but don't fail —
migration proceeds target by target. `--format=json` for CI
([60-cli.md](60-cli.md)).

## 10. Reading diagnostics

- Human output uses the rustc format: severity, a stable code, location
  labels, notes, and fix suggestions
- Unknown names (properties, functions, configuration keys, feature names,
  `match` arms, CLI options) come with edit-distance suggestions
- In `--message-format=json`, fix suggestions carry a span and a replacement
  string, and can be applied mechanically
- When stuck, close in from both sides: `dowel why` (value provenance) and
  `DOWEL_LOG=debug` (execution reasons)
