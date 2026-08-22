# ADR-0054: A source may be generated, and the generator runs where its output lands

**Status**: Accepted

## Context

dowel compiles sources that already exist. A project whose sources come out
of a program — a parser from `bison`, a scanner from `flex`, message types
from `protoc`, a table from a script — cannot say so in a manifest. The only
way to build one is to run the generator by hand first, which makes the
generated file look like a checked-in source and puts its freshness outside
the build.

The gap is visible from inside the project. The Meson importer skips the
generated sources of every target it reads, and said why in its own code:

> 生成されたソースは写せない。生成する規則は dowel 側に無く、黙って落とすと
> 下書きが「組めるように見えて足りない」形になる。

An import that silently drops rules produces a draft that looks buildable
and is not. The importer cannot do better while the target language has no
way to spell the rule.

Two things about *how* a generation runs were settled by measuring rather
than by reasoning.

**ninja does not see an ordering that is not a file relation.** The plan
already carries `deps` between actions, and make and the direct backend use
them. The ninja backend emits `build <outputs>: <rule> <inputs>` — a
generated header that no compile lists as an input is not ordered before
that compile at all. With the generated outputs left out of the compiles'
inputs, a project whose only generated file is a header builds like this:

```console
$ dowel build
[1/2] CC obj/app/app/src_main.c.o
FAILED: .../obj/app/app/src_main.c.o
src/main.c:2:25: error: 'LIMIT' undeclared (first use in this function)
ninja: build stopped: subcommand failed.
```

The compile ran first and the generation never ran at all. The `-I` for the
generated directory was on the command line; the directory was empty.

**Where the program runs decides what the manifest has to spell.** A
generator invoked from the build directory writes into the build directory:
two targets that both generate `parser.c` overwrite each other, and every
`outputs` entry has to be spelled as a path the manifest cannot construct,
since the build directory's name depends on the configuration.

## Decision

**A target may declare how its sources are made.** The block is
`[<kind>.<name>.generate]`, whose items are inline tables keyed by a name —
the same shape as `artifacts`, `inspect`, and `cases`:

```toml
[bin.calc]
sources = glob("src/*.c")

[bin.calc.generate]
parser = { command = "bison", args = ["-d", "-o", "parser.c"],
           inputs = [file("src/parser.y")], outputs = ["parser.c", "parser.h"] }
```

**The program runs on the build machine, so it is a command, not a tool.**
`command` is spelled directly and is looked up on `PATH`. It is deliberately
not one of `[toolchain]`'s tools: those are a property of the build and are
selected per target triple ([ADR-0031](0031-toolchain-is-the-builds.md)),
and in a cross build the generator has to run on the machine doing the
building. A `command` that is not there is `missing-generator`, reported at
the declaration when the plan is made — the same position issue #50 chose
for a missing compiler.

**Each generation gets a directory, and runs in it.**

```
<build>/generated/<package>/<target>/<name>/
```

`outputs` are named relative to that directory, and the program's working
directory *is* that directory. The manifest needs no path arithmetic and no
new language function, two generations cannot collide, and the short
`-o parser.c` a generator expects is what the manifest writes. An output
naming a path outside its directory is `invalid-output`: dowel has to know
where each output lands in order to make it an input of the compiles.

The command line is `<command> <args...> <inputs...>` — arguments first,
then the inputs by position, the placement
[ADR-0008](0008-runner-transfer.md) already chose for every command dowel
assembles. `dir()` and `file()` in `args` expand to absolute paths, because
the working directory is on the output side and a relative path would not
reach the package.

**Outputs that are sources are compiled into the declaring target; the
directory joins its include path.** Which outputs are sources is the same
closed question as everywhere else
([ADR-0051](0051-source-language-is-closed.md)): `parser.c` is compiled,
`parser.h` is not. The directory is put on the include path without being
declared — a generated header is included by name, and requiring the author
to also write an `includes` entry pointing into the build directory would
be asking them to spell what dowel just decided.

`public = true` propagates that directory to dependents, the way
`public.includes` propagates: direct dependents always, and further only
along public dependency edges. The default is the declaring target alone.

**Every generated output is an input of every compile in the reach.** Not
only the generated sources, and not only the compiles that include them.
This is what makes the ordering hold under ninja, per the measurement above,
and it is a true dependency besides: editing what the generation reads
re-runs it and recompiles what could have seen its output. Being more
precise would mean knowing which translation units include which generated
header, which is what the depfile answers — after the first successful
compile, which is too late to order it.

**An action may carry a working directory.** `Action` and `Step` gain
`cwd`, written into `build-graph.json` when present. The direct backend
sets it on the process; ninja and make receive `cd <dir> && <command>`,
since neither can express a working directory otherwise and both already
take the command as a shell line.

## Consequences

- A generation whose `outputs` is empty in the current configuration is
  `generates-nothing` rather than an action that writes nothing. This is
  ADR-0051's position applied one level earlier: a step that cannot add
  anything to its target is a declaration error, not a runtime surprise.
- The generator is not sandboxed and its outputs are not verified beyond
  existence. A program that writes files it did not declare puts them in
  its own directory, where nothing reads them; ADR-0051's post-build check
  is what catches one that writes nothing it did declare.
- A generation is re-run when its `inputs` change, not when the generator
  itself changes. `bison` upgrading in place does not invalidate anything —
  the same hole a transform's tool has, and closing it means recording the
  program the way a toolchain is recorded
  ([ADR-0028](0028-probe-facts.md)).
- `make` still takes one output per step, so a multi-output generation
  needs `--backend=ninja` or `--backend=direct`. The make backend already
  refuses and says so; nothing new is required of it here.
- The Meson importer still drops `generated_sources`, but for a different
  reason: dowel can now spell the rule, and Meson's introspection answers
  only with the *paths* of the generated files, never the command that made
  them. Carrying them across needs a source for that command, not a target
  notation.
- Nothing yet lets a *dependency's* generated headers be found without that
  dependency opting in with `public`. That is the same rule the rest of the
  interface follows, and it is what makes the propagation checkable.
