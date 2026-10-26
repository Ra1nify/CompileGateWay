# CompileGateway

CompileGateway pushes a source tree from your machine to an SSH target (usually a macOS build
box), runs the build there, and brings the artifacts back or packages them for export.

It is one core implementation with four front ends: a browser UI, a command-line client, a
built-in MCP server for AI agents, and the HTTP API they all use.

## What it does

- **Push** — send a local project directory to the target machine, with sensible excludes and
  CRLF→LF normalization for scripts.
- **Build** — run the target machine's own toolchain through a declarative *build type*
  (manifest): variables, ordered steps, artifact paths.
- **Bring results back** — pull artifacts to the host, export them as a zip, or copy/move/delete
  files on either side.

## Four front ends, one implementation

| Front end | What it is | Use it when |
|---|---|---|
| **WebUI** | browser UI served by the daemon | you want to click through projects, jobs, logs and settings |
| **CLI (`cgw`)** | thin HTTP client, never prompts | scripts, terminals, anything repeatable |
| **MCP** | built-in server on its own port | you want an AI agent to drive the builds |
| **HTTP API** | the API the other three use | you are writing your own client |

**Parity principle**: Whatever the WebUI can do, the CLI can do. The reverse is intentionally scoped (machine-oriented capabilities like raw JSON Schema retrieval and workflow-preset command-line application are CLI/MCP/HTTP only to keep the WebUI clean and focused).

## Requirements

Rust (nightly) and Node 18+ to build; WebView2 on Windows for the desktop shell; an SSH
target machine; the target's own toolchain (Xcode, Gradle, …) for builds.

## Quick start

```bash
# 1. build the WebUI first (it is baked into the binary)
cd webui-src && npm ci && npm run build && cd ..

# 2. build the binaries
cd src-tauri && cargo build --release && cd ..

# 3. start the daemon (headless) or the desktop shell
src-tauri/target/release/compilegateway-daemon

# 4. point the CLI at it (it finds the daemon by itself in most cases)
cgw doctor
```

Then: add a client (SSH target), register a project, push, pick or write a build type, build,
and pull or export the result. [Getting started](Getting-Started) walks through it.

## Documentation

| Page | What it covers |
|---|---|
| [Getting started](Getting-Started) | build, run, first project, first build |
| [Concepts](Concepts) | the object model, job lifecycle, concurrency rules |
| [Tasks](Tasks) | the seven task kinds, input/output contracts, artifacts |
| [Batches and workflows](Batches-and-Workflows) | ordered multi-step runs and cross-project templates |
| [Build types (manifests)](Manifests) | describing how a kind of project is built |
| [Clients and environment](Clients-and-Environment) | SSH targets, host keys, base dir, environment probes |
| [WebUI](WebUI) | the browser interface, page by page |
| [CLI](CLI) | `cgw`: commands, exit codes, scripting |
| [MCP](MCP) | the built-in AI-agent server |
| [HTTP API](HTTP-API) | endpoints, auth, error codes |
| [Configuration and logs](Configuration-and-Logs) | `config.json`, three ways to change it, log files |
| [Troubleshooting](Troubleshooting) | symptoms, error codes, what to try |

## At a glance

| | |
|---|---|
| Task kinds | `build`, `push`, `pull`, `copy`, `move`, `delete`, `export` |
| Concurrency | one job per project; per-project FIFO queue with `enqueue`; optional host-wide lock |
| MCP | 42 tools, 8 resources, 3 prompts, its own port (default 3994) |
| Schemas | `manifest`, `task`, `batch`, `client`, `project`, `workflow` — generated from the Rust structs |
| State on disk | `config.json`, `runtime.json`, `projects.json`, `jobs.json`, `manifest/`, `workflows/`, `logs/` |
