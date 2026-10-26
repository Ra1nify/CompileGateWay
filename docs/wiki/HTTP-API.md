# HTTP REST API

CompileGateway exposes a comprehensive RESTful API over HTTP/1.1. The WebUI, `cgw` CLI, and native
MCP server all communicate through these endpoints.

## Base URL and Headers

- **Default base URL:** `http://127.0.0.1:<port>` (actual port recorded in `runtime.json`).
- **Client Source Header:** Pass `X-Client: webui | cli | mcp | daemon` on requests to tag
  audit logs.
- **Content Type:** `Content-Type: application/json` for request bodies.

## Authentication

| Request Source | Requirement | Mechanism |
|---|---|---|
| Loopback (`127.0.0.1` / `::1`) | None (Always bypasses auth) | Direct access |
| LAN / WAN (No credentials configured) | None | Direct access (Yellow security banner in UI) |
| LAN / WAN (Credentials configured) | Login required | Session Cookie (`cgw_session`, 12h, `HttpOnly`, `SameSite=Lax`) |

### Auth Endpoints

- `POST /api/login` — Authenticate and issue session cookie:
  ```json
  { "username": "alice", "password": "secret" }
  ```
- `POST /api/logout` — Revoke active session.

## Error Response Model

All API responses use a uniform JSON wrapper:

### Success Format
```json
{ "ok": true, "data": { ... } }
```

### Error Format
```json
{
  "ok": false,
  "error": {
    "code": "projectBusy",
    "httpStatus": 409,
    "message": "Project is busy",
    "retryAfterSeconds": 5,
    "jobId": "j-1a2b",
    "projectId": "p-app"
  }
}
```

### Common Error Codes

| Code | HTTP Status | Description |
|---|---|---|
| `unauthorized` | 401 | Missing or invalid session cookie |
| `authLocalOnly` | 403 | Attempted to modify credentials/security settings from a non-loopback address |
| `authRateLimited` | 429 | Exceeded login failure rate limit (carries `retryAfterSeconds`) |
| `outsideRoot` | 403 | Path is outside `allowedRoots` or user home boundary |
| `notFound` | 404 | Target client, project, task, batch, job or manifest ID not found |
| `projectBusy` | 409 | Project already has an active job (carries active `jobId` and `retryAfterSeconds`) |
| `hostBusy` | 409 | `hostLock` is enabled and the target client is running another job |
| `artifactMissing` | 500 | Build succeeded (exit code 0) but declared output artifacts were not found |
| `workflowConflict` | 409 | Workflow apply conflict without `overwrite: true` |
| `workflowInvalid` | 400 | Invalid workflow preset JSON or undefined variable reference |
| `manifestInvalid` | 400 | Invalid manifest JSON syntax or undeclared variable placeholder |

---

## Endpoint Reference

### 1. System & Configuration

- `GET /api/status` — Get daemon health, version, auth state, and active warning flags.
- `GET /api/config` — Retrieve full runtime configuration.
- `PATCH /api/config` — Apply configuration patch (pass fields to update).
- `GET /api/schema/{name}` — Fetch draft-07 JSON Schema for `manifest`, `task`, `batch`, `client`, `project`, or `workflow`.

### 2. Clients (SSH Targets) & Environment

- `GET /api/clients` — List all configured SSH targets.
- `POST /api/clients` — Add a new SSH client.
- `PATCH /api/clients/{id}` — Update client settings.
- `DELETE /api/clients/{id}` — Remove a client.
- `POST /api/clients/{id}/test` — Perform live SSH connection and TOFU key check.
- `POST /api/clients/{id}/hostkey` — Manage host keys (`trust`, `forget`, `import`, `export`).
- `POST /api/clients/{id}/basedir` — Validate and save base directory on remote host.
- `GET /api/clients/{id}/env` — Read cached environment detection report.
- `POST /api/clients/{id}/env` — Trigger live remote toolchain detection (Xcode, JDK, Rust, etc.).

### 3. File System & Projects

- `GET /api/browse?path=<absolute-path>` — Browse daemon host directories within `allowedRoots`.
- `GET /api/projects` — List registered projects and active execution status.
- `POST /api/projects` — Register a source directory to a client.
- `DELETE /api/projects/{id}` — Remove project registration (keeps local and remote files).
- `GET /api/projects/{id}/tree?side=remote|host&path=<relative-path>` — List file tree entries.
- `GET /api/projects/{id}/cat?side=remote|host&path=<relative-path>` — Controlled text file preview.

### 4. Tasks & Execution

- `GET /api/projects/{id}/tasks` — List tasks defined in a project.
- `POST /api/projects/{id}/tasks` — Save/create a task definition.
- `PATCH /api/projects/{id}/tasks/{taskId}` — Update an existing task.
- `DELETE /api/projects/{id}/tasks/{taskId}` — Delete a task.
- `POST /api/projects/{id}/tasks/{taskId}/preview` — Render task command and expanded variables.
- `POST /api/projects/{id}/tasks/{taskId}/run` — Start task execution (immediate).
- `POST /api/projects/{id}/tasks/{taskId}/enqueue` — Queue task execution in per-project FIFO.
- `POST /api/projects/{id}/push` — Synchronize source to remote (`{ "clean": false, "keepCrlf": false }`).
- `POST /api/projects/{id}/pull` — Pull remote build artifacts to host managed directory.

### 5. Batches & Workflows

- `GET /api/projects/{id}/batches` — List batch pipelines in a project.
- `POST /api/projects/{id}/batches` — Save/create a batch workflow.
- `PATCH /api/projects/{id}/batches/{batchId}` — Update an existing batch.
- `DELETE /api/projects/{id}/batches/{batchId}` — Delete a batch.
- `POST /api/projects/{id}/batches/{batchId}/run` — Execute batch workflow (immediate).
- `POST /api/projects/{id}/batches/{batchId}/enqueue` — Queue batch workflow in per-project FIFO.
- `GET /api/workflows` — List cross-project workflow presets.
- `GET /api/workflows/{id}/raw` — Read raw workflow JSON.
- `PUT /api/workflows/{id}` — Save/update workflow preset (`{ "raw": "<json>" }`).
- `DELETE /api/workflows/{id}` — Delete workflow preset file.
- `POST /api/workflows/reset` — Restore missing built-in presets.
- `POST /api/projects/{id}/workflows/{preset}/apply` — Instantiate workflow template into project.

### 6. Build Manifests

- `GET /api/manifests` — List build type manifests and validation status.
- `GET /api/manifests/{id}/raw` — Read raw manifest JSON.
- `PUT /api/manifests/{id}` — Save/update manifest (`{ "raw": "<json>" }`).
- `DELETE /api/manifests/{id}` — Delete manifest file.
- `POST /api/manifests/reload` — Re-read `manifest/` directory from disk and report diff.
- `POST /api/manifests/detect` — Detect matching manifests for a host directory.

### 7. Jobs & Logs

- `GET /api/jobs` — List recent 50 jobs with status summaries.
- `GET /api/jobs/{id}` — Get job execution details, outputs, and step-by-step breakdown.
- `GET /api/jobs/{id}/logs` — Fetch log lines for a specific job (`?since=<line-number>`).
- `POST /api/jobs/{id}/cancel` — Cancel an active or pending job.
- `POST /api/jobs/{id}/retry` — Retry a failed job (`{ "fromStep": N }` or `{ "fromStart": true }`).
- `GET /api/logs/days` — List archived log dates.
- `GET /api/logs/date/{date}` — Read archived log records (`?source=cli&scope=...&limit=200`).
- `GET /api/logs/client/{id}` — Read live stream logs for a client (`?since=<line-number>`).

### 8. Native Dialogs

- `POST /api/dialog/pick-file` — Open native OS file picker on daemon machine (returns path).
- `POST /api/dialog/pick-dir` — Open native OS directory picker on daemon machine.

← [Home](Home)
