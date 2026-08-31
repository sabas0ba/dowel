# `build-graph.json` — the build graph format

The description a backend runs on. It is what dowel hands to ninja, to make,
and to its own sequential runner, written out verbatim so that a backend
outside this repository can consume the same thing
([ADR-0018](adr/0018-backend-layer.md)).

```sh
dowel build --backend=graph
# wrote: .dowel/build/x86_64-unknown-linux-gnu-debug/build-graph.json
```

The `graph` backend writes the document and stops. Nothing is compiled — the
document is the deliverable, and running it is the reader's job. `dowel test`
and `dowel inspect` refuse this backend rather than reporting a build that did
not happen.

The same document is what `dowel graph --kind=action --format=json` prints.
There is one JSON description of an action graph, and it is the one the
backends run on: a fact missing from it is a broken build, not a stale
document.

## The document

```json
{
  "format": "dowel-build-graph",
  "version": 3,
  "build_dir": "/home/me/p/.dowel/build/x86_64-unknown-linux-gnu-debug",
  "steps": [
    {
      "id": 0,
      "kind": "cc",
      "target": "app:app",
      "description": "CC obj/main.c.o",
      "program": "cc",
      "arguments": ["-c", "/home/me/p/src/main.c", "-o", "…/obj/main.c.o", "-MD", "-MF", "…/obj/main.c.o.d"],
      "inputs": ["/home/me/p/src/main.c"],
      "outputs": ["…/obj/main.c.o"],
      "depfile": "…/obj/main.c.o.d",
      "deps": []
    },
    {
      "id": 1,
      "kind": "link",
      "target": "app:app",
      "description": "LINK bin/app",
      "program": "cc",
      "arguments": ["…/obj/main.c.o", "-o", "…/bin/app"],
      "inputs": ["…/obj/main.c.o"],
      "outputs": ["…/bin/app"],
      "deps": [0]
    }
  ],
  "artifacts": [{ "target": "app:app", "path": "…/bin/app" }],
  "default_outputs": ["…/bin/app"],
  "tool_stamps": [{ "path": "…/tools/cc-3f9a1c04.stamp", "identity": "/usr/bin/cc:1023032:1766091591" }],
  "prepared_files": [{ "path": "…/lib/core.map", "contents": "{ global: core_open; local: *; };\n" }],
  "link_aliases": [{ "path": "…/lib/libcore.so", "target": "libcore.so.2" }]
}
```

| Key | Type | Meaning |
|---|---|---|
| `format` | string | always `dowel-build-graph`. A reader that does not find this must refuse the file |
| `version` | integer | the format version. Bumped on any change a reader of the previous version would misread. Refuse an unknown version rather than guessing. Version 3 added `prepared_files` and `link_aliases`; version 2 added `cwd` and `tool_stamps`. Each changes what running the document does |
| `build_dir` | string | the working directory every step is run in. Also where a backend puts its own files |
| `steps` | array | the process launches, described below |
| `artifacts` | array | `{"target", "path"}` — the final artifact of each target that is in this graph |
| `default_outputs` | array of strings | what to build when nothing is named. Not the same as "every output": a derived file is here even though nothing consumes it |
| `tool_stamps` | array | `{"path", "identity"}` — files that record which program each step launches ([ADR-0055](adr/0055-tool-identity-in-freshness.md)). They appear in the steps' `inputs`, and **a reader has to write them before running anything**; see below. Empty when the graph has no steps |
| `prepared_files` | array | `{"path", "contents"}` — generated inputs such as a shared library's export map. **A reader has to write them before running anything**, and only when the contents differ; see below |
| `link_aliases` | array | `{"path", "target"}` — symbolic links needed before the build, currently the unversioned name beside a versioned shared library. `target` is relative to the link's directory. Empty on hosts where dowel cannot place symbolic links |

## A step

One step is one process launch.

| Key | Type | Meaning |
|---|---|---|
| `id` | integer | identifies the step within this document. `deps` refers to it. Not stable across runs |
| `kind` | string | `cc` / `ar` / `link` / `transform` / `generate`. Informational — the command is complete on its own — except that `ar` requires the removal below |
| `target` | string | the target this step belongs to, in `<package>:<target>` form. The same string diagnostics use |
| `description` | string | one line for progress output |
| `program` | string | the command to launch. A bare name is looked up on `PATH`; a name containing a separator is a path |
| `arguments` | array of strings | passed as-is. **Already split** — do not re-split, and do not pass the joined line through a shell unless you quote it yourself |
| `inputs` | array of strings | the files this step reads, as absolute paths. Complete except for headers, which arrive via `depfile` |
| `outputs` | array of strings | the files this step writes, as absolute paths. Their directories may not exist yet — create them |
| `depfile` | string | absent when the step declares no header dependencies. Present for `cc`: a **make-format** file the compiler writes, listing the headers actually read. It is written by the step itself, so it does not exist before the first run |
| `cwd` | string | absent for almost every step, which runs in `build_dir`. Present for `generate`, which runs in the directory its outputs land in ([ADR-0054](adr/0054-generated-sources.md)) so that they can be named relatively |
| `deps` | array of integers | steps that must complete first. Usually implied by `inputs`, but not always — a step may depend on one whose output it does not read. Neither field alone is the whole ordering; see below |

## What a reader must do

- **Run each step in `build_dir`, or in its `cwd` when it has one.** Some
  tools resolve relative paths in their own output against the working
  directory, and a `generate` step's outputs are named that way on purpose
- **Write `tool_stamps` before running any step.** Each entry's file must
  hold exactly its `identity` string. Write one **only when its contents
  differ** — the file's timestamp is what tells the steps their tool
  changed, so rewriting an unchanged stamp rebuilds everything, every time.
  A step whose stamp is missing has no rule to make it
- **Write `prepared_files` before running any step.** Create their parent
  directories and make each file hold exactly its `contents`. As with tool
  stamps, write only when the contents differ: an unchanged export map must
  keep its timestamp or the shared library relinks on every build
- **Place `link_aliases` before running any step.** Create each parent
  directory and make `path` a symbolic link whose stored target is exactly
  `target`. The target need not exist yet; the build step creates it later
- **Create output directories.** Steps do not create their own
- **Delete an `ar` output before running it.** `ar` appends; rebuilding into
  a stale archive leaves objects that are no longer part of the target
- **Read the depfile if you decide freshness yourself.** A `cc` step whose
  `depfile` is missing has *no* known header dependencies — treat it as out
  of date rather than up to date, or a header edit is silently missed
- **Order by `deps` *and* by the files.** A step must wait both for every
  step in its `deps` and for every step that writes one of its `inputs`.
  Neither list contains the other: `deps` carries orderings no file
  expresses, and `inputs` carries orderings the graph does not repeat as
  edges. Running one step at a time in an order that satisfies either list
  happens to satisfy both; **running steps concurrently does not**, and an
  ordering present in only one of the two then becomes a race
  ([ADR-0056](adr/0056-direct-backend-parallelism.md)). dowel's own backends
  each take the union — `direct` in its scheduler, `make` as prerequisites,
  `ninja` by emitting the edges its inputs do not already carry as
  order-only prerequisites
- **Refuse an unknown `format` or `version`.** Executing a build description
  you half-understand is the shortest path to silently building the wrong
  thing

Paths are absolute, so the document is not relocatable — it describes one
build directory on one machine. Regenerate it rather than moving it.

## Limits of a given backend

A backend may be unable to express a graph. `make` cannot name a path
containing whitespace, `:`, `#`, `$`, `%`, `;`, `=`, `\`, `*`, `?`, `[`, or
`]`, and the `make` backend refuses such a build, naming the path, instead of
writing a Makefile that builds something else. `ninja` has no such limit.
This is a property of the backend, not of the format.

Neither `ninja` nor `make` can spell a **line terminator** inside a command,
because both put the command on one line; both refuse such a step rather than
altering it ([ADR-0058](adr/0058-a-command-a-backend-cannot-spell.md)). The
format itself carries it without trouble — `arguments` is an array of strings,
and a JSON string holds a newline. A reader that runs `arguments` as `argv`
has nothing to do; a reader that joins them into a shell line inherits the
same limit, which is one more reason not to join them.
