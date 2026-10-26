# WebUI

The WebUI is the browser face of CompileGateway: a React + Fluent UI single page app that the daemon serves from its own HTTP port, and that can do everything the CLI can do except the machine-facing features listed near the end of this page.

## What the WebUI is

- It is one page app built with Vite from `webui-src` into `webui/dist`, and that output is embedded **into the binary at compile time** (`rust-embed`, `src/server.rs`). There is no `node_modules` and no web server to install at runtime: the daemon serves `GET /` and the static assets itself.
- Open `http://<host>:<port>/`. That is the address the daemon prints at startup and reports in `GET /api/status`; with `port: 0` the operating system picks a free port and writes the real one to `runtime.json`.
- The same daemon, the same API. [CLI](CLI) and [MCP](MCP) talk to the exact same endpoints, so a job started from this page behaves like one started anywhere else — including the project lock and the job record.
- The page language follows the server: `config.json`'s `language` (`auto` / `zh-Hans` / `en-US`) decides which string table the WebUI loads, and changing it on the Settings page re-renders the whole page.
- Operations are icon buttons with tooltips; where a label exists in the string table it is quoted below exactly as it appears in the UI.

## The shell

- The left sidebar has five entries: `Status`, `Settings`, `Clients`, `Build types`, and `Workflows`. Inside the instance view a horizontal strip of client chips does the same job without going back to the list.
- The sidebar footer shows the version and the URL this page was loaded from, plus `Sign out` when it applies.
- While the first `/api/status` call is in flight the page shows only a spinner. If that call fails there is no partial page: it shows `Failed to load: {msg}`.
- Navigation, tabs and the client strip keep their text labels; action buttons are icon-only with a tooltip, so hovering is how you read them.

## Opening the page and signing in

| Where the request comes from | What happens |
|---|---|
| Loopback (`127.0.0.1`, `::1`) | Always allowed, **even when credentials are set** — no login page. |
| Another device, no credentials configured | Allowed as well; `lan` / `wan` do not force credentials. |
| Another device, credentials configured | The login page appears: `Username`, `Password`, `OK`. A successful `POST /api/login` sets the session cookie (`HttpOnly`, 12 hours). |
| Same source IP logging in too often | `429`; the page shows `Too many attempts, retry in {s}s.` |
| Changing credentials from another device | The `Username` / `Password` inputs are disabled with the hint `Credentials can only be changed from this machine (127.0.0.1)`. |

- **`lan` vs `wan`.** `Listen mode` `lan` binds only `127.0.0.1`, so only this machine can reach the page. `wan` binds `0.0.0.0`, so other devices can use `host-ip:port`.
- **The `wan` warning is persistent.** With `wan` and no credentials, a yellow banner titled `Security warning` stays at the top of every page until credentials are set. The API reports the same condition as `warning: "wanWithoutAuth"` in `/api/status`.
- **Login throttling is per source IP.** `Login rate-limit interval (seconds)` (`auth.loginIntervalSeconds`) is the minimum gap between two attempts from one IP; `0` disables it, the value is capped at 86400.
- **There is no "forgot password" flow.** Credentials can only be changed from loopback, so a different device can never take the daemon over by resetting them.
- **The page always loads; the data does not.** The static assets (`/`, the JS bundle, the stylesheet) are deliberately unauthenticated — the login page itself has to load — while every `/api/*` call is checked, so an unauthenticated visitor sees the shell and no content at all.
- **Behind a reverse proxy** the daemon only ever sees the proxy's address, so all external users share one throttle bucket and credentials can no longer be changed from outside (that refusal is `authLocalOnly`, and it is by design). Forward `X-Forwarded-Proto: https` so the session cookie is issued as `Secure`; the daemon does not trust `X-Forwarded-For`. See [Configuration and logs](Configuration-and-Logs).
- The page keeps almost no state: reloading it re-reads `/api/status`, `/api/config`, `/api/clients` and `/api/projects`, and any `/api/*` call answered with `401` sends the browser back to the login gate.
- `Sign out` appears in the sidebar footer only for a non-loopback, authenticated request.

## Status page

Read-only summary of the running daemon. Every value comes from `/api/status`:

| Row | What it shows |
|---|---|
| `Version` | App name and version. |
| `URL` | The address this WebUI is served on. |
| `Port` | The bound port. |
| `Listen mode` | `lan — this machine only (localhost / 127.0.0.1)` or `wan — reachable by other devices (host-ip:port)`. |
| `Language` | `auto`, `zh-Hans` or `en-US`. |
| `Credentials` | The username plus `set`, or `not set`. |
| `Other devices` | `yes` / `no` — whether this request came from somewhere other than loopback. |
| `MCP (AI agent)` | The live endpoint plus `N tools`, or `Enabled in the config, but not listening (the port may be taken).`, or `Off.` |
| `Config file` | Where `config.json` lives. |

The page itself has no buttons and no lists: projects, tasks and job history live under `Clients` → a client → `Projects`.

## Clients page

`Clients` manages the SSH targets (`clients.json`, next to `config.json`).

- **Search** matches `name / ip / port / tag`, partially and with regular expressions; an invalid pattern falls back to plain containment.
- **View mode** is a card list with no action buttons. Clicking a card opens that client's instance view (`Click a card to open this instance`).
- **`Edit`** puts every client into edit mode at once. In that mode the toolbar becomes `Add client` + search + `Done`; `Done` commits every addition, change and deletion in one pass.
- Per-client fields: `Name`, `IP address`, `Port` (default 22), `Auth`, `Tags`, `Base directory`, plus the host lock toggle `One job per client at a time (hostLock)` to strictly serialize jobs targeting this physical client.
- `Auth` is either `Password` (username + password) or `Key file` (username + path, with `Choose…` / `Select private key` opening a **file picker on the daemon machine**) plus `Key passphrase`.
- **Secrets are write-only.** The API never returns a password or passphrase, so an empty field means "leave the stored value" (`leave empty to keep it`). `Delete` asks `Delete this client?` and drops the card from the draft; the removal is only persisted when `Done` submits the whole set.
- `Tags` is free-form: each tag has a close button and there is a `New tag` add button.
- **`Test` (connections) and the host key are not on this page.** They live in the instance view, because a test really opens an SSH session and its output belongs to that machine's log.

## Instance page

Opening a client leads to its instance view: a strip of client chips at the top (switch machines without going back to the list), the client name and `host:port · auth`, a `Test` button, and `Back to list`. Below it are four tabs.

### `Status` tab

- Connection facts: `IP address`, `Port`, `Auth`, `Tags`, `Host key`.
- **`Base directory`** — the root for sources and artifacts on that machine. `Save and verify` really connects and runs `mkdir -p` plus `pwd`: on success the resolved absolute path is stored and shown under `Remote path`, on failure **nothing is written** and the remote error is shown as-is.
- **Stored host key (TOFU).** The fingerprint recorded on the first successful connection is shown; a later mismatch rejects the connection until you decide.
  - `Forget it` clears the record, so the next connection is treated as first trust again.
  - `Import from known_hosts` picks a `known_hosts` file **on the daemon machine** and claims the matching key without connecting first. `@revoked` entries, CA entries, and a host with several different keys are refused with an explanation.
  - `Export known_hosts line` produces one standard line you can append to `~/.ssh/known_hosts`; it needs the stored public key text, otherwise the page says only the fingerprint is available.
  - When `Test` hits a mismatch, a dialog titled `Host key changed` shows the stored and the presented fingerprint side by side and offers `Trust this new fingerprint`. Trusting is a deliberate decision, not a retry.

### `Environment` tab

- `Check the target machine` runs the whole environment probe over one SSH session (90 s timeout). Until then the page only reads the server-side cache and says `Never checked yet. Use the button above to connect once.`
- Results are grouped by platform (`xcode`, `gradle`, `flutter`, `dart`, `rust`, `common`), each item with its version or `missing`, plus `present, version unreadable` when a tool exists but will not report a version. A group with nothing found is marked `all missing`.
- `Disk` shows available and total space, used percentage and mount point.
- `Xcode installations` lists every `/Applications/Xcode*.app` with its own version and build, the licence badge (`Licence accepted` / `Licence not accepted`), and the `builds with DEVELOPER_DIR=<dir>` line that a build task can pass per run instead of touching the target's global `xcode-select`.

### `Logs` tab

- A terminal-style pane of this client's log domain. The pane is fixed dark on purpose; it is a log surface, not a normal card.
- `Log range` switches between `Live` (`Last 4000 in memory, refreshed every 1.5s`, polled as an increment) and a day of the on-disk history (`YYYY-MM-DD` files, columns time / source / log domain).
- Source checkboxes start at `All sources` plus `webui`, `cli` and `mcp`, and pick up any other source that actually appears; hiding lines reports `N hidden`.
- `Test` results, build output and security-relevant host-key changes all land here.

## The project card (inside the instance page)

`Projects` lists the source folders registered against this client: `Name`, `Host folder` and `Remote path` per card, plus a second line with `Excludes`, `Last sync` (or `never synced`) and a third with `Artifact` and `Built at` (or `never built`). `Import project folder` opens the daemon-side folder picker (`GET /api/browse`) and registers the chosen directory; removing a registration **deletes no files**.

Card actions, all icon buttons with a tooltip:

| Action | What it does |
|---|---|
| `Add build task` | Opens the task dialog for a new task. |
| `Project file tree` | Read-only tree of the client side and the host side. |
| `Push source` | Sends the source tree over tar/SSH; the confirmation has `Clean the remote directory first (overwrite)` (default off, which means "overwrite same names, keep extra remote files"). |
| `Pull` | Brings the remote directory back to the host-managed folder. |
| `Export archive` | Pack-and-download dialog (below). |
| `Delete` | Removes the registration only. |

- **File tree.** The dialog has `Client` / `Host` tabs, `Up`, a clickable breadcrumb and `root`. Each row can copy its relative path (`Copy the relative path (paste it into a file task)`); files open a controlled preview (binaries report size and time only, long text is truncated), symlinks are badged `symlink`, and `Create a file task from this path` starts a `copy` / `move` / `delete` / `export` task pre-filled with the current directory. Paths are relative to the project root; `..` and absolute paths are rejected at save time.
- **Export archive.** The dialog lists `Existing archives` with size and time and a `Download` link each, shows `Directory` (the host `exports/` folder), and below it builds a new archive: a `Client` / `Host` radio pair, `Path to pack` (defaults to the last build artifact), `Output file name` (empty means `<project>-<unix timestamp>.zip`), then `Start export`.
- **Job status bar.** While a job holds the project, the card shows a spinner, the kind badge, the title, `Running for {s}s`, and a cancel button (`Cancel job`, confirmed with `Cancel this job? The remote command will lose its connection.`). Starting another job for the same project is refused by the daemon (`projectBusy`) until this one finishes.
- **Result banner.** When the job ends, the card shows the outcome (`Build finished`, `Build failed`, `Cancelled`, with the exit code), the note `The build output was written to this client's Logs tab.`, and — for multi-step work — one line per step. If a step failed, a button labelled `Retry from step N` retries **from that step** using the input snapshot recorded in the job.
- **`Build tasks`.** Each row shows the task name, its kind badge (`Build`, `Push`, `Pull`, `Copy`, `Move`, `Delete`, `Export`), a `Signed` / `Unsigned` badge for builds, and the kind's key fields (for builds: manifest, variables and artifact path; for file tasks: `From` → `To` and the overwrite flag). Row actions: `Run build`, `Preview command` (a dialog titled `Command that will run` with `Copy`), `Add to a batch`, `Edit JSON` (`Edit JSON` opens the raw task object, equivalent to `cgw tasks save --body`), `Edit build task`, and `Delete`. Destructive kinds ask for confirmation with the concrete path first (`This is a delete task: <path> will be removed for good.` and similar for `move` and overwriting `copy`).
- **The task dialog** covers all seven kinds: pick the kind first, then only the relevant fields appear — `From`, `To`, `Path to delete`, `Sources` with `Add source`, `Path` (relative, `no .. and no absolute paths`), `Output file name`, `Signing` (`Signed` / `Unsigned`), `Artifact path`, `Artifact list`, and `Which Xcode to use`. `Pick from the file tree` fills a path from the project tree.
- **`Batches`.** A batch is an ordered list of task references. The row shows the name, `N steps` and the chain of task names; actions are `Run batch`, `Edit batch`, `Edit JSON` and `Delete`. The batch dialog offers `Name`, `Add step`, `Move up`, `Move down`; steps run in order, any failure stops the batch and later steps are recorded as skipped. Step-level variables (`steps[].variables`) can only be written through `Edit JSON`.
- **`Recent jobs`.** Below the cards, the last few jobs of this client with kind, state (`Running`, `Finished`, `Failed`, `Cancelled`), elapsed time, message, and a retry button when there is a failed step to resume from.

## Build types page

`Build types` is the manifest editor; each manifest is one build type, stored as `manifest/<id>.json`.

- **`Reload from disk`** is the only manual refresh in the WebUI, and it exists because those files are yours: if you edit a JSON in an editor, pull it with git or let another tool write it, the daemon cannot see that. Reloading re-reads the directory and reports what was added, changed or removed.
- **`Restore missing templates`** appears only when a built-in template is missing from disk and writes back **only** the missing ones; your edited files are never touched.
- **`New manifest`** asks for `File name (id) of the new manifest` and writes a starter template. Reusing an existing id asks first (`This build type already exists`).
- Each card shows display name, platform, the `shipped` badge for built-in names, `id`, `include.paths`, `Steps`, `Artifact`, `Cache` and `Variables`; invalid files show their validation errors on the card.
- Actions: `Edit manifest` (raw JSON textarea with `Format`, `Cancel`, `Save`; the daemon validates before saving and lists every problem), `Export JSON` (browser download of `<id>.json`) and `Delete manifest`.
- **Detect.** The input at the top takes a **host** directory and `Detect` reports which build types match, listing the `Matched:` paths and any `Missing:` ones. All of a manifest's `include.paths` must exist for it to match.

## Workflows page

`Workflows` provides centralized visual management and parameterized application of cross-project workflow presets (`workflows/<id>.json`):

- **Preset catalog**: Cards display preset name, ID badge, description, input count, task count, and the execution step chain.
- **Preset actions**:
  - `New workflow`: Provide a preset ID to create a new pipeline from a starter skeleton.
  - `Reload from disk`: Re-read `workflows/` directory from disk to pick up external file changes.
  - `Restore missing templates`: Automatically re-seed deleted built-in official templates.
  - `Edit JSON`: Raw JSON textarea with auto-formatting and server-side validation.
  - `Export JSON`: Download `<id>.json` directly in the browser.
  - `Delete workflow`: Prompt for confirmation and delete the file from disk (does not affect previously applied projects).
- **`Apply to project (Apply)` dialog**:
  - Select target project from dropdown list.
  - Dynamic parameter inputs: Automatically populated based on template's declared `variables` (marking required inputs and prefilling defaults).
  - Pre-flight preview: Previews created tasks, target batch ID (`wf-<preset id>`), and any name conflicts in the target project.
  - Overwrite toggle: Check `Overwrite conflicting tasks and workflows` to safely replace existing definitions.
  - Click `Confirm apply`: Submits the request and immediately generates the tasks and batch workflow in the target project.

## Settings page

The card at the top holds the general settings:

| Field | Notes |
|---|---|
| `Port` | `0 = pick a free port automatically`. A fixed port that is busy is an error, not a retry. |
| `Language` | `auto` / `zh-Hans` / `en-US`; the page follows it. |
| `Listen mode` | `lan` or `wan`; marked `restart required`. |
| `Login rate-limit interval (seconds)` | `0` = no limit, capped at 86400. |
| `Log retention (days)` | Daily rolling files older than this are deleted; default 7, capped at 3650. |
| `Log directory` | Read-only: where the daily files live. |
| `Extra host directories` | One absolute path per line. Browsing, project registration and build-type detection **share** this list. |
| `One job per client at a time` | The host lock, off by default. When on, another project on the same client gets `hostBusy` instead of running in parallel. |
| `Username`, `Password`, `Clear the stored password` | Credentials; disabled unless the request came from loopback. |

- `Save` reports either `Saved.` or `Saved. Port and listen mode changes take effect after a restart.`
- The **`MCP (AI agent)`** card is separate because it behaves differently: `Enable MCP`, `MCP listen mode`, `MCP port`, a read-only `Currently live` row, and `Give this URL to your client` with `Copy URL`. It listens on its own port (default 3994, `0` = pick one), independently of the WebUI port, and `MCP changes apply immediately — no restart.` A port that cannot be bound is reported right away and the previously working port keeps serving.
- Warnings shown here: the `MCP` wan-without-credentials note, a note when the default MCP port had to give way, and the persistent `wan` security banner from the top of the page.

## The live log pane in the CLI-style window vs the `Logs` tab

The desktop binary `compilegateway` is a **separate terminal-style window**: a log pane on top and an input box below, plus `diag <name>` to open one extra window per client. That pane is the local, always-available view of what this process is doing — it also has to work when the HTTP server never came up (a busy port), which is exactly when you need to read the log. The WebUI's `Logs` tab is the remote view of the same daemon over HTTP: it is scoped to one client, it can switch from the live tail to any day of the on-disk history, and it can filter by source. Use the window to see "is the service even running", and the `Logs` tab to inspect one target machine's build output after the fact. [Configuration and logs](Configuration-and-Logs) covers the files themselves.

## What the WebUI does not do

One capability is intentionally CLI / HTTP / MCP only, because it is for machines rather than for clicking:

- **JSON Schema access.** `cgw schema [name]` and `GET /api/schema/{name}` return draft-07 schemas for `manifest`, `task`, `batch`, `client`, `project` and `workflow`, plus the MCP resources `cgw://schema/<name>`. There is no schema viewer page.

Two related facts worth knowing:

- The `manifest/` and `workflows/` directories have **no file watcher**; `Reload from disk` on either page is the explicit refresh, equivalent to `cgw manifests refresh` and `cgw workflows refresh`.
- Native pickers are daemon-side: `Choose…`, `Import project folder` and `Import from known_hosts` all list files on the machine running the daemon (`/api/browse`, `/api/dialog/pick-file`, `/api/dialog/pick-folder`), not on the device showing the page.

## Which page solves which problem

| Goal | Where to go |
|---|---|
| See version, port, listen mode, credential state, MCP address | `Status` |
| Add, change or remove an SSH target | `Clients` |
| Find out whether a target can build this project at all | `Clients` → client → `Environment` |
| Watch a build or a sync scroll by | `Clients` → client → `Logs` |
| Set the root directory a target uses | client → `Status` → `Base directory` |
| Re-claim a host key after rebuilding a VM | client → `Status` → `Trust this new fingerprint` / `Import from known_hosts` |
| Push source, pull artifacts, run one task | client → `Projects` → project card |
| Run several tasks in order and resume after a failure | project card → `Batches`, `Run batch`, `Retry from step N` |
| Produce a zip and download it | project card → `Export archive` |
| Edit a build command template | `Build types` |
| Inspect, edit and apply cross-project workflow presets | `Workflows` |
| Change port, language, credentials, throttling, retention, allowed directories, host lock | `Settings` |
| Point an AI client at the daemon | `Settings` → `MCP (AI agent)` → `Give this URL to your client` |
| Get a JSON Schema | Not here — [CLI](CLI) or the HTTP API |

## Notes

| Setting | When it takes effect |
|---|---|
| `Port`, `Listen mode` | After restarting the program. |
| `Language` | Immediately — the whole page follows the server value. |
| `Username`, `Password`, `Clear the stored password` | Immediately, from loopback only. |
| `Login rate-limit interval (seconds)` | Immediately; re-read on every login attempt. |
| `Log retention (days)` | Immediately. |
| `Extra host directories` | Immediately. |
| `One job per client at a time` | Immediately. |
| `MCP (AI agent)` group, including its port | Immediately; the daemon rebinds at once. |

- **Where the log source `webui` comes from.** Every request the page makes carries `X-Client: webui`, and the daemon records that as the log line's source and as the job's `source`. So `webui` appears in the `Logs` tab's source checkboxes (they are seeded with `webui`, `cli` and `mcp` and pick up anything else that shows up), in the `source` column when you read history, and in the `source` field of every job this page started. The page itself writes no log lines — the label comes from the request header.
- Narrow instead of scrolling: the live pane keeps the last 4000 entries in memory, history is read one day at a time, and each stored line also carries a log domain (`global`, `client`, `project`, `job`) that the CLI exposes as `--scope`.
- If a page stays solid-colour or unclickable, it is almost always a Fluent portal mounting over the shell; [Troubleshooting](Troubleshooting) has the details.

← [Home](Home)
