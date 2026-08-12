# `dowel.toml` reference

Every table and key the implementation reads, and what happens when it is
missing or wrong.

A top-level table that `dowel.toml` does not read is `unknown-table`. When
its name belongs to `dowel.build`'s vocabulary the diagnostic says so.
`[runner.<triple>]` sits one table away from `[toolchain.<triple>]` and is
easy to write into the wrong file; without that, `missing-runner` would
insist a declaration is absent while the reader is looking at it
(issue #74). `[policy]` stays accepted: it is reserved and documented as not
yet acted on. Unknown **keys** inside `[package]` are still ignored (`edition`
and `[toolchain] sysroot` from the design examples are reserved the same
way); `[toolchain]` is the exception, where a misspelled key would silently
fall back to a default.

`dowel.toml` must stay strict TOML: function calls, `match`, postfix `when`,
and configuration references are rejected in value position with
`expression-in-strict-toml`. Anything that needs an expression belongs in
`dowel.build`.

## `[package]`

```toml
[package]
name    = "libfoo"
version = "0.3.1"
```

| Key | Type | Required | Behavior |
|---|---|---|---|
| `name` | string | yes | the package name, used in target references (`<package>:<target>`) and diagnostics. Missing: `missing-field`. Default while erroring: the directory name |
| `version` | string | no | the package version. Readable from `dowel.build` as `pkg.version` ([12-build-reference.md](12-build-reference.md), [ADR-0020](adr/0020-package-constants.md)), so the value a library reports at run time comes from here rather than being written a second time in a header. Not yet used for resolving this package as someone else's dependency. Default `0.0.0` |
| `description` | string | no | a one-line description of the package. Used as `Description:` in the pkg-config file `dowel install` writes ([ADR-0043](adr/0043-pkgconfig-generation.md)), which requires it; when absent the target name stands in, so a file is still produced and still validates |
| `targets` | list of strings | no | the target triples this package is for. When declared, any other triple — the host included — is refused with `unsupported-target` before building. Undeclared (the default) means the package builds for any triple. This is deliberately separate from `[toolchain.<triple>]`: a package that builds for the host but swaps tools when cross-compiling declares toolchains without narrowing its targets |

| `toolchains` | string | no | a file of shared toolchain declarations ([ADR-0033](adr/0033-shared-toolchain-file.md)), resolved relative to this `dowel.toml`. See below |

A missing `[package]` table is `missing-table`. A `dowel.toml` whose
directory has no `dowel.build` defines no targets but can still be depended
on for its metadata (in practice every package has both).

## `[toolchain]` and `[toolchain.<triple>]`

```toml
[toolchain]                              # applies to host builds
c   = "clang-19"
cxx = "clang++-19"

[toolchain.aarch64-unknown-linux-gnu]    # applies to --target=aarch64-unknown-linux-gnu
c   = "aarch64-linux-gnu-gcc"
cxx = "aarch64-linux-gnu-g++"
```

| Key | Type | Behavior |
|---|---|---|
| `url` | string | an archive holding the toolchain ([ADR-0044](adr/0044-toolchain-acquisition.md)). Requires `sha256`. Fetched once into `$XDG_CACHE_HOME/dowel/toolchains/<hash12>/` — the **user's** cache, not the tree, because the same archive is the same bytes everywhere and a toolchain is far larger than a dependency. Later runs touch no network. A failing fetch or a digest mismatch is `unfetchable-toolchain`, and the build stops rather than falling back to PATH |
| `sha256` | string | required with `url`: **64 hexadecimal digits**, the digest of the archive itself. Anything else is `unpinned-toolchain` — a URL is a name, and the bytes behind a name can change. Verified **before** unpacking |
| `style` | string | how dowel spells the arguments **it** assembles: `gnu` or `msvc` ([ADR-0027](adr/0027-toolchain-style.md)). Derived from the triple when absent (`*-msvc` → `msvc`), and this key overrides that derivation. It also decides the tools' defaults, so a project declaring nothing gets a coherent set. An unknown value is `invalid-value` |
| `c` | string | the C compiler command, default `cc` (`cl` under the MSVC style) for host builds. It must be on PATH at plan time (a value containing a path separator is probed as a path). When the table declares `url`, a relative path is resolved against the unpacked toolchain's root; an absolute path and a bare name are left alone, since those already mean a place on this machine and a PATH lookup ([ADR-0044](adr/0044-toolchain-acquisition.md)). Missing from PATH: `missing-toolchain`. Required in `[toolchain.<triple>]`: missing there is `missing-field` |
| `cxx` | string | the C++ compiler command, default `c++` for host builds. Required — and probed — only when the build contains C++ sources. Missing from PATH: `missing-toolchain` |
| `link` | string | the linker command. Empty by default under the GNU style, where the compiler driver links (and the C++ driver is chosen when the link closure contains C++); `link` under MSVC, where it is a separate program. Probed only when something is linked |
| `ar` | string | the archiver command, default `ar` (`lib` under the MSVC style). Required — and probed — only when the build produces a static library. Cross builds should declare it alongside `c` / `cxx` so archives are not created by the host's tool. Missing from PATH: `missing-toolchain` |
| `objcopy` | string | the object copier, default `objcopy`. Used by `[<kind>.<name>.artifacts]` to derive files from an artifact ([12-build-reference.md](12-build-reference.md)); probed only when such a declaration exists. Missing from PATH: `missing-toolchain` |
| `size` `nm` `objdump` `readelf` | string | reporting tools, each defaulting to its own name. Used by `[<kind>.<name>.inspect]` ([12-build-reference.md](12-build-reference.md)). An inspection is not part of the build graph, so these are not probed at plan time; `dowel inspect` reports a tool it cannot start |

Any other key is `unknown-property`, with a suggestion — a misspelled tool
would otherwise silently fall back to its default, which for a cross
archiver means the host's `ar` quietly builds the archives.

### Sharing one file between several packages

Several consumers in one tree usually need the same triple-to-tools
mapping. `[package] toolchains` names a file that holds it
([ADR-0033](adr/0033-shared-toolchain-file.md)):

```toml
# cli/dowel.toml
[package]
name       = "cli"
toolchains = "../toolchains.toml"
```

```toml
# toolchains.toml — the same tables, in a file of their own
[toolchain.aarch64-unknown-linux-gnu]
c  = "aarch64-linux-gnu-gcc"
ar = "aarch64-linux-gnu-ar"

[toolchain.thumbv7em-none-eabihf]
c       = "arm-none-eabi-gcc"
ar      = "arm-none-eabi-ar"
objcopy = "arm-none-eabi-objcopy"
```

- **A local declaration wins, one tool at a time.** Declaring
  `[toolchain.thumbv7em-none-eabihf] c = "..."` next to the `toolchains`
  key replaces the compiler for that triple and leaves `ar` and `objcopy`
  coming from the file. Overriding per triple would mean rewriting the
  whole table to change one tool
- The file holds `[toolchain]` and `[toolchain.<triple>]` and nothing
  else; another table there is `unknown-table` rather than ignored
- It cannot name a further file — reading is one level
- A file that cannot be read is `unreadable-toolchains`, reported at the
  key that names it
- **A dependency's `toolchains` is not read**, exactly as its
  `[toolchain]` is not ([ADR-0031](adr/0031-toolchain-is-the-builds.md)).
  This gives a consumer one place to write the table, not a way to
  inherit one

### The argument style

Declaring a tool's **name** is not enough to use it: the arguments dowel
assembles have a spelling, and it differs between toolchains
([ADR-0027](adr/0027-toolchain-style.md)).

| | GNU | MSVC |
|---|---|---|
| include path | `-Iinc` | `/Iinc` |
| define | `-DA=1` | `/DA=1` |
| debug / no optimisation | `-g -O0` | `/Z7 /Od` |
| compile | `-c src.c -o out.o` | `/c src.c /Fo:out.obj` |
| header dependencies | `-MD -MF out.o.d` | `/showIncludes` |
| archive | `ar rcs libcore.a …` | `lib /OUT:core.lib …` |
| link output | `-o bin/app` | `/OUT:bin\app.exe` |
| object / archive names | `.o`, `lib<name>.a` | `.obj`, `<name>.lib` |

`-MD` is why this cannot be left to the user: under MSVC it is a valid flag
meaning "link the dynamic CRT". A request for a dependency record would be
read as a choice of ABI.

**Only what dowel assembles is spelled per style.** The `flags` and
`link_flags` written in a manifest pass through untouched — translating them
would mean holding a table of flag equivalences, which is to say knowing the
compiler. A project building for MSVC writes MSVC flags.

Header dependencies differ in mechanism, not only spelling: MSVC writes no
record, it prints one. Whoever runs the compiler folds those lines into the
same `.d` file, so everything that reads the record stays style-agnostic.
The consequence is that under MSVC the record is not shared across backends
(ninja keeps its own in `.ninja_deps`), so switching backends costs one
extra recompile.

The toolchain is selected by the target triple, the same way
`[runner.<triple>]` is (issue #42). The plain `[toolchain]` table is the
declaration for host builds; it never applies to another triple. Passing
`--target=<triple>` for a triple with no `[toolchain.<triple>]` declaration
is refused before building, with `missing-toolchain`. Building with the
host compiler would place host artifacts under that triple's name, and the
mistake would surface much later — as a runner's `Invalid ELF image for
this architecture`, or a debugger showing the wrong architecture. Likewise, a cross build whose sources contain C++ requires
`cxx` in the triple's table; falling back to the host `c++` is refused.

If a dependency package declares a toolchain different from the one the
build uses, planning warns with `toolchain-mismatch` — ABI checking assumes
a single pinned toolchain per build. Only declarations that apply to the
current target triple participate in this comparison.

## `[[dependencies]]`

```toml
[[dependencies]]
name = "libgreet"
path = "../libgreet"

[[dependencies]]
name = "bar"
git  = "https://github.com/example/bar"
rev  = "9f3c0a1e2b7d4856c0f1a93e5d2b8c4770ae6135"

[[dependencies]]
name   = "mylib"
url    = "https://example.org/mylib-1.0.tar.gz"
sha256 = "b3d6cd8f6460100d3e67a2acc5bbe8ba6bb2c3a65a86e61ff8f353061fc1fe96"

[[dependencies]]
name     = "zlib"
version  = "1.3"        # resolved via the system pkg-config
optional = true
```

Each `[[dependencies]]` entry declares one package this package may use.
Declaring it here creates no edge by itself — a target must also reference
it with `dep("name")` in `dowel.build` ([12-build-reference.md](12-build-reference.md)).

| Key | Type | Behavior |
|---|---|---|
| `name` | string | required. Missing: `missing-field`. The name used by `dep("...")` and, for optional dependencies, by the feature flag that activates them |
| `path` | string | a directory containing another dowel package, relative to this `dowel.toml`. The path must exist and contain a manifest (`missing-manifest` otherwise) |
| `git` | string | a git URL (anything `git` itself accepts, including local paths). Requires `rev`. Fetched once into `.dowel/deps/<name>-<rev12>/`; later runs never touch the network. A failing fetch is `unfetchable-dependency` |
| `rev` | string | required with `git`: a **full 40-digit commit sha**. Branches, tags, and abbreviated shas are refused with `unpinned-dependency` — a name-only reference does not count as pinned. Because the rev pins the content exactly, git dependencies need no lock file |
| `url` | string | an archive to download and unpack ([ADR-0029](adr/0029-tarball-dependencies.md)). Requires `sha256`. Fetched once into `.dowel/deps/<name>-<hash12>/`; later runs never touch the network. Fetching runs `curl` (or `wget`) and `tar`, which must be on PATH; a failing fetch is `unfetchable-dependency`. If the archive contains exactly one top-level directory it is stripped, the usual `name-version/` wrapper |
| `sha256` | string | required with `url`: **64 hexadecimal digits**, the digest of the archive itself (not of the unpacked tree). Anything else is `unpinned-dependency`, exactly as an unpinned `rev` is — a URL is a name, and the bytes behind a name can change. The archive is verified **before** it is unpacked, and a mismatch reports both the expected and the received digest |
| `version` | string | a system package, resolved through **pkg-config** ([ADR-0015](adr/0015-version-deps-pkgconfig.md)). `name` is the pkg-config module name; the version is a **minimum** (`--atleast-version`). `--cflags` / `--libs` become the dependency's public flags and link flags. Absent module, too-low version, or missing pkg-config: `unsatisfied-dependency`. Resolutions are recorded in `dowel.lock` (below) |
| `optional` | bool | default `false`. An optional dependency participates only when a feature flag with the same name is enabled. When inactive, neither the edge nor the node exists — the package is not even loaded |
| `when` | inline table | reserved for conditional dependencies (`when = { os = "windows" }`). Parsed, but **not yet honored** — the dependency is treated as unconditional |

A dependency has **exactly one** source. An entry with none of `path` /
`git` / `url` / `version` is `incomplete-dependency`; an entry with two or
more is `conflicting-dependency-source`, naming each one (issue #79).
Accepting two would leave one declaration unread, with the manifest not
saying which. The source of a library gets switched during development —
`path` while it is being edited, `git` or `version` once it is published —
and leaving the old key behind still builds for whoever has the tree.

## `[features]`

```toml
[features]
default   = ["zlib"]
zlib      = []
png       = ["zlib", "libpng/simd"]     # also enables `simd` in the dependency
exclusive = [["headless", "x11"]]       # these two are never on together
```

Each key declares a feature flag; its value is the list of other features it
enables (transitively closed, cycle-safe). Values must be arrays of strings
(`type-mismatch` otherwise). `default` and `exclusive` are reserved and are
not feature names.

A feature **belongs to the package that declares it**
([ADR-0017](adr/0017-feature-forwarding.md)). Two packages may use the same
feature name for unrelated things, and enabling one never enables the other.
A value of the form `dep/feat` **forwards**: it enables `feat` in the
dependency `dep` rather than becoming a feature of this package.
`dep` must be declared in `[[dependencies]]` (`undeclared-dependency`
otherwise), and `feat` must be declared in that dependency's `[features]`
(`unknown-feature`). The second is reported at the forwarding site: an
unforwarded typo would evaluate to false in the dependency, which looks
exactly like a feature deliberately left off.

- `default` is special: it is included unless `--no-default-features` is
  passed. `default` itself is never a feature name
- `exclusive` declares sets of features that must **not** be active
  together, as an array of arrays ([ADR-0021](adr/0021-exclusive-features.md)).
  Two or more of a group active for this package is
  `conflicting-features`, naming them and where each came from — most often
  `default`, which `--no-default-features` drops. Names in a group must be
  declared in this table (`unknown-feature`); a group of fewer than two
  names forbids nothing and warns (`empty-exclusive-group`).

  Features stay additive: `--features=x11` never turns `headless` off.
  Exclusivity is a **declared constraint**, never inferred — dowel cannot
  see that two source files define the same symbol. It is what makes the
  `lib` case fail at all: two implementations in one archive otherwise build
  green and the linker keeps whichever member it reached first (issue #82).
  For choosing between two implementations, `match feature.<name>` is the
  spelling that always selects one
  ([12-build-reference.md](12-build-reference.md))
- The set of valid feature names is exactly the keys of this table. An
  unknown name fails with a diagnostic and a suggestion, whether it comes
  from `--features` on the command line or from a `feature.<name>` reference
  in `dowel.build`
- An enabled feature named like an `optional` dependency activates that
  dependency
- Feature selection is fixed before loading; inside `dowel.build`, features
  are read as `feature.<name>` in `when` conditions
  ([12-build-reference.md](12-build-reference.md))

## `dowel.lock`

`path` dependencies are local content and `git` dependencies are pinned by
their rev, so neither needs locking. `version` dependencies resolve against
whatever the system has, so each resolution is recorded in `dowel.lock` at
the workspace root ([ADR-0015](adr/0015-version-deps-pkgconfig.md)):

```toml
[[package]]
name    = "zlib"
version = "1.3.1"
source  = "pkg-config"
```

- A resolution with no entry is **appended**
- A resolution matching its entry is silent
- A resolution differing from its entry warns with `lockfile-drift` and the
  file is **never rewritten silently** — delete the entry (or the file) to
  accept the new resolution

The lock detects drift; it does not restore anything. A system package
cannot be fetched, so the promise is "you will notice a changed
environment", not "you will get the same bits".

## What is deliberately absent

- **No expressions** — enforced, see above
- **No target definitions** — targets live in `dowel.build`
- **No version ranges** — a `version` constraint is a lower bound only;
  comparison is delegated to pkg-config itself
