# CLI (`cgw`)

`cgw` is the command-line front end of CompileGateway: a thin HTTP client for the daemon that
holds no build logic of its own.

## What it is

Each invocation becomes one HTTP request, and the JSON that comes back is rendered for the
terminal. Nothing is compiled, packed or transferred inside the client; if the daemon is not
running, `cgw` says so and exits 2.

- **Binary:** `compilegateway-cli` (`compilegateway-cli.exe` on Windows), at
  `src-tauri/target/release/compilegateway-cli[.exe]`. `cgw` is the name used throughout the
  documentation and in the program's own help.
- **Build it:** `cargo build --release --bin compilegateway-cli`, or headless (no GUI libraries) as
  `cargo build --release --no-default-features --bin compilegateway-daemon --bin compilegateway-cli`.

Put it on PATH and read the environment with `cgw setup` / `cgw setup --apply` (Windows: adds the
directory to the user PATH, no admin needed). On Linux and macOS `cgw setup` deliberately only
**prints** the `export PATH=...` line instead of editing your shell rc files.

A bare `cgw` opens the interactive shell — the terminal form of the desktop window: scrolling log
above, `cgw> ` prompt below, one command per line. If stdin is not a terminal (a pipe, a CI job) it
prints the help tree instead of hanging. `cgw -shell` (also `--shell` / `shell`) spells the same
thing out: it reuses a running daemon, starts the sibling `compilegateway-daemon` if there is none,
and adds the shell's own `help`, `clear`, `logs`, `webui` and `exit` on top of every normal command.

## Finding the daemon

`cgw` resolves the daemon address on every run; first hit wins.

| Order | Source | Notes |
|---|---|---|
| 1 | `--url http://host:port` | per invocation, beats everything |
| 2 | `$CGW_URL` | environment variable |
| 3 | `runtime.json` in `CGW_HOME` | the per-user shared directory; the daemon writes a pointer there |
| 4 | `runtime.json` next to the `cgw` executable | the usual layout for an unpacked build |
| 5 | `runtime.json` in the current working directory | lets you run `cgw` inside a checkout |
| 6 | `port` in `config.json` | last resort — works only for a fixed port, never for `port: 0` |

`runtime.json` is authoritative because it records the port the daemon **actually** bound, which is
unknowable when `config.json` says `port: 0` (pick a free port). `CGW_HOME` defaults to
`~/.compilegateway` and holds the pointer file and `cgw-session.json`.

```bash
cgw doctor                                # daemon up? login needed? counts?
cgw setup                                 # which source the address came from, in words
cgw --url http://127.0.0.1:65251 status   # override the whole chain
```

`cgw doctor` is the command to read when something is off: it runs each check independently, prints
`OK` / `FAIL` per line plus any daemon warning, and exits 0 when everything passes, 3 when any
check fails — including "daemon not reachable", because doctor is a *report*, not a transport call.

## Global flags

| Flag | Meaning |
|---|---|
| `--url <url>` | daemon base URL; overrides discovery |
| `--json` | machine-readable output; also turns errors into structured JSON on stdout |
| `--wait` | follow the job just started until it finishes, then exit with its outcome |
| `--timeout <seconds>` | budget for `--wait`; default 3600 |
| `--help`, `-h`, `help`, `?` | usage, drill-down included (`cgw help tasks save`, `cgw tasks save -h`) |
| `--version`, `-V` | print the version |

## Authentication

- **Loopback is free.** Requests from `127.0.0.1` / `::1` need no login even when credentials are
  configured — which is why a local `cgw` just works.
- **A `wan` daemon with credentials:** pass the address and the credentials together, and they are
  used once, on the first `401`, without being written to disk:

  ```bash
  CGW_USERNAME=alice CGW_PASSWORD=secret cgw --url http://build-box:65251 status
  ```

- **`cgw login` / `cgw logout`** trade credentials for a session cookie stored per host in
  `cgw-session.json` under `CGW_HOME`, and delete it again.

  ```bash
  cgw login --username alice --password secret
  cgw logout
  ```

- HTTP Basic is the authentication mechanism of the MCP endpoint on its own port; the CLI and the
  daemon's HTTP API use the session cookie described above.
- Nothing about authentication is interactive: a missing credential is an error (exit 7), not a
  prompt.

## Exit codes

Exit codes are the contract. Branch on them; never grep the prose.

| Code | Name | Meaning |
|---|---|---|
| 0 | success | command completed (for `--wait`, the awaited job ended `ok`) |
| 1 | usage | unknown command, missing argument, malformed JSON, bad value |
| 2 | transport | cannot reach the daemon (refused, timed out, no address discovered) |
| 3 | api | daemon returned an error — other 4xx/5xx statuses, and any failed `doctor` check |
| 4 | project busy | HTTP 409 `projectBusy`: another job is already running for this project |
| 5 | job failed | the job `--wait` followed ended `failed` or `cancelled`, or `--timeout` expired first |
| 6 | not found | HTTP 404: wrong or deleted project / task / batch / job / manifest id |
| 7 | login required | HTTP 401, plus the one 403 that really means "go log in" (`authLocalOnly`) |

Other 403s — `outsideRoot`, for example — are the caller's mistake and map to 3, not 7.

## The script and agent contract

- **It never prompts.** No code path blocks on stdin for an answer; destructive confirmations become
  explicit flags (`--force`, `--overwrite`, `--clean`).
- **`--json` covers failures too.** On success the command's own JSON goes to stdout. On failure
  stdout carries exactly one object:

  ```json
  {"ok":false,"exitCode":4,"error":{"code":"projectBusy","httpStatus":409,
   "message":"project is busy","retryAfterSeconds":5,"jobId":"j-1a2b","projectId":"p-app"}}
  ```

  `error` is the daemon's error object, with `code`, `httpStatus`, and — when they apply —
  `retryAfterSeconds`, `jobId`, `projectId` and `details`.
- **JSON in, from anywhere:** `--body '<json>'`, `--file <path>`, or `--file -` for stdin, so a
  pipeline never has to fight shell quoting.

  ```bash
  cgw tasks save app --file - < task.json
  cat task.json | cgw tasks save app --file -
  ```

- **`--wait` polls.** Without it, an execution command returns immediately with `jobId=...`. With
  it, `cgw` polls `GET /api/jobs/{id}` roughly every 800 ms, writes progress to **stderr** and the
  final result to **stdout**, so a pipeline captures only the outcome. It gives up after `--timeout`
  seconds and exits 5.
- **Exit codes are stable on purpose.** They are part of the interface, published by
  `cgw help --json` and served to agents as the MCP `cgw://cli` resource, so "is the project busy?"
  is `[ $? -eq 4 ]` and not a regex over a log line.

## Command groups

This build (v0.1.0) reports **80 commands** in `cgw help --json`, 71 of them with an explicit WebUI
counterpart — the machine-checkable half of the rule that the CLI can do whatever the WebUI can.

### status, doctor, setup, login

`status` · `doctor` · `setup [--apply]` · `login [--username U] [--password P]` · `logout`

```bash
cgw doctor --json
cgw setup
```

### clients, basedir, env

`clients [list]` · `clients add --host <ip> [--user U --password P | --key <private key>]
[--name N] [--port P] [--tags a,b] [--base-dir <path>]` · `clients edit <id> [...]` ·
`clients test <id>` · `clients hostkey <id> [--trust <SHA256:...> | --forget | --import <file|-> [--force] | --export]` ·
`clients rm <id>` · `basedir <id> <path> [--no-verify]` · `env <id> [--cached]` · `env --script`

`clients test` really connects, and on a host-key change it returns the `presentedKey` and the
`mismatch` instead of silently trusting the new key. `basedir` verifies by connecting unless
`--no-verify` is given; `env` is the (slow) environment probe.

```bash
cgw clients add --name mac --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519
cgw clients test mac && cgw env mac
ssh-keyscan -p 2222 build-box | cgw clients hostkey c-mac --import -
```

### browse, projects, tree, cat

`browse [<absolute path>]` · `projects [list]` · `projects add --client <id> --path <absolute path> [--name N]` ·
`projects rm <id>` · `tree <project> [path] [--side remote|host]` · `cat <project> <path> [--side remote|host]`

`browse` lists subdirectories of the **daemon's** machine, inside `allowedRoots`. `cat` is a
controlled read: binaries report metadata only.

```bash
cgw browse ~/src
cgw projects add --client c-mac --path ~/src/app --name app
cgw tree app Sources --side remote
cgw cat app README.md
```

### tasks, run

`tasks [list] <project>` · `tasks save <project> --body '<json>' | --file <path> | -` ·
`tasks rm <project> <taskId>` · `tasks preview <project> <taskId>` ·
`run <project> <taskId> [--wait] [--timeout N] [--enqueue]`

```bash
cgw tasks app
cgw tasks preview app build-app      # render the command, run nothing
cgw run app build-app --wait
```

### batches, workflows

`batches <project>` · `batch save <project> <name> <taskId...> | --body '<json>'` ·
`batch run <project> <batchId|name> [--from N] [--wait] [--enqueue]` · `batch rm <project> <batchId>` ·
`workflows [list]` · `workflows show <id>` · `workflows save <id> --body '<json>' | --file <path> | -` ·
`workflows rm <id>` · `workflows apply <project> <id> [--set k=v ...] [--apply] [--overwrite] [--run] [--name N]` ·
`workflows refresh`

The shipped presets are `push-build-pull` and `build-export-zip`. Apply is preview-first: without
`--apply` you get the task list, the deterministic batch id (`wf-<preset id>`) and the conflict
list, and `--overwrite` is required to accept a collision.

```bash
cgw batch save app release push-src build-app pull-out
cgw batch run app release --wait
cgw workflows apply app push-build-pull --set scheme=Runner --apply --run --wait
```

### push, pull, export, exports

`push <project> [--clean] [--keep-crlf] [--wait]` · `pull <project> [--wait]` ·
`export <project> [--source remote|host] [--path P] [--name N]` · `exports <project>` ·
`exports <project> <name> --save <path>`

`push` normalizes CRLF→LF for shebang scripts unless `--keep-crlf`; `--clean` wipes the remote
directory first and refuses `~`, `/`, `.` or the base dir as a target. `export` packs a zip into
the host `exports/` (`--name app-release` becomes `app-release.zip`).

```bash
cgw push app --wait
cgw export app --source remote --path build/output --name app-release
cgw exports app app-release.zip --save ./app-release.zip
```

### jobs

`jobs [list]` · `job <jobId>` · `jobs logs <jobId> [--since N]` · `jobs cancel <jobId>` ·
`jobs retry <jobId> [--from N|--from-start] [--wait]`

History keeps the last 50 jobs. `job` shows the per-step results, so a failure names the step.

```bash
cgw jobs
cgw job j-1a2b
cgw jobs logs j-1a2b --since 0
```

### manifests, schema

`manifests [list]` · `manifests show <id>` · `manifests save <id> --file <path> | -` ·
`manifests rm <id>` · `manifests status` · `manifests templates` · `manifests reset [id...]` ·
`manifests import <file.json> [--id <name>] [--force]` · `manifests export <id> [--save <path>]` ·
`manifests detect <host directory>` · `manifests refresh` · `manifests schema`

`manifests refresh` re-reads the directory after you edited manifest JSON outside the daemon.
`schema` takes one of six names — `manifest`, `task`, `batch`, `client`, `project`, `workflow` —
and prints the draft-07 JSON Schema generated from the Rust structs; `manifests schema` is an alias
of `cgw schema manifest`.

```bash
cgw manifests detect ~/src/app
cgw manifests refresh
cgw schema task
```

### logs, config, set

`logs [days]` · `logs <YYYY-MM-DD> [--source S] [--scope X] [--limit N]` ·
`logs client <id> [--since N]` · `config [show]` · `config set [...]` ·
`set port|listen|user|language|login-interval|log-retain|roots|mcp|host-lock|all`

`logs client` is the live, incremental view; `logs <date>` reads the archived day. `config` is the
flat form, `set` the categorized form that mirrors the Settings page.

```bash
cgw logs client c-mac --since 0
cgw logs 2026-01-31 --source cli --limit 200
cgw set roots add /srv/builds
cgw set host-lock on
```

`port` and `listen` need a daemon restart; `mcp.*`, `allowedRoots`, `login-interval`, `log-retain`
and `host-lock` take effect immediately.

### mcp, api

`mcp [status]` · `mcp url` · `mcp tools [<name>]` · `mcp enable|disable` · `mcp port <N>` ·
`mcp listen lan|wan` · `api <METHOD> <PATH> [JSON]`

`mcp` reports the switch, listen mode, port, the URL a client should be configured with and the
live tool count; `api` is the escape hatch that reaches every endpoint, including ones without a
first-class command.

```bash
cgw mcp url
cgw mcp tools cgw_run_task
cgw api POST /api/projects/p-app/tasks/t-build/run '{"enqueue":true}'
```

See [MCP](MCP) for the agent-facing side and [HTTP API](HTTP-API) for the endpoint list.

## Task and batch execution recipes

**Run one task and wait.** A non-zero exit means "go look at the job", not "parse the message".

```bash
cgw run app build-app --wait --timeout 1800
cgw job j-1a2b --json
```

**Run a batch.** The batch holds the project lock for its whole run; a failing step stops it and the
later steps are recorded as skipped.

```bash
cgw batch run app release --wait
```

**Enqueue when the project is busy.** Without `--enqueue` a second request exits 4 (`projectBusy`)
and names the holder's `jobId`; with it the job waits in the per-project FIFO.

```bash
cgw run app build-app --enqueue --wait
```

**Cancel a running job.** Cancellation is cooperative; a `--wait` caller still exits 5.

```bash
cgw jobs cancel j-1a2b
```

**Retry from the failed step.** Retry reuses the input snapshot recorded in the job, not the task's
current values.

```bash
cgw jobs retry j-1a2b --wait               # resumes at the failed step
cgw jobs retry j-1a2b --from 3 --wait      # pick a step explicitly
cgw jobs retry j-1a2b --from-start --wait  # really start over
```

**Preview a task.** Renders the command and the expanded paths without running anything.

```bash
cgw tasks preview app build-app
```

## Self-describing help

The command surface is data, not prose: the same table renders the human tree, the drill-down, the
WebUI parity table and the JSON contract, and it is also what the MCP `cgw://cli` resource serves.

```bash
cgw help               # top-level tree, one line per group
cgw help tasks         # one group, with usage and WebUI mapping per subcommand
cgw help tasks save    # one leaf: usage, arguments, WebUI counterpart
cgw help --json        # machine-readable: commands, args, exit codes, global flags
cgw help --webui       # "WebUI location -> cgw command" table
```

`cgw help --json` returns `commands[]` (name, usage, summary, args, webui), `exitCodes`,
`globalFlags`, `env`, `help` and `webuiParity`. An unknown path is a usage error (exit 1) that lists
every name valid at that level.

## Common recipes

```bash
# First build: register, verify, push, build, take the artifacts back
cgw clients add --name mac --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519
cgw clients test mac && cgw env mac
cgw projects add --client c-mac --path ~/src/app --name app
cgw push app --wait
cgw run app build-app --wait
cgw pull app --wait

# Push and build in one shot, as a batch
cgw batch save app release push-src build-app
cgw batch run app release --wait

# Export a zip and fetch it
cgw export app --source remote --path build/output --name app-release
cgw exports app app-release.zip --save ./app-release.zip

# Tail logs
cgw logs client c-mac --since 0            # live, incremental
cgw logs 2026-01-31 --source cli --limit 200

# Who is busy
cgw doctor                                 # "N projects, M running" plus warnings
cgw jobs                                   # recent jobs with their state
```

---

← [Home](Home)
