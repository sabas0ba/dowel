# Developer experience

> This is a design document for runners, debugger integration, and editor
> integration. Usage of runners and editors is in
> [63-guides.md](63-guides.md); what is implemented is in
> [91-implementation-status.md](91-implementation-status.md)
> (`dowel debug` is not implemented).

## 1. The runner abstraction

Declare an execution wrapper per target triple, and let
`dowel test --target <triple>` run through the wrapper transparently.

```toml
[runner.riscv64gc-unknown-linux-gnu]
command = "qemu-riscv64"
args    = ["-L", "/usr/riscv64-linux-gnu"]
```

Intended instantiations: qemu-user, qemu-system, SSH to real hardware,
serial-port flashing and execution. Making real hardware declarable is what
covers embedded use.

### Runners with transfer

When the target machine cannot see the build machine's file system, the
artifact is transferred before launch. Source and destination paths are not
written in the manifest; the implementation appends them
([ADR-0008](adr/0008-runner-transfer.md)).

```toml
[runner.aarch64-unknown-linux-gnu]
host       = "board.local"
remote_dir = "/tmp/dowel"
transfer   = ["scp", "-q"]
command    = "ssh"
args       = ["board.local"]
```

This expands to:

```
scp -q <build>/bin/unit_test board.local:/tmp/dowel/unit_test
ssh board.local /tmp/dowel/unit_test
```

`transfer` and `remote_dir` are specified together. The exit status is
whatever the launch command returns, so with `ssh` the target machine's exit
status is the verdict.

Prior art (the capability itself is established):

| System | Equivalent |
|---|---|
| Cargo | `target.<triple>.runner` |
| Meson | `exe_wrapper` |
| CMake | `CMAKE_CROSSCOMPILING_EMULATOR` |

## 2. Debugger integration

This is the real differentiator. The build system knows every input of the
action that produced an artifact, so it can **generate** debugger
configuration.

### 2.1 Resolving the reproducibility-vs-debugging trade-off

Normalizing source paths with `-ffile-prefix-map` for reproducibility breaks
the debugger's source resolution. The one party that knows the correct
`substitute-path` to compensate is the party that applied the mapping — the
build system.

**Reproducibility and debugging experience are inherently a trade-off, and
this layer is the only place it can be resolved.**

### 2.2 What `dowel debug <target>` does

- Pins down the sysroot, `substitute-path`, and shared-library search paths
- For cross execution, starts qemu's gdbstub and connects the gdb version
  tied to the toolchain
- Alternatively emits a DAP (Debug Adapter Protocol) launch configuration so
  an editor reproduces the same environment

### 2.3 Derived features

`dowel test --debug-failed` — rerun a failing test directly under the
debugger. Realizable from the same information.

## 3. Editor integration

Three paths; do not conflate them.

| Target | Role |
|---|---|
| the manifest language LSP | implemented in-house (another frontend of the core) |
| the C/C++ LSP | delegated to clangd; the build system is the **supplier** |
| debugging | supply information to DAP |

### 3.1 Supplying clangd

The only current interface is `compile_commands.json`, with these limits:

- It expresses a single configuration only
- It carries no C++20 module information

Immediate updates that track configuration switches can be improved on our
side; module support is also immature on clangd's side and cannot be solved
unilaterally.

### 3.2 The manifest language LSP

Initially restricted to diagnostics and hover. As long as the four
constraints in [20-architecture.md](20-architecture.md) are respected,
features can be added incrementally.

Plan on this part **never being finished** (it carries a permanent
maintenance cost).

Started as `dowel lsp`; speaks LSP on stdin/stdout. The editor is the
starting party and it exits with the editor, which distinguishes it from the
resident daemon rejected by [ADR-0002](adr/0002-no-daemon.md). The CLI never
depends on the language server's existence.

Hover explains the schema itself: property types and merge rules, each level
of a table header, builtin function signatures, configuration key domains.
The source is the same table `dowel schema dump` reads; nothing is kept
twice. Word identification walks the CST rather than evaluated values,
because explanations must appear even in files that contain errors.

The VS Code client lives in `editors/vscode/`. It starts `dowel lsp`,
receives diagnostics and hover, and adds syntax highlighting for
`dowel.build`.

Diagnostics come from a workspace model rebuilt per change: the open buffers
overlay the disk, and the model is loaded from every open manifest's
directory, so cross-file checks (`undeclared-dependency`, the feature
vocabulary, merge conflicts, cycles) reach the editor. The editor session
never fetches, never touches the store, and is dropped after each change.
What is still not produced — plan-stage checks that scan the file system —
is listed with reasons in `dowel_lsp::UNSUPPORTED`, and the check fails if a
listed diagnostic is in fact being emitted.

## 4. Designing for LLM assistance

LLMs generate from the distribution of their training corpus, so novel syntax
with no public corpus is their weakest ground. "LLMs exist, so unfamiliar
syntax is fine" does not hold.

What LLMs are reliably good at is the **repair loop**: with located,
structured diagnostics, convergence from wrong output to correct text is
fast.

The LLM premise therefore justifies not looser syntax but **investment in
diagnostic quality**. Concretely:

1. **JSON diagnostics** (`--message-format=json`), including fix suggestions
   (span + replacement) as rustc does, in a form agents can apply
   mechanically
2. **A machine-readable schema**: `dowel schema dump` prints every `kind` and
   property with types and merge rules. Supplying it as context compensates
   for the missing corpus
3. **A fast `dowel check`**: the faster the generate-verify loop, the faster
   the convergence. Incremental evaluation pays off here

## 5. C++20 modules

With modules, dependencies are unknown until sources are scanned. ninja can
express this with `dyndep`, but the generated structure gets complex, and
neither CMake nor Meson has matured here (the same shape of problem Fortran
has carried for decades).

Treating scan actions as first-class citizens of the graph fits incremental
evaluation well and is clearly open territory today — though parts depend on
the state of clangd support.
