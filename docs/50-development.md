# Development environment

Development happens inside the Nix / direnv environment defined by
[sabas0ba/dotfiles](https://github.com/sabas0ba/dotfiles), or inside the
container environment built from it with identical contents.

Tools are not installed directly on the host. `apt install` / `brew install` /
`npm install -g` / `pip install --user` and the like undermine
reproducibility and are not used.

## 1. Prerequisites

- [Nix](https://nixos.org/download/) (with flakes enabled)
- [direnv](https://direnv.net/) (optional; with it, `cd` alone enters the
  environment)
- Docker (optional; only if you use the container environment)

Installation steps, the version-pinning policy, and checksum verification are
covered by the dotfiles README. Running installers unverified
(`curl ... | sh`) is not done.

## 2. Setting up

```sh
git clone https://github.com/sabas0ba/dotfiles.git ~/repos/dotfiles
cd ~/repos/dotfiles
nix develop
scripts/check-env.sh
```

direnv setup and applying the home-manager configuration (`make hm-dry` /
`make hm-switch`) also follow the dotfiles README.

Work is done inside the development shell. If the environment variable
`DOTFILES_ENV` is `nix-develop`, you are inside it.

## 3. The container environment

An environment identical to the host can be built inside a container. The
Dockerfile carries no tool list of its own — it evaluates the dotfiles
`flake.nix` — so the contents match the host.

```sh
make docker-build   # build the image
make docker-shell   # enter the development shell inside the container
make docker-check   # smoke test inside the container
```

Running CI inside a `--network none` container is the policy that keeps CI
from becoming a third environment distinct from host and container.

**For now this is not in effect.** CI runs on GitHub Actions runners
([`.github/workflows/verify.yml`](../.github/workflows/verify.yml)). The path
for evaluating the dotfiles flake from this repository's CI is not set up,
and there is no present need to build it.

To keep the eventual migration cheap, **what is checked is decoupled from
where it runs**: local runs and CI both invoke the same entry point,
`scripts/verify.sh`, and the workflow does nothing but launch it. Moving the
execution environment later swaps the workflow's internals only; the
definition of the checks does not move.

## 3.1 Verification

Verification has a single entry point.

```sh
make verify      # run every stage, leaving results in .work/verify/
```

A failing stage does not stop the run; everything executes and the run fails
at the end. Knowing "what else passed" in the same run — not just "where it
failed" — makes the repair loop faster.

| Output | Contents |
|---|---|
| `.work/verify/summary.md` | per-stage results, pass counts, timing, startup measurements |
| `.work/verify/results.json` | the same, machine-readable |
| `.work/verify/logs/<stage>.log` | each stage's raw output |
| `.work/verify/startup.json` | startup-time measurements |

CI stores `.work/verify/` as an artifact and prints `summary.md` into the job
summary. Results of failing runs, especially, are preserved.

The stages: `fmt` / `clippy` / per-crate unit tests / parser robustness /
model integration and incrementality / e2e / scenarios / real-shaped fixtures /
diagnostics and coverage / examples / release build / startup measurement.
Startup measurement alone is informational and does not fail the run on
machine noise (a loose cap catches only clear regressions).

What each layer answers, and where a new test belongs, is in
[51-testing.md](51-testing.md).

To run a subset, stages can be skipped:

```sh
DOWEL_VERIFY_SKIP="e2e example" make verify
```

`make check` (formatting check + lints + tests) is for quick iteration and
leaves no records.

## 4. Adding tools

Tools the implementation needs (compilers, linkers, qemu, ninja, …) are added
to `nix/packages.nix` on the dotfiles side:

1. Add the package name to `nix/packages.nix`
2. If it is used as a command, also add it to `required_commands` in
   `scripts/check-env.sh`
3. Confirm `make check` passes

Do not add tool names to the `Dockerfile`; duplicated definitions drift.

Adding a dependency at all is confirmed in advance.

## 4.1 Cutting a release

```sh
scripts/release.sh 0.1.0
```

The script writes the version into `Cargo.toml`'s `[workspace.package]` — the
one place that holds it — refreshes `Cargo.lock`, runs the full verification,
commits, and creates an annotated `v0.1.0` tag. **It pushes nothing.**
Pushing the tag is what publishes the assets, so the irreversible step stays
with a person; the script prints the two commands to run.

`dowelup` has its own version and the script does not touch it: it is
installed once and then updates itself, so its version does not track the
compiler's.

Pushing the tag starts `.github/workflows/release.yml`, which

1. runs `make verify` again on the tagged commit — pushing a tag is a human
   act and the commit under it need not be one CI has seen,
2. builds `dowel` on a runner **of each published triple**, unpacks what it
   just packed, and runs `--version` from inside the archive,
3. publishes `dowel-<tag>-<triple>.tar.gz` beside its `.sha256`.

Those names are the ones `dowelup` reconstructs
([ADR-0036](adr/0036-prebuilt-distribution.md)); a mismatch makes it fall
back to building from source without saying why, so the triples it expects
(`dowel_up::prebuilt::PUBLISHED_TRIPLES`) and the ones the workflow builds
are compared by a test.

Adding a platform means adding a row to the workflow's matrix and a line to
that list; the test refuses one without the other. What cannot be added that
way is a triple no GitHub-hosted runner runs — the workflow builds each asset
natively so that "it runs on that machine" is confirmed on that machine.

## 5. Conventions

The dotfiles README is where the conventions live. Only project-specific
items are listed here; anything unlisted follows dotfiles.

### Inherited

- Commits follow Conventional Commits; one purpose per commit
- Feature work happens on a branch or worktree
- Temporary files go in the gitignored `.work/` inside the repository, never
  outside it (`/tmp` etc.)
- External artifacts are pinned uniquely; a reference by tag or branch name
  alone does not count as pinned
- No secrets in commits; machine-specific settings go in `.envrc.local`
  (outside git)
- Formatting is done by `make fmt`, not by hand
- Comments explain why a choice was made, not what the code does

### Project-specific

- The implementation language is Rust
  ([ADR-0007](adr/0007-implementation-language.md)). The core depends on the
  standard library only; adding an external crate requires agreement each
  time
- **The language of the program is English**: identifiers (including test
  names), string literals, diagnostics / logs / CLI output, generated files
  (`build.ninja` etc.), and metadata (`description` in `Cargo.toml`, workflow
  step names). The only exception is test data that exercises non-ASCII
  handling itself.
  **Documentation (`docs/` and the READMEs) is written in English.**
  Code comments and doc comments are in Japanese — the convention that
  comments record the reason for a choice benefits from the density of the
  native language
- Formatting is `make fmt` (`cargo fmt`); lints are `make lint`
  (`cargo clippy -D warnings`)
- Run `make check` (formatting check + lints + tests) before submitting
- Design decisions are recorded as ADRs in `docs/adr/`. To overturn one, mark
  it Superseded and add a new ADR
- **One sentence says one thing.** Reasons matter and are worth writing, but
  a reason chained onto a rule with a dash or a colon usually wants to be its
  own sentence. Two limits keep it honest: a diagnostic note fits on a
  terminal line (about 90 characters), and a sentence in `docs/` stays under
  roughly 260. Both are guides for writing, not checks — where a longer
  sentence genuinely reads better, it stays

## 6. Handing work to Claude Code

State explicitly:

- Work inside the dotfiles environment (Nix development shell or container)
- Refer to the dotfiles
  [`CLAUDE.md`](https://github.com/sabas0ba/dotfiles/blob/main/CLAUDE.md)
  and README
- This repository's `CLAUDE.md` contains repository-specific instructions and
  takes precedence over the shared conventions

The repository-root [`CLAUDE.md`](../CLAUDE.md) records the same.
