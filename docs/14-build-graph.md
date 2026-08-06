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
  "version": 1,
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
  "default_outputs": ["…/bin/app"]
}
```

| Key | Type | Meaning |
|---|---|---|
| `format` | string | always `dowel-build-graph`. A reader that does not find this must refuse the file |
| `version` | integer | the format version. Bumped on any change a version-1 reader would misread. Refuse an unknown version rather than guessing |
| `build_dir` | string | the working directory every step is run in. Also where a backend puts its own files |
| `steps` | array | the process launches, described below |
| `artifacts` | array | `{"target", "path"}` — the final artifact of each target that is in this graph |
| `default_outputs` | array of strings | what to build when nothing is named. Not the same as "every output": a derived file is here even though nothing consumes it |

## A step

One step is one process launch.

| Key | Type | Meaning |
|---|---|---|
| `id` | integer | identifies the step within this document. `deps` refers to it. Not stable across runs |
| `kind` | string | `cc` / `ar` / `link` / `transform`. Informational — the command is complete on its own — except that `ar` requires the removal below |
| `target` | string | the target this step belongs to, in `<package>:<target>` form. The same string diagnostics use |
| `description` | string | one line for progress output |
| `program` | string | the command to launch. A bare name is looked up on `PATH`; a name containing a separator is a path |
| `arguments` | array of strings | passed as-is. **Already split** — do not re-split, and do not pass the joined line through a shell unless you quote it yourself |
| `inputs` | array of strings | the files this step reads, as absolute paths. Complete except for headers, which arrive via `depfile` |
| `outputs` | array of strings | the files this step writes, as absolute paths. Their directories may not exist yet — create them |
| `depfile` | string | absent when the step declares no header dependencies. Present for `cc`: a **make-format** file the compiler writes, listing the headers actually read. It is written by the step itself, so it does not exist before the first run |
| `deps` | array of integers | steps that must complete first. Usually implied by `inputs`, but not always — a step may depend on one whose output it does not read |

## What a reader must do

- **Run each step in `build_dir`.** Some tools resolve relative paths in
  their own output against the working directory
- **Create output directories.** Steps do not create their own
- **Delete an `ar` output before running it.** `ar` appends; rebuilding into
  a stale archive leaves objects that are no longer part of the target
- **Read the depfile if you decide freshness yourself.** A `cc` step whose
  `depfile` is missing has *no* known header dependencies — treat it as out
  of date rather than up to date, or a header edit is silently missed
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
