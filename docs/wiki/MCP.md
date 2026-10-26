# Model Context Protocol (MCP)

CompileGateway includes a native, built-in Model Context Protocol (MCP) server running inside the
daemon on an independent port, providing AI coding assistants (Claude Code, Cursor, Cline,
Codex, etc.) with structured access to the complete remote build toolchain.

## Architecture

```
┌───────────────────────────────────────────────────────────┐
│                      CompileGateway                       │
│                                                           │
│  HTTP API (:65251)           Native MCP Server (:3994)    │
│  ├─ WebUI                    ├─ 42 Tools                  │
│  ├─ cgw (CLI)                ├─ 8 Resources               │
│  └─ REST endpoints           └─ 3 Prompt templates        │
└──────────────┬─────────────────────────────┬──────────────┘
               │                             │
       (Loopback / LAN)            Streamable HTTP / stdio
               │                             │
        Browser / Terminal            AI Agent / Client
```

- **Built into the daemon (Rust):** No external Node.js runtime is required for the server itself.
  When the daemon starts, the MCP server starts with it.
- **Dedicated port:** Runs on its own port (default `3994`, configured via `mcp.port` and
  `mcp.listen`). Changes to MCP configuration take effect **immediately** without restarting the
  daemon.
- **Transport protocol:** Streamable HTTP (`POST /mcp`, stateless JSON-RPC 2.0).
- **Optional stdio bridge (`mcp/`):** A zero-dependency Node.js bridge is included for legacy
  clients that only speak stdio. Clients supporting Streamable HTTP can connect directly to the
  endpoint without Node.js.

## Configuring your MCP Client

### Streamable HTTP (Recommended)

Point your client directly to the MCP endpoint:

```json
{
  "mcpServers": {
    "compilegateway": {
      "url": "http://127.0.0.1:3994/mcp"
    }
  }
}
```

If connecting over LAN/WAN with authentication enabled, include HTTP Basic credentials:

```json
{
  "mcpServers": {
    "compilegateway": {
      "url": "http://192.168.1.20:3994/mcp",
      "headers": {
        "Authorization": "Basic YWxpY2U6c2VjcmV0"
      }
    }
  }
}
```

> [!NOTE]
> When running in WAN mode with authentication enabled, MCP enforces IP-based rate limiting against brute-force password guessing. Repeated failed attempts trigger a cool-down block, returning HTTP 429 and JSON-RPC error `-32001` with a `Retry-After` header.

### Stdio Bridge

For desktop apps (like Claude Desktop) requiring stdio:

```json
{
  "mcpServers": {
    "compilegateway": {
      "command": "node",
      "args": ["/path/to/CompileGateway/mcp/dist/index.js"]
    }
  }
}
```

The bridge reads `runtime.json` in `CGW_HOME` (or `CGW_URL`) automatically to find the running
daemon.

## MCP Tools (42 Tools)

All tool parameter schemas are generated directly from the daemon's Rust structures, ensuring zero
schema drift between the API and MCP.

### System and Configuration

| Tool | Purpose |
|---|---|
| `cgw_status` | Query daemon version, listen mode, language, active port and MCP status |
| `cgw_get_config` | Read current runtime configuration |
| `cgw_set_config` | Update configuration (`mcp.*`, `allowedRoots`, `hostLock`, `loginIntervalSeconds`, etc.) |

### Target Clients and Environment

| Tool | Purpose |
|---|---|
| `cgw_list_clients` | List all configured SSH target machines (names, hosts, ports, auth methods, base directories, and `hostLock` status) |
| `cgw_save_client` | Create or update a client configuration (credentials, key files, base directory, and per-client `hostLock` mutual exclusion toggle) |
| `cgw_delete_client` | Remove a client configuration |
| `cgw_test_client` | Perform a real SSH connection probe and verify TOFU host key |
| `cgw_detect_env` | Run single-roundtrip remote environment probe (Xcode, JDK, Gradle, Rust, disk, etc.) |

### Projects and File System

| Tool | Purpose |
|---|---|
| `cgw_list_projects` | List registered projects and active execution status |
| `cgw_browse` | List host directories inside `allowedRoots` to pick source paths |
| `cgw_create_project` | Register a new source directory and link to a client |
| `cgw_delete_project` | Remove a project registration (preserves all files) |
| `cgw_tree` | Traverse controlled file tree on either `remote` or `host` side |

### Tasks and Execution

| Tool | Purpose |
|---|---|
| `cgw_list_tasks` | List a project's tasks across all 7 kinds (`build`, `push`, `pull`, `copy`, `move`, `delete`, `export`) |
| `cgw_save_task` | Create or update a project task (validates relative paths and contracts) |
| `cgw_delete_task` | Remove a task and clean up references in batches |
| `cgw_preview_task` | Dry-run render commands and expanded paths without executing |
| `cgw_run_task` | Start a task (`enqueue: true` to queue if busy; queued plans persist across daemon restarts and resume automatically); returns `jobId` immediately |

### Batches and Workflows

| Tool | Purpose |
|---|---|
| `cgw_list_batches` | List multi-step batch workflows in a project |
| `cgw_save_batch` | Create or update a batch workflow (validates step order; supports referencing task outputs and manifest named-step `outputs` via `{{steps.<stepId>.outputs.<name>}}`) |
| `cgw_delete_batch` | Delete a batch workflow |
| `cgw_run_batch` | Execute a batch workflow (`enqueue: true` to queue; queued plans persist across daemon restarts and resume automatically) |
| `cgw_list_workflows` | List cross-project workflow templates |
| `cgw_read_workflow` | Read workflow template raw JSON and parse result |
| `cgw_save_workflow` | Create or update a cross-project workflow template |
| `cgw_delete_workflow` | Delete a workflow template |
| `cgw_apply_workflow` | Preview or apply a workflow template to a project (with variable parameter substitution and conflict detection) |

### Build Types (Manifests)

| Tool | Purpose |
|---|---|
| `cgw_list_manifests` | List all build types with validation status and default artifacts |
| `cgw_manifest_raw` | Read raw manifest JSON |
| `cgw_save_manifest` | Create or update a build type manifest (supports named `build.steps` with step-level `outputs` declarations and verification) |
| `cgw_delete_manifest` | Delete a build type manifest |
| `cgw_reload_manifests`| Trigger directory refresh for external disk changes (`manifest/`) |
| `cgw_manifest_status` | Inspect manifest directory state and missing built-ins |
| `cgw_export_manifest` | Export manifest JSON content |
| `cgw_detect_manifests`| Detect matching build types for a source directory |

### Jobs and Logs

| Tool | Purpose |
|---|---|
| `cgw_list_jobs` | Query recent 50 jobs with execution states |
| `cgw_job` | Query detailed job status, outputs and step results |
| `cgw_job_logs` | Stream or fetch logs for a specific job |
| `cgw_cancel_job` | Cooperatively cancel an active or pending job |
| `cgw_retry_job` | Resume a failed job from the failed step or start over |
| `cgw_log_days` | Query archived log dates and retention settings |
| `cgw_logs` | Read archived daily logs with source/scope filtering |

## MCP Resources (8 Resources)

| URI | Name | Description |
|---|---|---|
| `cgw://guide` | CompileGateway Guide | One-page playbook covering core workflows, concurrency rules and failure recovery |
| `cgw://cli` | CLI Command Matrix | Complete machine-readable CLI command table, exit codes and flags (derived from `help::HELP`) |
| `cgw://schema/manifest` | Manifest Schema | Draft-07 JSON Schema for build type manifests |
| `cgw://schema/task` | Task Schema | Draft-07 JSON Schema for project tasks |
| `cgw://schema/batch` | Batch Schema | Draft-07 JSON Schema for project batches |
| `cgw://schema/client` | Client Schema | Draft-07 JSON Schema for client requests |
| `cgw://schema/project` | Project Schema | Draft-07 JSON Schema for project registrations |
| `cgw://schema/workflow`| Workflow Schema | Draft-07 JSON Schema for workflow presets |

## Prompt Templates (3 Prompts)

- **`first-build`**: Guides an agent through connecting a client, detecting remote tools,
  registering a project, syncing source, and performing the first remote build.
- **`diagnose-job`**: Takes a `jobId`, fetches step breakdown and logs, analyzes failure root
  cause, and suggests fixes or retry commands.
- **`pick-manifest`**: Runs directory detection (`cgw_detect_manifests`) and helps choose and
  configure the optimal build type.

## Agent Guidelines & Playbook

1. **Always read `cgw://guide` first:** Before orchestrating complex flows, inspect the playbook to
   understand lock semantics and error codes.
2. **Handle `409 projectBusy` gracefully:** When a project is already executing a job, do not poll
   rapidly in a loop. Use `enqueue: true` on `cgw_run_task` / `cgw_run_batch` to wait in the FIFO
   queue. Queued execution plans persist to disk (`queued_runs.json`) and resume automatically if
   the daemon restarts.
3. **Handle `409 hostBusy`:** When `hostLock` is enabled (either globally via `cgw_set_config` or
   per-client via `cgw_save_client`), multiple projects targeting the same host will serialize. Use
   `enqueue: true` to wait cleanly in the queue.
4. **Use dry-run preview:** Call `cgw_preview_task` to verify rendered shell commands and expanded
   paths before triggering expensive builds.
5. **Use step retries:** On batch failures, call `cgw_retry_job` to resume execution from the
   failing step without repeating already completed steps.
6. **Leverage step-level outputs:** Manifests can declare output artifacts directly inside named
   `build.steps` (`"outputs": { "app": "path" }`). The daemon verifies their existence and
   directory/file types upon completion, and publishes them for downstream batch steps to consume
   via `{{steps.<stepId>.outputs.<name>}}`.

← [Home](Home)
