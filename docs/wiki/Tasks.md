# Tasks

A task is one action attached to a project — run a build type, move files between the host and the SSH target, or pack a zip — and it is the unit you run, preview, retry, and reference from a batch.

## What a task is

A project owns a list of tasks. Each task has exactly one `kind`, and there are seven: `build`, `push`, `pull`, `copy`, `move`, `delete`, `export`.

All seven kinds share **one flat JSON shape**: you fill in the fields your kind uses and leave the rest out, and fields belonging to another kind are ignored. `Task::validate()` runs at save time, so a structurally broken task is rejected with `taskInvalid` rather than failing halfway through a run. An empty or missing `kind` means `build`, which is why `{"manifest": "xcode"}` is a valid task body; every other kind has to state its `kind`, because `from` + `to` alone cannot tell a `copy` from a `move`.

**Task ids are generated server-side.** You never invent one: POST a body without `id` and the daemon allocates it.

```
POST /api/projects/{projectId}/tasks        body: the task JSON        ->  { ok, project }
```

The new task comes back inside `project.tasks` with an id of the form `t-` + 12 hex digits (`t-9f3ac41b7e02`), drawn from the OS random source. To update an existing task, POST the same body **with** that `id` set and the daemon replaces the record in place; an `id` matching nothing in the project creates a new task. `cgw tasks save <project> --body '<json>' | --file <path> | -` and the WebUI task dialog send the same body. Tasks live in `projects.json`, which is the authority — not the daemon's memory.

## Field reference

| Field | Type | Used by | Meaning |
|---|---|---|---|
| `id` | string | all | Server-generated (`t-` + 12 hex). Send it back to update that task instead of creating one. |
| `kind` | string | all | One of the seven. Empty = `build`. An unknown value is rejected. |
| `name` | string | all | Display name. Falls back to the manifest id, then to the kind. |
| `manifest` | string | `build` | Build type id; `manifest/<id>.json` must exist and parse, or the save fails with `manifestInvalid`. |
| `variables` | object | `build`, and any kind with `inputs` | Name → value map: build variables for `build`, input values for a task that declares `inputs`, step overrides inside a batch. |
| `signingMode` | string | `build` | `signed` or `unsigned`; injected into the manifest as the reserved `{{signingExtraArgs}}`. Anything that is not `signed` is stored as `unsigned`, which is also the default. |
| `artifactPath` | string | `build` | Single artifact path template. Overridden by a non-empty `artifacts` list. |
| `artifacts` | string[] | `build` | Artifact path list. When non-empty it wins over `artifactPath`. |
| `xcodePath` | string | `build` | `.app` path passed per run as `DEVELOPER_DIR`. Empty = use the target's global `xcode-select`. |
| `clean` | bool | `push` | Wipe the remote project directory before unpacking. Default false. |
| `keepCrlf` | bool | `push` | Push bytes as-is, without CRLF → LF normalization. Default false (normalize). |
| `overwrite` | bool | `copy`, `move`, `pull` | Allow replacing an existing destination. Default false. |
| `from` | Location | `copy`, `move` | Source `{ side, path }`. |
| `to` | Location | `copy`, `move`, `pull` | Destination. It is the **full destination path**, so it can rename; it is not a directory to drop things into. |
| `target` | Location | `delete` | What to delete. |
| `sources` | Location[] | `pull`, `export` | At least one source `{ side, path }`. |
| `outputName` | string | `export` | Zip file name. Empty = `<project name>-<epoch seconds>.zip`. |
| `inputs` | object | all | Declared inputs: `"<name>": { type, default?, side?, values? }`. See [Input declarations](#input-declarations). |
| `outputs` | object | all | Declared outputs: `"<name>": { type, side, path, required? }`. See [Output declarations](#output-declarations). |

A **Location** is always `{ "side": "remote" | "host", "path": "<relative path>" }`. A missing `side` means `remote`; any other value is rejected. `remote` is the project root on the target machine (`<baseDir>/<slug>`); `host` is the daemon-side directory for the same slug (`<home>/CompileGateway/<slug>`).

Every `path` must be **relative to its side's project root**. `..`, absolute paths, `~`, drive letters and UNC prefixes are rejected while saving (`taskInvalid`), and the host side adds a symlink check on top.

## build

Runs one build type on the target machine. Required: `manifest` (and it must exist). Optional: `variables`, `signingMode`, `artifactPath`, `artifacts`, `xcodePath`, `inputs`, `outputs`. `kind` may be omitted.

> [!NOTE]
> **Notice**: The `build` task strictly invokes build commands on the target machine and **does not implicitly push source files**. For newly registered projects, run a `push` task first or use the `push-build-pull` workflow preset.

Artifact precedence (highest first): task `artifacts` > task `artifactPath` > manifest `build.artifacts` > manifest `build.artifactPath`. Execution timeout is 3600 seconds.

```json
{
  "kind": "build",
  "manifest": "flutter-ios",
  "variables": { "scheme": "Runner" },
  "signingMode": "unsigned",
  "artifacts": [".cgw/out/Runner.ipa"]
}
```

**Sides:** a build happens entirely on the `remote` side inside the project's remote root, so `artifactPath`, `artifacts` and remote outputs are all remote paths — getting anything back to the host is a separate `pull`.

## push

Copies the host source tree to the remote project directory. Required: nothing — the source directory and the remote location both come from the project registration. Optional: `clean`, `keepCrlf`.

`clean: false` (default) unpacks over the existing tree; `clean: true` deletes the remote directory first, and refuses to treat `~`, `/`, `.` or the base directory itself as the target. With `keepCrlf: false` (default), files starting with `#!` have CRLF normalized to LF so a Windows-checked-out script does not fail with `bad interpreter: /bin/sh^M`; binaries are untouched. Timeout 900 s; the transfer is always a full tar over SSH.

```json
{ "kind": "push", "name": "Push sources", "clean": false, "keepCrlf": false }
```

**Sides:** host → remote; both endpoints come from the project, which is why the task itself needs no paths.

## pull

Copies remote paths back to the host. Required: `sources` (at least one), `to`. Optional: `overwrite`, `inputs`, `outputs`.

```json
{
  "kind": "pull",
  "sources": [{ "side": "remote", "path": ".cgw/out/Runner.ipa" }],
  "to": { "side": "host", "path": "artifacts" },
  "overwrite": true
}
```

**Sides:** remote → host; `sources` are remote paths and `to` is the host destination, each relative to its own project root.

## copy

Copies a file or directory. Required: `from`, `to`. Optional: `overwrite`, `inputs`, `outputs`.

Same-side copying uses the target machine's own `cp -a` (or `std::fs` on the host); a cross-side copy goes through tar over SSH. The save step rejects a `to` that sits inside its own `from` on the same side, because that always fails at run time.

```json
{
  "kind": "copy",
  "from": { "side": "remote", "path": ".cgw/out/App.app" },
  "to": { "side": "remote", "path": ".artifacts/staging/App.app" },
  "overwrite": true
}
```

**Sides:** any combination — host → host, host → remote, remote → host, remote → remote — and the transfer strategy is chosen from the two sides.

## move

Moves a file or directory. Required: `from`, `to`. Optional: `overwrite`, `inputs`, `outputs`. The rules and the self-nesting check are the same as for `copy`.

```json
{
  "kind": "move",
  "from": { "side": "host", "path": "artifacts/Runner.ipa" },
  "to": { "side": "host", "path": "archive/Runner.ipa" },
  "overwrite": true
}
```

**Sides:** any combination, but there is no shared `rename` across machines — a cross-side move degrades to copy followed by deleting the source, so it is not atomic.

## delete

Deletes a file or directory. Required: `target`, which cannot be the project root itself. Optional: `inputs`, `outputs`.

```json
{ "kind": "delete", "target": { "side": "remote", "path": ".artifacts/staging" } }
```

**Sides:** both are meaningful; same-side deletion uses the target machine's `rm -rf` or the host's filesystem calls.

## export

Packs one or more paths into a zip in the host-side `exports/` directory. Required: `sources` (at least one). Optional: `outputName`, `inputs`, `outputs`.

The name defaults to `<project name>-<epoch seconds>.zip`; the archive lands in `<host>/exports/`, can be listed with `cgw exports <project>`, and is downloadable from the WebUI. The zip comes from a small built-in writer: entries get `0644`, file names are UTF-8, and symlinks are skipped (reported numerically, not packed). The `files` / `bytes` counts report what was **packed**, not the size of the zip file.

```json
{ "kind": "export", "sources": [{ "side": "host", "path": "artifacts" }], "outputName": "Runner-release.zip" }
```

**Sides:** `sources` may be on either side, but the product is always written on the host.

## Variables and build variables

A build type declares its own variables in the manifest as an array of `{ name, label, required, default }` (see [Build types (manifests)](Manifests)), and a `build` task supplies values through its `variables` map, keyed by variable name:

```json
{ "kind": "build", "manifest": "flutter-ios", "variables": { "scheme": "Runner", "configuration": "Release" } }
```

- Placeholders written `{{name}}` in the manifest's build steps and artifact paths are replaced with the resolved value. Every ordinary placeholder must be a declared variable, and the values are shell-quoted, so a value containing `;` cannot run a command on the target.
- A value uses the task's entry first, then the declared `default`. If a variable is marked `required` and still ends up empty, the run is refused with `variableRequired` (HTTP 400) before anything is sent to the target.
- `variables` is also the channel for declared inputs (next section) and for per-step overrides in a batch.

**The reserved `signingExtraArgs`** is injected by `signingMode`, not declared — do not add it to `variables` or to a manifest's variable list.

- `"signingMode": "signed"` → it expands to nothing.
- `"signingMode": "unsigned"` → it expands to `CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY=`.
- Anything that is not `signed` (case-insensitive) is stored as `unsigned`; omitting the field does the same.

It expands to **several bare arguments**, so unlike every other placeholder it is not quoted as a single value. That is the whole reason it is reserved. `xcodePath` is not a variable either: the daemon passes it per run as `export DEVELOPER_DIR=...` in front of the steps, so the target's global `xcode-select` is never modified.

## Input declarations

`inputs` is a map of `"<name>": { type, default?, side?, values? }`. A declared input is a promise about what the task accepts, checked before the command is rendered.

| Key | Required | Meaning |
|---|---|---|
| `type` | yes | `string` (the default), `enum`, `path`, `bool`, `number`. Used for validation and UI hints. |
| `default` | no | Value used when neither a variable nor a snapshot supplies one. |
| `side` | no | For `path` inputs: which side the path belongs to (`remote` / `host`). Empty means "do not check the side". |
| `values` | no | Allowed values. An `enum` input with an empty `values` list is rejected at save time. |

Validation per type: `bool` accepts `true` / `false` / `1` / `0`; `number` must parse as a number; a non-empty `values` list means the value must be one of them. Anything else is an empty string, which is allowed.

**How a value is resolved.** On a normal run it is the task's `variables` entry of the same name, falling back to the declared `default`, and the empty string if neither exists. On a **retry**, the input snapshot recorded in the job wins over both, so a retry reproduces the values that actually ran.

**How it is referenced.** Other fields write `{{inputs.<name>}}`. Expansion applies to `artifactPath`, each entry of `artifacts`, `outputName`, the `path` of every Location the kind actually uses (`from` / `to` / `target` / `sources[]`), and each declared output's `path`. It is **single-pass**: a `{{...}}` inside a substituted value stays literal, so one value cannot inject another placeholder. A reference to an undeclared input is rejected at save time and again at run time.

**Expanded paths are re-validated.** After substitution each path goes through the same normalization and boundary check as a hand-written one — no `..`, no absolute path, no `~`, no drive letter, and a value that expands to the project root itself is refused. This happens on every run, so a value cannot smuggle an escape in later. Failures come back as `inputInvalid`.

## Output declarations

`outputs` is a map of `"<name>": { type, side, path, required? }`.

| Key | Required | Meaning |
|---|---|---|
| `type` | no | `file` (the default) or `directory`. An unrecognized value is treated as `file`. |
| `side` | yes | `remote` or `host` — where the output is expected. |
| `path` | yes | Relative path template (may contain `{{inputs.x}}`), relative to that side's project root. `..` is rejected. |
| `required` | no | Default `true`. `false` means a missing output is only noted, not a failure. |

**Verification happens after a successful command.** Once the task's own work reports success, the daemon checks every declared output for existence **and** type (60 s timeout). The host side uses a symlink-aware metadata check plus the boundary check; the remote side runs one SSH command that reports each path as `dir`, `file` or `missing`. A path that exists with the wrong type is a problem, not a pass.

Once any output is declared and a **required** one was not produced, **exit code 0 alone is no longer success**: the step is marked failed with error code `artifactMissing`, its exit code is forced to `-1`, and the message names the missing output. Outputs that did verify replace the artifact paths guessed from the template inside the job record, so the job lists what really exists.

A concrete failing example — a `push` that never produces the ipa it declares:

```json
{ "kind": "push", "outputs": { "ipa": { "type": "file", "side": "remote", "path": ".cgw/out/Runner.ipa" } } }
```

The push itself succeeds, the remote directory has no `.cgw/out/Runner.ipa`, and the step fails with `artifactMissing` naming `remote:.cgw/out/Runner.ipa`. Adding `"required": false` changes the outcome: the step succeeds and the run message simply ends with a note that the optional output was not produced.

## Referencing another step's output

Inside a batch, a later step can consume a path an earlier step **verified**, by writing `{{steps.<step id>.outputs.<name>}}` into a later step's `variables` value. The producing step must come earlier, carry a step `id`, and declare that output.

```json
{ "kind": "build", "id": "t-build", "manifest": "flutter-ios",
  "outputs": { "app": { "type": "directory", "side": "remote", "path": ".cgw/out/Runner.app" } } }
```

```json
{ "kind": "pull", "id": "t-pull",
  "inputs": { "appPath": { "type": "path", "side": "remote" } },
  "sources": [{ "side": "remote", "path": "{{inputs.appPath}}" }],
  "to": { "side": "host", "path": "artifacts" } }
```

```json
{ "id": "b-release", "name": "release",
  "steps": [
    { "taskId": "t-build", "id": "package" },
    { "taskId": "t-pull", "variables": { "appPath": "{{steps.package.outputs.app}}" } }
  ] }
```

Only outputs that actually passed verification are published, so a consumer can never read a path that was merely declared. If a reference cannot be resolved when the step starts, the step fails immediately with error code `stepRefMissing` — rather than handing a literal `{{steps...}}` to the task, which would fail far from the cause — and the remaining steps are skipped.

**Side-consistency rule.** If the consuming step's task declares an input for the value it receives and that input declares a `side`, the side must equal the producing output's `side`. A `host` input fed by a `remote` output is refused at save time with `batchInvalid`, because the path would look equally valid on the wrong machine. Leaving the input's `side` empty skips the check. Batch structure, ordering and retry are covered in [Batches and workflows](Batches-and-Workflows).

## Retry and the recorded input snapshot

Every job records the **inputs it used**, keyed per task as `<taskId>.<input name>`.

Retry creates a **new job** and starts from the failed step (or from a chosen step with `--from N`, or from the beginning with `--from-start`); steps that already succeeded are not re-run, and step numbers stay aligned with the original batch.

Retry resolves inputs from the recorded snapshot rather than from the task's current values, so editing a task variable and retrying an old job re-runs **the version that was recorded**, keeping logs and results auditable. `cgw jobs retry <jobId>` is the CLI entry point.

## Inspecting without running

| Command | What you get |
|---|---|
| `cgw tasks <project>` | The task list: id, kind, name, and the fields that matter for that kind. |
| `cgw task <project> <taskId>` | One task's stored JSON (the templates, not the expansion). |
| `cgw tasks preview <project> <taskId>` | The **expanded** copy, rendered without running anything. |
| `cgw schema task` | The JSON Schema for the task body. |

`cgw tasks preview` resolves inputs and outputs first and then renders the running copy: a `build` returns `kind`, `command`, `artifactPath`, `artifacts` and `xcodePath`, while the file kinds return their resolved `from` / `to` / `target` / `sources` / `outputName` and flags. The response also carries the resolved inputs and declared outputs, which is what tells you which values the run will use and what will be verified afterwards. Missing required variables come back as `variableRequired` and a contract problem as `inputInvalid` — both without touching the target machine.

The HTTP endpoint is `POST /api/projects/{id}/tasks/{taskId}/preview`. For the schema use `GET /api/schema/task` or the MCP resource `cgw://schema/task`: it is generated from the Rust structs (so it cannot drift from the parser), it is draft-07, and its seven `oneOf` branches state exactly which fields each kind requires.

## Common gotchas

- **Declaring an output the step cannot produce.** The classic `artifactMissing`: the command exits 0, but the declared artifact is not there, so the step fails. Declare only what the step really creates, or mark it `required: false`.
- **`..` in a path.** Rejected at save time, and rejected again **after** `{{inputs.x}}` expands — an input value cannot smuggle one in.
- **Forgetting `overwrite`.** `copy`, `move` and `pull` refuse to replace an existing destination unless `overwrite` is true; the run fails instead of overwriting.
- **Treating `to` as a directory.** It is the full destination path, so `{"to": {"path": "dst"}}` puts the item at exactly `dst` — that is what lets a copy rename.
- **Expecting `build` to push.** It never does; an untouched project fails on an empty remote directory.
- **CRLF scripts.** Leave `keepCrlf` false unless you want the bytes verbatim, or a Windows-checked-out script can land with `bad interpreter: /bin/sh^M`.
- **Referencing a step with no `id`**, or assuming a declared output exists: only verified outputs are published, and a step whose outputs a later step consumes must carry an `id`.

← [Home](Home)
