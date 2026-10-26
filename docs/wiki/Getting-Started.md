# Getting Started

CompileGateway pushes a local source tree to an SSH target machine, runs the build there, and brings the artifacts back or packages them for export.

## Prerequisites

| Requirement | Notes |
|---|---|
| Rust **nightly** toolchain | The crate is built on nightly; install it with `rustup toolchain install nightly`. |
| Node 18+ | Only needed to build the WebUI assets. |
| WebView2 runtime (Windows) | Required by the Tauri shell window. |
| A reachable SSH target machine | Usually a macOS build box. It must be reachable from the machine running the daemon, and the account you use must be able to write inside the base directory. |

The headless daemon and the CLI do not need a GUI toolkit, so a small always-on Linux host without GTK/WebKit can still build and run them.

## Build from source

### 1. Build the WebUI first

```bash
cd webui-src
npm ci
npm run build
cd ..
```

This produces `webui/dist`. That directory must exist **before** you build the Rust side: the daemon embeds the WebUI into its own binary with `rust-embed` at compile time. It is committed on purpose, so a plain checkout already contains a working build — rebuild it only after changing the frontend sources.

### 2. Build the binaries

```bash
cd src-tauri
cargo build --release
```

The artifacts land in `src-tauri/target/release/`:

| Binary | Purpose | Needs GUI libraries |
|---|---|---|
| `compilegateway` | Tauri shell: a CLI-style window plus the embedded WebUI | yes |
| `compilegateway-daemon` | Headless HTTP daemon with the built-in MCP server | no |
| `compilegateway-cli` | Command-line client (`cgw`) | no |

The three targets share one core library, so they behave identically. The Tauri dependencies all sit behind the default `gui` feature; `compilegateway` itself is only built when that feature is on.

### 3. Headless-only build

On a machine without GTK/WebKit development packages, turn the GUI feature off and build just the two front ends that do not need it:

```bash
cd src-tauri
cargo build --release --no-default-features --bin compilegateway-daemon --bin compilegateway-cli
```

With `--no-default-features`, Tauri, wry and webkit are neither compiled nor linked.

## Run it

There are three front ends over the same daemon; start one daemon and use whichever client you like.

```bash
# 1) The Tauri shell: window + embedded WebUI (needs a graphical session)
./target/release/compilegateway

# 2) The headless daemon: HTTP only, no window and no webview (--open also opens the WebUI)
./target/release/compilegateway-daemon

# 3) The CLI client
./target/release/compilegateway-cli --help
```

### What appears on first start

Both daemons create their state next to the executable (falling back to the current working directory), and both also write a pointer copy into `CGW_HOME` so a `cgw` that lives elsewhere can find them:

| Path | Created when | What it is |
|---|---|---|
| `config.json` | first start | The daemon configuration. Every field may be omitted; defaults are filled in. |
| `runtime.json` | every start | The ports actually bound (`url`, `port`, `listen`, `mcpUrl`, `mcpPort`, `mcpEnabled`, `pid`, `configPath`, `startedAt`). |
| `manifest/` | first start | Build types. Seeded once from the built-in templates, then it is yours. |
| `workflows/` | first start | Cross-project workflow presets, seeded once the same way. |
| `logs/` | every start | Daily rolling log files, `YYYY-MM-DD.log`. |
| `projects.json` / `jobs.json` | first project / first job | Project registry and job history (the last 50 jobs). |

### Finding the port when `port` is `0`

`port: 0` (or an omitted `port`) means "let the system pick a free port". `config.json` cannot tell you the answer, so read `runtime.json` (`cat runtime.json`):

```json
{
  "url": "http://127.0.0.1:65251",
  "port": 65251,
  "listen": "lan",
  "mcpUrl": "http://127.0.0.1:3994",
  "mcpPort": 3994,
  "mcpEnabled": true
}
```

The `url` field is the WebUI/API base address; `mcpUrl` is the MCP endpoint. A fixed port that is already in use is an error, not a retry.

### 4. Wire up the CLI

If the `cgw` executable is not on your `PATH`, or the daemon runs on another machine, use one of these three routes:

1. Put `cgw` on `PATH` so it can be run from any directory: `cgw setup --apply` writes the executable's directory into the current user's `Path` on Windows (no administrator rights needed), while on Linux/macOS `cgw setup` prints the `export PATH=...` line for your shell profile.
2. Set `CGW_HOME` to relocate the per-user shared directory (default `~/.compilegateway`), where the daemon writes its `runtime.json` pointer so the CLI finds it from any working directory.
3. Set `CGW_URL` to the daemon address, e.g. `CGW_URL=http://127.0.0.1:65251`, to skip discovery entirely; `--url <url>` does the same for a single command.

Run `cgw setup` to see all of this at once and `cgw doctor` to check whether work can happen right now (`cgw doctor` exits non-zero when a check fails).

## First project, end to end

### 1. Create a client

A client is the SSH target machine. Authenticate with a password or a private key; `--key-passphrase` covers an encrypted key, `--port` defaults to 22 and `--base-dir` to `~/CompileGateway`. Secrets are write-only: the API never returns a stored password or passphrase.

```bash
cgw clients add --host 192.168.1.20 --user builder --password 'secret' --name mac
cgw clients add --host 192.168.1.20 --user builder --key ~/.ssh/id_ed25519 --name mac
```

### 2. Test the connection and confirm the host key

The first connection records the host fingerprint (trust on first use). If a later connection presents a different key, the test fails with both fingerprints and expects an explicit decision. `@revoked` markers, cert-authority lines, multiple keys on one line and silent replacement are all rejected.

```bash
cgw clients test mac
cgw clients hostkey mac                       # show what is recorded
cgw clients hostkey mac --trust SHA256:...    # accept a new fingerprint
cgw clients hostkey mac --forget              # drop the record, next connection is TOFU again
cgw clients hostkey mac --import known_hosts  # pre-trust from a known_hosts file
cgw clients hostkey mac --export              # print one known_hosts line
```

### 3. Set the client base directory

```bash
cgw basedir mac ~/CompileGateway
```

By default this really connects, runs `mkdir -p` and stores the resolved absolute path; add `--no-verify` to save without connecting.

### 4. Register a project from a local directory

The path is a directory on the machine running the daemon (the host), not on the target.

```bash
cgw projects add --client mac --path /home/me/src/app --name app
```

The project is now registered under an id such as `p-xxxx`; the remote source directory is derived from the client's base directory. `cgw projects` lists what is registered, and `cgw projects rm <id>` removes the registration without deleting any files.

### 5. Push the source

```bash
cgw push app --wait
```

Source is transferred as a tar stream over SSH, preserving symlinks and permissions. Files with a shebang are normalized from CRLF to LF unless you pass `--keep-crlf` (this prevents `bad interpreter: /bin/sh^M` on macOS); `--clean` wipes the remote project directory first.

### 6. Create a build task and inspect the manifest

A `build` task refers to a build type by id and carries the variable values for that run. `cgw tasks preview` renders the command and the expanded paths without running anything.

```bash
cgw manifests                       # list build types; `cgw manifests show xcode` prints the source
cgw tasks save app --body '{"name":"build","kind":"build","manifest":"xcode","variables":{"scheme":"Runner"}}'
cgw tasks app                       # list the project's tasks, with their generated t-xxxx ids
cgw tasks preview app t-1a2b        # render the command and expanded paths without running
```

A build type lives at `manifest/<id>.json`; the file name is the id. It declares `displayName`, `platform`, detection rules (`include.paths`, all of which must exist), `variables` (`name`, `label`, `required`, `default`), `build.steps` (either a bare command string or `{ "id": ..., "run": ... }`), `build.artifactPath`, `build.artifacts` and `cache.paths`. Every `{{name}}` placeholder must be a declared variable; the reserved `signingExtraArgs` is injected by `signingMode` and must not be declared. See [Manifests](Manifests).

### 7. Run the build

```bash
cgw run app t-1a2b --wait           # `cgw job <jobId>` shows each step; `cgw jobs logs <jobId>` the log
```

A build does **not** push first, so an untouched project fails against an empty remote directory — push before building, or use a batch. A fixed manifest variable marked `required` with no value fails with `variableRequired`.

### 8. Bring the artifacts back

```bash
cgw pull app --wait                 # remote -> host managed directory
cgw export app --source remote --path .cgw/derived-data/Build/Products --name app.zip
```

`pull` copies the declared artifacts into the project's host managed directory, `~/CompileGateway/<project>/` on the daemon machine; `export` packs a zip into `exports/` inside that same directory instead (declared sources plus an optional `outputName`, no destination needed). List what is there with `cgw exports app`.

## Sanity checks

| Check | Command / place | Passes when |
|---|---|---|
| Environment self-test | `cgw doctor` | It reports the daemon version and URL, auth, project count, manifest validity and log retention. Any failing check is marked `FAIL`; `WARN` lines are shown separately. |
| Daemon status | `cgw status` | Shows port, listen mode, language, config file and WebUI URL, plus client and running-job counts. |
| HTTP liveness | `GET /healthz` | Returns `ok` as plain text. No auth needed. |
| WebUI | the `url` from `runtime.json` in a browser | The WebUI loads and the status page renders. |

Where to look next: [Concepts](Concepts) for the vocabulary, [Tasks](Tasks) for the seven task kinds, [Batches-and-Workflows](Batches-and-Workflows) for chaining them, [Manifests](Manifests) for build types, [Clients-and-Environment](Clients-and-Environment) for SSH targets and host keys, [WebUI](WebUI) / [CLI](CLI) / [MCP](MCP) / [HTTP-API](HTTP-API) for the four front ends, [Configuration-and-Logs](Configuration-and-Logs) for every config key and the logs, and [Troubleshooting](Troubleshooting) when something does not work.

## Files created on disk

| File / directory | Location | Contents |
|---|---|---|
| `config.json` | next to the executable (or the current working directory) | Daemon configuration; generated with defaults on first start. |
| `runtime.json` | next to `config.json`, plus a copy in `CGW_HOME` | The actually bound ports and process info. |
| `manifest/` `workflows/` | next to `config.json` | Build types and workflow presets, `<id>.json`. Both are yours to edit. |
| `logs/` | next to `config.json` | Daily log files, `YYYY-MM-DD.log`. |
| `projects.json` `jobs.json` | next to `config.json` | Project registry with their tasks and batches; job history (the last 50). |
| `exports/` | project host managed directory, `~/CompileGateway/<project>/` | Zip archives produced by `export`. |
| `~/.compilegateway/` | `CGW_HOME` (overridable) | The shared pointer location where the CLI finds the daemon. |

---

← [Home](Home)
