# Batches and workflow presets

A batch is an ordered list of steps on one project, and a workflow preset is a cross-project
template that instantiates such a batch (plus its tasks) into a project.

> [!NOTE] Concept Boundaries: Task vs Batch vs Job
> - **Task (Definition)**: Static configuration defining a single atomic action (push source, run build, pull artifacts, export archive).
> - **Batch (Pipeline)**: Static configuration orchestrating an ordered sequence of tasks with variable overrides and step outputs.
> - **Job (Runtime Instance)**: The dynamic execution instance created when a Task or Batch is triggered (assigned a unique `jobId`). Logs, step durations, and output snapshots attach to the Job. Modifying a Task or Batch never alters historical Jobs.

## What a batch is

A batch belongs to exactly one project and holds an ordered list of steps that run top to
bottom. Each step is a small object:

```json
{ "taskId": "t-build", "id": "build", "variables": { "scheme": "Runner" } }
```

- `taskId` (required) — the id of a task in the **same** project. An empty `taskId` is
  rejected while saving; it is no longer dropped silently.
- `id` (optional) — the step's name, used only so later steps can reference this step's outputs.
- `variables` (optional) — overrides that apply to this step alone.

A whole batch stored in a project looks like this:

```json
{
  "id": "b-8f2c1a9d0e44", "name": "first build",
  "steps": [
    { "taskId": "t-push-source", "id": "push" },
    { "taskId": "t-build-app", "id": "build" },
    { "taskId": "t-pull-app", "id": "pull",
      "variables": { "source": "{{steps.build.outputs.app}}" } }
  ]
}
```

Batch id and name are yours to choose; created through the API without an `id`, the daemon
generates an opaque `b-<hex>` id. A batch name also works where a batch id is expected, so
hand-written batches stay readable in the CLI. Manage them with:

```bash
cgw batches p-ios                                  # list
cgw batch save p-ios "first build" t-push-source t-build-app t-pull-app
cgw batch save p-ios --file batch.json             # when steps need variables or ids
cgw batch run p-ios "first build" --wait
cgw batch rm p-ios b-8f2c1a9d0e44
```

The HTTP side is `GET|POST /api/projects/{id}/batches`,
`PATCH|DELETE /api/projects/{id}/batches/{batchId}`, and
`POST /api/projects/{id}/batches/{batchId}/run` or `.../enqueue`. A batch with no steps is
refused (`steps` needs at least one entry).

## Step ids and outputs

A step `id` is optional, but it is required when a later step wants to consume this step's
outputs: the daemon stores each verified output under the producing step's id, and a consumer
writes `{{steps.<step id>.outputs.<output name>}}` in a `variables` value.

```json
{ "taskId": "t-pull-app", "id": "pull",
  "variables": { "source": "{{steps.build.outputs.app}}" } }
```

The rules are validated when the batch is saved, not at run time:

- step ids must be unique within the batch (an empty id is fine, but exposes nothing);
- the referenced step must exist and must come **earlier** — a step cannot reference itself or
  a later step;
- the producer must declare that output (either in the task's `outputs` contract or via step-level `outputs` in the build manifest);
- if the consuming input declares a `side`, it must match the producer's output side.

The reference resolves to the real path the earlier step verified, so a missing required output
fails the producing step instead of feeding a literal `{{steps...}}` string to the next one
(see [Tasks](Tasks) for the `inputs` / `outputs` contract).

## Step-level variables

A step's `variables` are merged over the task's own `variables` for that step only. The task in
the project JSON is not modified, and the next step starts from the task's values again. This
is the channel for both "run this build task with a different scheme in this batch" and "pass
the previous step's artifact into this step". Step overrides are recorded in the job's input
snapshot, so a retry replays the values that really ran.

## Failure, skip and retry

A batch is fail-fast. The first failing step stops the run; every later step is recorded as
`skipped`, and the job's message names the step that failed (for example
`第 2 步失败 - ...`). Batch-level totals (`files`, `bytes`) are the sums over the steps that
actually ran, and cancelling a batch marks the remaining steps `skipped` and the job
`cancelled`. A failing batch also records `retryFromStep`, the index of the first unsuccessful
step (`null` when nothing failed):

```bash
cgw jobs retry j-3c1f0a7b2e                # resume from the first unfinished step
cgw jobs retry j-3c1f0a7b2e --from 1       # explicit fromStep index (zero-based)
cgw jobs retry j-3c1f0a7b2e --from-start   # start over from step 1
cgw jobs retry j-3c1f0a7b2e --wait         # poll to the end and mirror the result in the exit code
```

HTTP equivalent: `POST /api/jobs/{id}/retry` with `{ "fromStep": 1 }` or
`{ "fromStart": true }`, answering `{ ok, jobId, retriedFrom }`.

A retry is a **new job**. It registers only the remaining steps, but the step numbering stays
aligned with the original batch, so the two records can be read side by side. It also reuses the
input snapshot recorded in the original job rather than re-reading the task's current variables,
so editing a task and retrying cannot silently swap the inputs of an already diagnosed run.

## The project lock and the queue

One job per project runs at a time, and a batch holds that project-level lock for its whole run
— from the first step to the last. A second request while the lock is held fails with HTTP 409
`projectBusy`, including the holder's `jobId` (CLI exit code `4`). To wait instead of failing,
use the queued form:

```bash
cgw batch run p-ios "first build" --enqueue
cgw run p-ios t-push-source --enqueue
```

The HTTP queueing form is `POST /api/projects/{id}/tasks/{taskId}/enqueue` and
`POST /api/projects/{id}/batches/{batchId}/enqueue`. MCP exposes the same thing as
`enqueue: true` on `cgw_run_task` / `cgw_run_batch`. Note that `cgw workflows apply ... --run`
is not an enqueue path: `run: true` uses the immediate run, so a busy project still answers
`projectBusy`.

A queued batch:

- starts in the `pending` state and joins a per-project FIFO queue (`queuePosition` is reported
  when it is created);
- is promoted automatically when the project frees up — no one has to re-submit it — and can be
  cancelled while it is still pending;
- **resumes automatically after a daemon restart**. The execution plan is persisted to
  `queued_runs.json`, and on restart the daemon restores any pending jobs and re-pumps the queue
  (orphaned pending jobs without a plan are gracefully marked cancelled).

The optional host exclusion feature (default off) can be configured globally (`hostLock` in
`config.json`) or per client machine (`hostLock` on each client in `clients.json`). When active,
projects targeting the same physical client machine run strictly serially; immediate requests answer
409 `hostBusy` with the `clientId`, while queued runs wait. See [Configuration-and-Logs](Configuration-and-Logs)
and [Clients-and-Environment](Clients-and-Environment).

## Cross-project workflow presets

Tasks and batches are reuse **inside** one project. The problem presets solve is the next
project: you built a pipeline once and want the same one for a second project without rebuilding
every task by hand. Presets are stored as plain files next to `config.json`:

```
<config.json dir>/workflows/<id>.json
```

They are seeded **once**: the first run writes the two built-in presets and afterwards never
touches the directory again. `cgw workflows` prints the directory it is reading, and because the
directory is read on every call, editing a preset on disk needs no reload step.

- **`push-build-pull`** — "first build: push → build → pull". Recommended initial run for fresh projects to resolve the build-before-push dependency.
- **`push-build-export`** — "push → build → export zip". End-to-end flow: pushes source, runs remote build, and packs the artifact into a host-side zip file.
- **`build-export-zip`** — "build → export zip". For projects already synced that just need a build and a downloaded zip package.
- **`clean-push-build-pull`** — "clean push → build → pull". Wipes remote directory before full push, clean build and artifact retrieval.

## Preset file format

```json
{
  "name": "My pipeline",
  "description": "push, build and pull the app",
  "variables": [
    { "name": "manifest", "label": "Build type", "required": true, "default": "xcode" },
    { "name": "scheme", "label": "Xcode scheme", "required": false, "default": "App" }
  ],
  "tasks": [
    { "id": "t-push", "kind": "push", "name": "Push source" },
    { "id": "t-build", "kind": "build", "name": "Build {{scheme}}",
      "manifest": "{{manifest}}", "variables": { "scheme": "{{scheme}}" },
      "artifactPath": "build/{{scheme}}.app",
      "outputs": { "app": { "type": "directory", "side": "remote", "path": "build/{{scheme}}.app" } } },
    { "id": "t-pull", "kind": "pull", "name": "Pull artifact",
      "sources": [ { "side": "remote", "path": "{{inputs.source}}" } ],
      "to": { "side": "host", "path": "artifacts" },
      "inputs": { "source": { "type": "path", "side": "remote" } } }
  ],
  "steps": [
    { "id": "push", "taskId": "t-push" },
    { "id": "build", "taskId": "t-build" },
    { "id": "pull", "taskId": "t-pull", "variables": { "source": "{{steps.build.outputs.app}}" } }
  ]
}
```

- `name` (required, non-empty) — the display name; it becomes the batch name unless you pass
  one. `description` is free text.
- `variables` — the parameters: `name`, `label`, `required`, `default`. Names must be unique.
  A required parameter with no value and no default makes apply fail.
- `tasks` — inline task definitions in the same shape the task API accepts (see
  [Tasks](Tasks)). Every task needs a non-empty, unique `id`; the author's ids are kept, which
  is what makes collisions visible.
- `steps` — batch steps as described above (at least one). Presets and project batches share
  **one** validator, so a preset that validates is a batch that the project will accept.

Substitution covers every JSON string in `tasks` and `steps`, and it replaces **declared**
variables only, in a single pass. `{{configuration}}` or `{{inputs.source}}` — names the preset
did not declare — are left untouched for the manifest-variable and task-`inputs` layers below,
so the two layers never swallow each other. A value is not expanded twice, so a parameter whose
value contains `{{...}}` stays literal. Passing a parameter the preset never declared is an
error (`workflowInvalid`), not a silent no-op, so a typo cannot look like a successful change.

## Applying a preset

```bash
cgw workflows                                              # list presets and their validity
cgw workflows show push-build-pull                         # raw file + parse result
cgw workflows apply p-ios push-build-pull --set scheme=Runner   # preview only
cgw workflows apply p-ios push-build-pull --set scheme=Runner --apply
cgw workflows apply p-ios push-build-pull --apply --overwrite --run
```

Apply is preview-first. Without `--apply` the API writes nothing and answers
`{ ok, applied: false, preview, conflicts }`, where the preview carries the deterministic batch
id, the batch name, the tasks that would be added and the steps:

```json
{ "ok": true, "applied": false, "conflicts": [],
  "preview": { "presetId": "push-build-pull", "batchId": "wf-push-build-pull",
    "batchName": "首次构建:推送 → 构建 → 回传",
    "tasks": [ { "id": "t-push-source", "kind": "push", "name": "推送源码" } ],
    "steps": [ { "id": "push", "taskId": "t-push-source" } ], "conflicts": [] } }
```

The batch id is `wf-<preset id>` and is deterministic, so applying the same preset twice lands
on the same batch instead of piling up new ones. Conflicts are a same-named task id or the same
batch id already present in the project. When `--apply` is given and conflicts exist without
`--overwrite`, the request fails with HTTP 409 `workflowConflict` and returns the conflict list
plus the preview; nothing is written. With `--overwrite`, a same-named task or batch is replaced
**in place**, keeping its position in the list.

`--run` executes the batch immediately after writing and returns a `jobId`. Otherwise the result
is just data: an ordinary set of tasks plus one ordinary batch, which appears in the project and
can be run by any front end:

```bash
cgw batch run p-ios wf-push-build-pull
```

HTTP equivalent: `POST /api/projects/{id}/workflows/{preset}/apply` with
`{ "variables": {...}, "apply": true, "overwrite": true, "run": false, "name": "" }`.

## Authoring a preset

Presets are user files, like build-type manifests. Write them with the CLI, the API, or your
editor. `save` validates before writing; an invalid file is refused with 400 `workflowInvalid`:

```bash
cgw workflows save my-flow --file my-flow.json
cgw workflows save my-flow --body '<json>'
cgw workflows rm my-flow
cgw workflows refresh
```

- `save` maps to `PUT /api/workflows/{id}` (body `{ "raw": "<preset JSON>" }`); the raw form is
  `GET /api/workflows/{id}/raw`.
- `rm` deletes the preset file. Projects that already applied it are untouched.
- `refresh` restores **missing** built-ins only; existing files — including ones you rewrote —
  are never touched. It maps to `POST /api/workflows/reset`, while
  `GET /api/workflows/templates` shows which built-ins are still missing.

Get the schema before writing a file:

```bash
cgw schema workflow          # JSON Schema (draft-07)
```

The same schema is available as `GET /api/schema/workflow` and as the MCP resource
`cgw://schema/workflow`. The task definition embedded in it is the very same one as
`cgw schema task`, so "a task in a preset" and "a task the API accepts" cannot drift apart;
[CLI](CLI), [HTTP-API](HTTP-API) and [MCP](MCP) list the other schema names.

## Authority and lifecycle

The project JSON is authoritative. Applying is one-way, preset → project: there is no reverse
sync, and editing or deleting a preset never changes a project that already applied it. Once
applied, the tasks and the batch are the project's own data, editable without being pushed back
into the preset. Because the preset's task ids are kept and the batch id is deterministic,
re-applying an updated preset is an explicit operation: you see the conflicts in the preview and
decide with `--overwrite`. A preset is a starting point, not a live link.

← [Home](Home)
