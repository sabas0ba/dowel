# Acquiring dowel and switching versions (dowelup)

`dowelup` acquires dowel itself, pins a version per project, and switches
between versions transparently. The design decisions are in
[ADR-0013](adr/0013-self-acquisition.md).

## Installation

Releases are not set up yet, so bootstrap by building from this repository.

```sh
cargo build --release -p dowel-up        # target/release/dowelup
dowelup shim ~/.local/bin                # create a link named `dowel`
```

`dowelup shim <dir>` creates `<dir>/dowel` as a symlink to dowelup. That
`dowel` selects a version on every launch and execs the selected binary.

## Version specifiers

| Form | Meaning |
|---|---|
| `stable` | the latest release tag upstream |
| `nightly` | the tip of the default branch |
| `nightly-YYYY-MM-DD` | the last commit on the default branch by the end of that day (UTC) |
| `X.Y.Z` | tag `vX.Y.Z` or `X.Y.Z` |
| `branch:<name>` | the tip of a branch |
| `tag:<name>` | any tag |
| `<sha>` | a commit; a unique prefix (7+ characters) suffices |

Every form is resolved to a commit sha at `install` / `pin` / `default` time;
from then on the sha is the source of truth. Upstream has no release tags
yet, so `stable` and `X.Y.Z` cannot resolve until one appears. The pin holds
the sha whichever way the binary arrived, so a project pinned to a sha gets
the same version whether its developers took the published binary or built
it themselves.

## Commands

```sh
dowelup install 0.1.0              # a release: takes the published binary
dowelup install 0.1.0 --from-source    # ... or builds it instead
dowelup install nightly            # resolve, build, place under versions/<sha>/
dowelup install branch:feature     # a specific upstream branch
dowelup install 2915da5ab          # a specific commit (prefix suffices)
dowelup list                       # what is installed; `*` marks the default
dowelup default nightly            # the version used where no pin exists; fetches if missing
dowelup pin nightly                # write the resolved sha to .dowel-version
dowelup which                      # the path of the binary that would run here
dowelup run branch:feature -- check    # run a specific version, bypassing selection
dowelup uninstall branch:feature   # remove it
```

Resolution is delegated to `git`. The default upstream is
`https://github.com/sabas0ba/dowel`, overridable with `--upstream <url>` or
the environment variable `DOWELUP_UPSTREAM`.

## Where the binary comes from

A release specifier (`stable`, `X.Y.Z`, `tag:`) takes a **published
binary** from the release assets, verified against the `.sha256` published
beside it ([ADR-0036](adr/0036-prebuilt-distribution.md)). Everything else
— `nightly`, `branch:`, a bare sha — has no asset to take, and builds from
source with `cargo`. `--from-source` forces the build in every case.
`install` says which path it took.

The two differ in what they let you conclude:

| | trusts | proves |
|---|---|---|
| source build | the git history, pinned by sha | the binary was built from *this* commit |
| published binary | the release publisher, over HTTPS | the bytes match what the publisher listed |

The checksum catches a truncated download, a proxy that mangles bytes, or
a stale mirror. It does not catch a compromised release: whoever can
replace the tarball can replace the checksum next to it. Nothing checks
that a published binary was built from the sha it is installed as — if
that matters, use `--from-source`, which needs `cargo` on PATH.

Fetching is delegated to `curl` (or `wget`), unpacking to `tar`.

The output split matches dowel itself ([60-cli.md](60-cli.md)): stdout
carries artifacts (resolved shas, listings, paths), stderr carries progress
and errors.

## Version selection

`dowel` (the shim) selects a version in this order:

1. A leading `+<specifier>` argument (e.g. `dowel +nightly check`), chosen
   from what is installed
2. The first `.dowel-version` found walking up from the current directory
3. The default set by `dowelup default`

Selection never touches the network. If the selected sha is not installed,
the error tells you to run `dowelup install <sha>`.

## The pin file

`.dowel-version` is written by `dowelup pin <specifier>`. It contains the
resolved sha and a comment recording which specifier it was resolved from.

```
# Managed by dowelup. Resolved from "nightly".
2915da5c1f0e3b7a9d2c4e6f8a0b1c2d3e4f5a6b
```

If a channel or branch name is written by hand, the shim refuses to resolve
it and points to `dowelup pin`. This enforces the rule that a reference by
branch name alone does not count as pinned
([50-development.md](50-development.md) section 5).

## Layout

| Path | Contents |
|---|---|
| `$DOWELUP_HOME` (default `~/.dowel`) | the root of dowelup's state |
| `versions/<sha>/bin/dowel` | an installed binary |
| `versions/<sha>/origin` | which specifiers and upstream it was resolved from; appended to when the same sha is installed again under a different specifier |
| `upstream.git` | the mirror used for resolution and fetching |
| `default` | the sha used where no pin exists |
| `tmp/<sha>` | a build work tree; removed on success, kept on failure for inspection |
