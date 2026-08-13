# ADR-0047: `sysroot()` is a path base, declared once beside the tools that need it

**Status**: Accepted

Closes the divergence
[91-implementation-status.md](../91-implementation-status.md) recorded
against [30-devexp.md](../30-devexp.md) section 1. Builds on
[ADR-0044](0044-toolchain-acquisition.md), which gave a fetched toolchain a
root to be relative to.

## Context

The design documents have shown `sysroot()` since before there was an
implementation:

```toml
args = ["-L", sysroot()]
```

It could not be written. `PathBase::Sysroot` existed in the value type and
every path that reached it was refused with `unimplemented-path-base`,
noting that toolchain descriptions were a later phase.

For a cross build the sysroot is not optional decoration. The headers and
the libraries for the target live in it, and the compile line has to say
where. Without it, a tree either hardcodes an absolute path — which is the
thing pinning was for — or writes nothing and picks up the host's headers,
which fails later and in the compiler's words.

dowel has no string concatenation ([ADR-0004](0004-syntax.md)), so
`"--sysroot=" + path` is not available. That is what `Type::Word` already
solved for `link_flags`: a list element may be a `Str` or a `Path`, and a
`Path` expands to its absolute spelling (issue #70). The design example is
that shape exactly — `["-L", sysroot()]`, two words.

## Decision

**`sysroot()` names the toolchain's sysroot; `sysroot("usr/include")` names
a path under it.** It is the one builtin that takes no argument, because the
root itself is the common case and there is nothing to write for it.

**The sysroot is declared in the toolchain table**, beside the tools that
need it:

```toml
[toolchain.aarch64-unknown-linux-gnu]
url     = "https://example.org/gcc-13-aarch64.tar.xz"
sha256  = "…"
c       = "bin/aarch64-linux-gnu-gcc"
sysroot = "aarch64-linux-gnu/libc"
```

A relative path is resolved against a fetched toolchain's root, the way tool
commands are (ADR-0044) — a cross sysroot normally lives inside the toolchain
it belongs to. **The rule differs from the tools in one place**: a bare name
with no separator resolves too. For a tool that spelling means "look it up on
PATH"; a sysroot is never a PATH lookup, so `sysroot = "sysroot"` means the
directory of that name inside the toolchain.

**`flags`, `c_flags`, and `cxx_flags` become `List<Word>`**, joining
`link_flags`. The sysroot's use is on the compile line as much as the link
line, and `Word` is what already carried a path there.

**Writing `sysroot()` with none declared is `missing-sysroot`.** There is no
default. A default would put a path nothing declared into a command line, and
the failure would come back in the compiler's words about a header it could
not find.

## Consequences

- A cross-compiling tree can now say where the target's headers and libraries
  are without writing an absolute path — the toolchain archive and the
  sysroot inside it are pinned together by one `sha256`.
- A runner's `args` stays `List<Str>`, so the literal example in
  30-devexp.md is still not writable *there*. A runner's `args` is a command
  line for a program on the build machine, and widening it needs the runner's
  own package as a path base for `dir()` and `file()` — a separate question
  from this one. The divergence table records what changed and what did not.
- `--sysroot=<dir>` as one token still cannot be written, for the reason
  string concatenation does not exist. The two-word forms (`-I`, `-L`,
  `-isysroot`) are what the example used and what works.
- Nothing checks that the declared sysroot exists or contains anything. It
  reaches the compiler as a path, and a wrong one is reported by the compiler
  — the same standing as a tool that is on PATH but broken.
- `unimplemented-path-base` is gone. Both bases that the value type can carry
  now resolve.
