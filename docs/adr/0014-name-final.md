# ADR-0014: Adopt `dowel` as the official name

**Status**: Accepted

**Supersedes**: [ADR-0006](0006-naming.md)

## Context

[ADR-0006](0006-naming.md) selected `dowel` as a provisional name. Primary
trademark searches (USPTO TESS, J-PlatPat, EUIPO eSearch) and namespace
acquisition were left open, and until they were done the name was treated as
tentative: documents carried a "provisional name" caveat, and changes that
would embed the name broadly into identifiers and paths were avoided.

## Decision

`dowel` is the official name of the project. The provisional standing from
ADR-0006 is lifted: identifiers, paths, and documentation may embed the name
freely. `dowelup` follows the name ([ADR-0013](0013-self-acquisition.md)).

## Consequences

- Documents drop the "provisional name" caveat. Q5 (finalizing the name) in
  `docs/99-open-questions.md` is closed by this decision
- The namespace and trademark follow-ups noted in ADR-0006 remain worth doing,
  but they no longer gate the name. In particular, the PyPI name `dowel` is
  held by an unrelated project, so anything published to PyPI will need a
  distinct name
- Constraints that existed only because the name was provisional — such as not
  publishing the VS Code extension to the marketplace — are lifted. Whether to
  publish is now a separate decision on its own merits
