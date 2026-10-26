# Concepts

CompileGateway pushes a local source tree to an SSH target machine, runs the build there, and brings the artifacts back or packages them for export.

## What the tool does

The source stays on the development machine; the compiler runs on the target machine (usually a macOS build box, and equally a Linux host). A single daemon orchestrates the whole chain:

- **Push** — pack a local directory and stream it over SSH into the project's remote directory.
- **Build** — run one of your build-type manifests on the target, streaming the output back line by line.
- **Pull / export** — copy the produced artifacts back to the host, or pack them into a zip in the host-side `exports/` directory.

The daemon never compiles anything itself. It speaks SSH to the target, keeps projects, tasks, batches, job history and logs, and answers on HTTP. It runs on Windows, Linux and macOS.

## One core, four front ends

Everything the tool does is implemented once, in the daemon, and exposed through four front ends that all talk to the same HTTP API:

| Front end | What it is |
|---|---|
| WebUI | Browser UI served by the daemon (embedded into the binary at compile time) |
| `cgw` (CLI) | Thin HTTP client, never interactive, script and agent friendly; bare `cgw` opens an interactive shell |
| MCP | Built-in MCP server on a **second port** (Streamable HTTP, stateless) |
| HTTP API | The interface the other three use; usable directly |

**Parity principle: whatever the WebUI can do, the CLI can do.** The reverse is intentionally scoped — machine-oriented capabilities (such as raw JSON Schema retrieval) are focused on the CLI, MCP and HTTP interfaces to keep the WebUI clean and focused:

- **JSON Schema access** — `cgw schema [name]` and `GET /api/schema/{name}` for `manifest`, `task`, `batch`, `client`, `project` and `workflow`.
- **Workflow preset apply** — `cgw workflows apply <project> <preset>` and the workflow apply endpoint. Applying instantiates standard tasks and an ordered batch in the target project.

## Object model at a glance

```
Client (remote SSH target build box)
  └─ Project (local source directory + remote mirrored path)
       ├─ Task (declared static action: build / push / pull / copy / move / delete / export)
       ├─ Batch (ordered execution pipeline of tasks within this project)
       └─ Workflow preset (cross-project reusable template, instantiates into Task + Batch)
            └─ Job (runtime execution instance: includes step results, snapshots and logs)
                 └─ Log
```

## Client

A client is one managed remote SSH target build box.

- Fields: `id`, `name`, `host`, `port` (default `22`), `auth` (`method: password | key`, `username`, `password`, `keyPath`, `passphrase`), `tags`, `baseDir` (default `~/CompileGateway`), `baseDirResolved`, `hostKey`, `hostKeyLine`.
- Secrets are **write-only**: once saved, the API never returns passwords or passphrases, returning only configuration status flags to prevent credential leakage.
- Host keys use TOFU (Trust On First Use) — the first connection records the fingerprint, and subsequent mismatches require explicit user trust.
- The base directory uses **real-time verification**: saving connects over SSH, runs `mkdir -p`, and verifies write permissions before persisting the resolved absolute path.

## Project

A project couples a source directory on the daemon's machine with a remote directory on one client.

- Remote work happens under `<baseDir>/<slug>`; the host side mirrors it under the host directory for that slug.
- A project owns its `tasks` and `batches`, its `excludes` (by default `.git`, `.DS_Store`, `Thumbs.db`), and its last sync/artifact summary.
- The project registry is the authority: tasks and batches live in `projects.json`, not in the daemon's memory.

## Task

A task is one action attached to a project.

- Fields: `id`, `name`, `kind`, `variables`, `signingMode`, `artifactPath`, `artifacts`, `xcodePath`, `clean`, `keepCrlf`, `overwrite`, `inputs`, `outputs`.
- A non-empty `artifacts` list overrides `artifactPath`.
- Run it with `cgw run <project> <taskId>`, which returns a job immediately; `--wait` follows it to the end.
- `cgw tasks preview <project> <taskId>` renders the command and the expanded paths without running anything.

## Batch

A batch is an ordered list of steps, each `{ taskId, id?, variables? }`.

- A step `id` is optional, but required if a later step references that step's outputs.
- It is fail-fast: any failing step stops the batch and the later steps are recorded as skipped.
- Outputs flow between steps as `{{steps.<step id>.outputs.<name>}}`, usable in a later step's `variables`; the producing step must come earlier and declare that output.
- Fail-fast stops the run, so the totals (`files`, `bytes`) are sums over the steps that ran.

## Workflow preset

A workflow preset is a cross-project template: `name`, `description`, `variables`, inline `tasks` and `steps`.

- Two built-ins ship with the binary and are seeded once (`push-build-pull`, `build-export-zip`); `cgw workflows refresh` only restores missing ones.
- `{{name}}` substitution replaces declared variables only; passing an undeclared parameter is rejected.
- Apply is preview-first: without `apply` you get the task list, the deterministic batch id (`wf-<preset id>`) and the conflict list. Conflicts are an error (`workflowConflict`) unless `overwrite` is given.
- Applying keeps the preset's task ids, and is one-way: editing a preset does not change projects that already applied it. Afterwards it is an ordinary task set plus one batch.

## Job

A job is one execution — of a task, a batch, a push or a pull.

- It records its kind, title, source (`webui` / `cli` / `mcp` / `daemon`), per-step records, artifacts, file and byte counts, and the input snapshot it used.
- Each step carries its state, message, inputs and outputs (as `host:` / `remote:` strings) and an error code.
- History keeps the last 50 jobs.

## Log

A log is the running record of what happened, per line.

- Every line carries a **source** (`webui` / `cli` / `mcp` / `daemon`) and a **scope** (global, client, project, job), so you can filter. MCP-driven work is marked `mcp`, so one glance shows what an AI did.
- Files roll daily as `logs/YYYY-MM-DD.log`; retention is configured by `logRetainDays`.
- Read them live in the WebUI Logs tab, or with `cgw logs client <id>` (live), `cgw logs` (days), `cgw logs <YYYY-MM-DD>` (history).

## The seven task kinds

| kind | Required fields | What it does |
|---|---|---|
| `build` | `manifest` | Runs a build-type manifest on the target. It does **not** push first. |
| `push` | — | Host → remote source sync; normalizes CRLF to LF for shebang scripts unless `keepCrlf`. |
| `pull` | `sources`, `to` | Remote → host copy of the artifacts. |
| `copy` | `from`, `to` | Copies files or directories; either side combination. |
| `move` | `from`, `to` | Moves them; a cross-side move degrades to copy-then-delete. |
| `delete` | `target` | Deletes a file or directory. |
| `export` | `sources` | Packs a zip into the host-side `exports/`; `outputName` names it. |

## Job lifecycle

```
pending ──▶ running ──▶ ok | failed | cancelled
```

- `pending` means queued and not started yet. `running` is executing. The three terminal states are `ok`, `failed` and `cancelled`; cancel is cooperative.
- A job has **steps**. A single task is one step; a batch is one step per task, numbered by position in the batch.
- A job lists the **artifacts** it produced, plus the file and byte counts of the run.
- **Retry resumes from a chosen step** (or the failed one by default), so the steps that already succeeded are not re-executed. Retry creates a new job and only registers the remaining steps, while keeping step numbers aligned with the original batch.
- **A job records the inputs it used**, and retry works from that input snapshot — not from the task's current values. Changing a task after the fact does not change what a retry of an old job does.

## Concurrency: projects, queues and hosts

- **One job per project at a time.** A second request is refused with 409 `projectBusy`, carrying the holder's `jobId`.
- **Add `enqueue: true` (MCP) / `--enqueue` (CLI)** and the request joins that project's FIFO queue instead of failing. The queue head is promoted when the project frees up, and a queued job can still be cancelled.
- **Queued jobs are never resumed across a daemon restart.** They are marked `cancelled` with a message telling you to submit the work again. A job that was still `running` when the daemon stopped is likewise not resumed; it is recorded as failed and marked as interrupted.
- **`hostLock`** (config, default off) is host-level exclusion: when it is on, only one job at a time runs across all projects that share the same client. A blocked immediate request returns 409 `hostBusy` with the `clientId`; queued work simply waits.
- Enabling or disabling `hostLock` takes effect immediately.

## The input/output contract

Values come from the task's `variables`, falling back to each declared `default`. `inputs` is a map of `"<name>": { type, default?, side?, values? }`, where `type` is `string` (the default), `enum`, `path`, `bool` or `number`. A declared input is a promise about what this task accepts, and it is validated before the command is rendered.

Other fields reference an input as `{{inputs.<name>}}`. Paths are re-validated **after** expansion, so `..` and escapes are rejected no matter how they were assembled. `cgw tasks preview` renders the same expansion without running anything, which is the cheap way to see what a set of values will produce.

Declared `outputs` are `{ type: file | directory, side, path, required? }`. After the command exits, the daemon verifies existence and type on the declared side. A missing **required** output fails the step with error code `artifactMissing` — exit code 0 alone is no longer success. Setting `required: false` records a note instead of failing, which is what you want for optional artifacts.

## Where things live and who owns them

| Path | Owner | Contents |
|---|---|---|
| `projects.json` | daemon (runtime state) | The **source of truth** for projects, their tasks and their batches. |
| `jobs.json` | daemon (runtime state) | Job history, the last 50 entries, restored on restart. |
| `manifest/` | **you** | Build types, one `<id>.json` per type. Seeded from the built-in templates exactly once. `cgw schema manifest` describes the format. |
| `workflows/` | **you** | Cross-project workflow presets, `<id>.json`. Same seeding rule: built-ins are copied in once, `cgw workflows refresh` only restores missing ones. |
| `config.json` | generated on first run | Daemon settings: port, language, listen mode, auth, MCP, `allowedRoots`, `hostLock`, `logRetainDays`. |
| `runtime.json` | daemon | The ports actually bound (`url`, `mcpUrl`, `mcpPort`, …), so thin clients can find a daemon whose port was auto-picked. |
| `logs/` | daemon | Daily log files `YYYY-MM-DD.log`, pruned according to `logRetainDays`. |
| `exports/` | daemon | Host-side directory holding exported zip archives. |

The two directories marked **you** are the ones to treat as hand-authored content: the daemon seeds them once and never overwrites what you changed.

## Architectural Boundaries and Scope

- **Regarding literal native OpenWrt router plugins (e.g. luci-app-cgw)**:
  **Not planned at this stage**. CompileGateway is designed as an orchestration gateway for distributing and compiling source code. Typical router devices have constrained CPU, RAM, and Flash resources, making them ill-suited for heavy compilation workloads (building modern firmware often requires 16GB+ RAM and tens of gigabytes of disk space). The CompileGateway daemon and sources should run on your primary workstation or a dedicated Linux/NAS build host; routers are best suited as target deployment devices. Should lightweight router telemetry or direct artifact pushing workflows arise in the future, a dedicated lightweight integration may be evaluated then.

## What to read next

- [Getting started](Getting-Started) — install, first run, first build.
- [Clients and environment](Clients-and-Environment) — SSH targets, host keys, base directories, environment detection.
- [Tasks](Tasks) — every task kind and every field, with examples.
- [Batches and workflows](Batches-and-Workflows) — ordering, step variables, retry, presets.
- [Manifests](Manifests) — build types, variables, artifact lists, detection.
- [WebUI](WebUI) — what the browser interface shows.
- [CLI](CLI) — the full command surface and exit codes.
- [MCP](MCP) — the tool set an AI agent sees.
- [HTTP API](HTTP-API) — endpoints, error codes, the async pattern.
- [Configuration and logs](Configuration-and-Logs) — `config.json`, ports, retention, log reading.
- [Troubleshooting](Troubleshooting) — the things that commonly surprise people.

← [Home](Home)
