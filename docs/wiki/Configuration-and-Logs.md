# Configuration and Logs

CompileGateway is configured via a single JSON file (`config.json`) and writes structured,
daily-rotating logs.

## `config.json`

The file is stored next to the executable (fallback to current working directory if not found).
If absent on first launch, a default `config.json` is generated automatically.

```json
{
  "port": 0,
  "language": "auto",
  "listen": "lan",
  "auth": {
    "username": "",
    "passwordHash": "",
    "loginIntervalSeconds": 60
  },
  "logRetainDays": 7,
  "allowedRoots": [
    "F:/Projects"
  ],
  "hostLock": false,
  "mcp": {
    "enabled": true,
    "listen": "lan",
    "port": 3994
  }
}
```

### Configuration Fields Reference

| Field | Type | Default | Description |
|---|---|---|---|
| `port` | `number` | `0` | Daemon HTTP port. `0` picks an available port dynamically; fixed numbers fail immediately if occupied. **Requires restart.** |
| `listen` | `string` | `"lan"` | Listen scope: `"lan"` (127.0.0.1 loopback only) or `"wan"` (0.0.0.0 all interfaces). **Requires restart.** |
| `language` | `string` | `"auto"` | UI and log language: `"auto"` (system locale), `"zh-Hans"` or `"en-US"`. Takes effect immediately. |
| `auth.username` | `string` | `""` | Login username for non-loopback requests. |
| `auth.passwordHash`| `string` | `""` | Argon2id password hash string. Plaintext is never stored. |
| `auth.loginIntervalSeconds` | `number` | `60` | Minimum interval between consecutive failed logins from the same IP (rate limit). |
| `logRetainDays` | `number` | `7` | Days to keep archived log files before automatic cleanup (1–3650). |
| `allowedRoots` | `string[]` | `[]` | Whitelist of absolute host directories allowed for project browsing and registration. |
| `hostLock` | `boolean` | `false` | Host-level mutual exclusion: when `true`, only one job runs at a time per client machine across all projects. |
| `mcp.enabled` | `boolean` | `true` | Enable or disable the native MCP server. |
| `mcp.listen` | `string` | `"lan"` | Listen scope for MCP (`"lan"` or `"wan"`). Independent of daemon `listen`. |
| `mcp.port` | `number` | `3994` | Dedicated port for MCP. Independent of main `port`. **Hot-reloads immediately.** |

## Ways to Change Configuration

### 1. WebUI Settings Page
Open Settings from the navigation sidebar. Form updates are validated and submitted via `PATCH /api/config`.

### 2. Command Line (`cgw set` / `cgw config`)

The CLI provides categorized `cgw set` subcommands:

```bash
cgw set port 65251                      # changes main port (needs restart)
cgw set listen wan                      # changes listen mode (needs restart)
cgw set user alice                      # prompts/updates admin user
cgw set language zh-Hans                # immediate
cgw set login-interval 30               # immediate
cgw set log-retain 14                   # immediate
cgw set roots add /srv/builds           # immediate
cgw set host-lock on                    # immediate
cgw set mcp port 3995                   # immediate hot-reload
cgw set mcp listen wan                  # immediate hot-reload
```

Or pass JSON patches via `cgw config set`:

```bash
cgw config set --body '{"hostLock": true, "logRetainDays": 14}'
```

### 3. MCP (`cgw_set_config`)
AI agents update settings using the structured tool call `cgw_set_config`.

### 4. Direct JSON Editing
Edit `config.json` while the daemon is stopped, or edit dynamic fields and reload. Note: password
must be hashed with Argon2id; editing plaintext password into the file will not work (use
`cgw set user` or WebUI instead).

## The Runtime Pointer (`runtime.json`)

When the daemon starts, it writes an ephemeral `runtime.json` file to `CGW_HOME`
(`~/.compilegateway`) and next to the executable:

```json
{
  "pid": 12345,
  "url": "http://127.0.0.1:65251",
  "mcpUrl": "http://127.0.0.1:3994/mcp",
  "version": "0.1.0",
  "startedAt": 1775184000
}
```

CLI clients and the MCP stdio bridge read this file to discover the live daemon without manual
configuration.

## Logging System

Logs are written to the `logs/` directory next to `config.json`.

- **Daily rotation:** Named by host local date: `logs/YYYY-MM-DD.log`.
- **Single-line format:** Each record is a single tab-separated line:
  ```text
  <ISO8601 Timestamp>\t<LEVEL>\t<SCOPE>\t<SOURCE>\t<MESSAGE>
  ```
  Embedded newlines and tabs within log messages are safely escaped.
- **Source tracking:** Captured via HTTP request header `X-Client: webui | cli | mcp | daemon`.
- **Scope isolation:**
  - Global daemon events: `global`
  - Per-client SSH stream: `client-<id>`
  - Per-job execution stream: `job-<id>`

### Viewing and Querying Logs

```bash
# Real-time streaming log for a client
cgw logs client c-mac --since 0

# Query archived log for a specific date
cgw logs 2026-01-31 --source cli --limit 100

# Inspect available log days
cgw logs days
```

Expired log files older than `logRetainDays` are pruned automatically upon date rollover or
configuration changes.

← [Home](Home)
