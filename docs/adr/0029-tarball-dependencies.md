# ADR-0029: An archive dependency is pinned by its contents; fetching is delegated, verification is not

**Status**: Accepted

## Context

Dependencies could come from three places: a local `path`, a `git`
repository pinned to a full sha, and a system package resolved through
pkg-config ([ADR-0015](0015-version-deps-pkgconfig.md)). A great deal of C
is distributed as neither — a release tarball on a web server, which is what
`FetchContent_Declare(URL ...)`, Meson's wrap files, and every distribution's
package recipe consume.

The gap is not exotic: a project wanting a specific release of a library
that publishes tarballs had to either vendor it, point `git` at a mirror
that may not exist, or rely on the system having it.

## Decision

A dependency may name a `url` and a `sha256`:

```toml
[[dependencies]]
name   = "mylib"
url    = "https://example.org/mylib-1.0.tar.gz"
sha256 = "b3d6cd8f6460100d3e67a2acc5bbe8ba6bb2c3a65a86e61ff8f353061fc1fe96"
```

- **The hash is required**, exactly as `rev` is for git. A URL is a name,
  and names are not pins — the bytes behind one can change, and for release
  tarballs they demonstrably do (re-rolled releases, mirrors that differ,
  a compromised host). `url` without `sha256` is `unpinned-dependency`,
  the same code the same situation gets for git.
- **The hash covers the archive, not the unpacked tree.** It is the thing
  that arrived over the network, so it is the thing to check, and it can be
  checked *before* unpacking — unpacking is an operation that lets the
  archive's contents decide where bytes land.
- **Fetching and unpacking are delegated**, following
  [ADR-0013](0013-self-acquisition.md): `curl` (falling back to `wget`) and
  `tar`. dowel implements neither HTTP with its authentication and redirects
  and TLS, nor the compression formats.
- **Verification is not delegated.** SHA-256 is implemented in-tree
  (`dowel_support::sha256`), because the tool that computes it differs per
  system — `sha256sum` on GNU, `shasum -a 256` on macOS, something else on
  Windows. A pin that can only be checked where a particular tool happens to
  exist is a weaker promise than a pin. `git` could be delegated to because
  fetching and verification are the same tool there.
- **The layout is the git checkout's**: `.dowel/deps/<name>-<hash[..12]>/`,
  with a completion marker written last, so a crash mid-fetch leaves
  something that is retried rather than trusted.
- **One wrapping directory is stripped, by looking rather than by
  declaring.** Archives conventionally contain a single `name-version/`
  directory. If exactly one directory is at the top, it becomes the root;
  otherwise the contents are used as they are. A `strip_components` key
  would be one more thing to get right in exchange for a case nobody has.

## Consequences

- The pin is stronger than git's in one respect: a git `rev` names a commit
  whose content the server could in principle serve differently, while the
  hash here is of the bytes themselves. It is weaker in another: there is
  no history to fetch a different version from, so a moved URL is a hard
  failure rather than a fetch of something older.
- Cold fetch needs `curl`/`wget` and `tar` on PATH. That is a wider
  dependency than git alone, and it is stated in the diagnostic rather than
  discovered by an exec failure.
- SHA-256 in-tree is ~150 lines and pinned by published test vectors. This
  is the first cryptographic primitive in the codebase; it is used for
  verification only, never for secrecy, so constant-time comparison is not a
  requirement (the value compared against is public, written in the
  manifest).
- No registry, no version resolution, no index. A `url` dependency names one
  archive, the way a `git` dependency names one commit. Anything resembling
  a package index — searching, version ranges, transitive resolution — is a
  separate decision that this does not prejudge, and the absence of a lock
  file for these follows for the same reason it does for git: the
  declaration already pins the content exactly.
- Nothing verifies that the archive's contents *are* a dowel package until
  it is unpacked and read. A tarball of something else fails the way any
  malformed package does.
