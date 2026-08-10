# ADR-0024: `dowel debug` starts a declared debugger; the stub is declared, not guessed

**Status**: Accepted

## Context

[30-devexp.md](../30-devexp.md) section 2 calls debugger integration the real
differentiator: the build system knows every input of the action that
produced an artifact, so it can *generate* the debugger's configuration
rather than asking a person to keep it in step. Section 2.2 lists what
`dowel debug <target>` should do — pin down the sysroot, `substitute-path`,
and library search paths; for cross execution start a gdbstub and connect
the toolchain's own gdb; or emit a DAP launch configuration instead.

Nothing of it existed. `dowel debug` was listed as not implemented, and the
one piece of information a debugger most needs from a build system — *which*
debugger, for *which* triple — had nowhere to be written: the toolchain
table names a compiler, an archiver, and reporting tools, but no debugger.

Two things had to be decided.

**Which debugger.** A cross build needs the gdb built for its triple
(`riscv64-linux-gnu-gdb`), not the host's. That is exactly what
`[toolchain.<triple>]` exists to say, and the tool table is the mechanism
that makes such a name selectable per triple.

**How a cross session attaches.** Under an emulator, debugging is two
processes: something hosts the program behind a stub, and a client connects
to it. dowel cannot know the flag that turns a given runner into a stub —
`-g 1234` for qemu-user, a `gdbserver` invocation over ssh for a board, and
neither is derivable from the other. Guessing here would produce a command
that looks right and hangs.

## Decision

`dowel debug <target>` builds the target and starts a debugger on its
artifact, with the package root as the working directory.

**The debugger is a toolchain tool.** `debug` joins the tool table with the
default `gdb`, so it is declared, defaulted, and selected per triple exactly
like `ar` or `objcopy`:

```toml
[toolchain.riscv64gc-unknown-linux-gnu]
c     = "riscv64-linux-gnu-gcc"
debug = "riscv64-linux-gnu-gdb"
```

It is probed only when `dowel debug` runs — the same rule the other tools
follow, and the reason a project that never debugs needs no gdb.

**The stub is declared.** A runner says how to host the program behind a
stub and where the client attaches:

```toml
[runner.riscv64gc-unknown-linux-gnu]
command       = "qemu-riscv64"
args          = ["-L", "/usr/riscv64-linux-gnu"]
debug_args    = ["-g", "1234"]         # these turn the runner into a stub
debug_connect = "localhost:1234"       # this is where the client attaches
```

Both are written out. The port appears twice, which is a wart, and it is the
honest one: dowel does not parse the runner's flags, so it cannot derive the
address from the arguments or the arguments from the address. A cross target
whose runner declares neither is refused with a diagnostic naming both keys,
rather than starting a host gdb on a foreign binary.

**`--dap` emits instead of starting.** The same resolved facts are written
to stdout as a DAP launch configuration, so an editor reproduces the session
dowel would have started. Nothing is launched.

**No `substitute-path` is emitted, because nothing is remapped yet.** dowel
does not pass `-ffile-prefix-map`, so there is no mapping to compensate for,
and emitting a substitution would be a fiction. When the reproducibility
side lands, this is where its counterpart belongs — section 2.1's point is
that only this layer can hold both halves.

## Consequences

- The debugger becomes a per-triple, declared, probed tool. A cross project
  stops needing a wrapper script whose only job is to pick the right gdb
- Adding it took one row in the tool table, one in the configuration
  vocabulary, and one use site — the extension recipe that table was built
  for. `tc.debug` is readable from `dowel.build` like any other tool
- A cross debug session is refused unless declared. That is a diagnostic
  where there would otherwise be a hang, and the cost is two keys in the
  manifest
- `--dap` output is a product, so it goes to stdout while the human-facing
  progress goes to stderr — the split the CLI already keeps
- `dowel test --debug-failed` (section 2.3) is not part of this. It needs
  the test job list and the debug launch to meet, which is now possible:
  both sides exist, and joining them is a separate, smaller change
- Only `bin` and `test` targets can be debugged. A library has nothing to
  start, and saying so is better than starting a debugger on an archive
