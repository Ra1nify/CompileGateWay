# Troubleshooting

This guide lists common error scenarios, their root causes, and explicit remediation steps.

---

## 1. Connection & Host Key Issues

### Host Key Mismatch (TOFU)
- **Symptom:** `cgw clients test` or a task fails reporting host key mismatch.
- **Root Cause:** Trust On First Use (TOFU) recorded a fingerprint on initial connection. Rebuilding a VM, reinstalling the target OS, or changing SSH host keys altered the server's public key.
- **Fix:**
  - Inspect the presented key in the error message or WebUI dialog.
  - Trust the new key explicitly:
    ```bash
    cgw clients hostkey <clientId> --trust <SHA256:...>
    ```
  - Or forget the old fingerprint to re-prompt on next connection:
    ```bash
    cgw clients hostkey <clientId> --forget
    ```

### Private Key Rejected ("Unsupported legacy encrypted PEM")
- **Symptom:** Daemon reports that encrypted PEM with AES-CBC is not supported.
- **Root Cause:** Legacy OpenSSL PEM encryption (`EVP_BytesToKey` with AES-CBC) is intentionally unsupported.
- **Fix:** Convert the private key to standard PKCS#8 format using OpenSSL:
  ```bash
  openssl pkcs8 -topk8 -v2 aes-256-cbc -in old.key -out new.key
  ```

---

## 2. Build & Synchronization Issues

### `chmod: gradlew: No such file or directory` / Remote directory is empty
- **Symptom:** First build on a newly registered project fails immediately at the first step.
- **Root Cause:** A `build` task performs **only** the remote build; it intentionally **does not auto-push** source code.
- **Fix:**
  - Push source code first:
    ```bash
    cgw push <project> --wait
    ```
  - Or use the recommended `push-build-pull` workflow preset:
    ```bash
    cgw workflows apply <project> push-build-pull --apply --run --wait
    ```

### `/bin/sh^M: bad interpreter` or `\r: command not found`
- **Symptom:** Shell script or Gradle wrapper fails on remote Unix host.
- **Root Cause:** Windows CRLF (`\r\n`) line endings in executable scripts.
- **Fix:** CompileGateway automatically normalizes scripts containing a shebang (`#!`) to LF on `push`. If you passed `--keep-crlf`, remove that flag and re-push:
  ```bash
  cgw push <project> --wait
  ```

### `artifactMissing` (HTTP 500)
- **Symptom:** Build steps all exited with code 0, but the job status is marked `failed` with code `artifactMissing`.
- **Root Cause:** The manifest or task declared an expected output file or directory (`artifactPath` or `outputs`), but the remote build did not generate a file at that path.
- **Fix:**
  - Inspect the actual remote output paths using `cgw tree <project> build --side remote`.
  - Adjust the `artifactPath` or `variables` in your task/manifest to match the real output location.

---

## 3. Concurrency & Queue Conflicts

### `409 projectBusy` (CLI Exit Code 4)
- **Symptom:** Starting a task or batch fails with HTTP 409 `projectBusy`.
- **Root Cause:** The project already has an active or pending job holding the project-level lock.
- **Fix:**
  - Wait for the active job to finish, or inspect it:
    ```bash
    cgw doctor
    cgw jobs
    ```
  - Queue the request in the per-project FIFO:
    ```bash
    cgw run <project> <task> --enqueue
    cgw batch run <project> <batch> --enqueue
    ```
  - Or cancel the stuck job:
    ```bash
    cgw jobs cancel <jobId>
    ```

### `409 hostBusy`
- **Symptom:** Starting a task fails with HTTP 409 `hostBusy`.
- **Root Cause:** Global `hostLock` is enabled in `config.json`, and another project is currently executing a job on the same SSH client.
- **Fix:** Use `--enqueue` to wait for the host to free up, or disable `hostLock` if parallel builds on that machine are safe:
  ```bash
  cgw set host-lock off
  ```

---

## 4. Port & Service Issues

### Port Already in Use on Startup
- **Symptom:** Daemon fails to start, logging that port binding failed.
- **Root Cause:** A fixed `port` in `config.json` is occupied by another process.
- **Fix:**
  - Edit `config.json` and set `"port": 0` to let the system select an available port automatically, or assign a different unused port number.

### MCP Port Conflict
- **Symptom:** Log warns that default MCP port 3994 is occupied.
- **Behavior:** Daemon automatically falls back to an available dynamic port. Inspect the actual active URL with:
  ```bash
  cgw mcp url
  ```

---

## 5. Security & Authentication

### `403 outsideRoot`
- **Symptom:** Browsing directories or registering a project returns 403 `outsideRoot`.
- **Root Cause:** The requested host path is outside the user home directory and not included in `allowedRoots`.
- **Fix:** Add the host directory to the whitelist:
  ```bash
  cgw set roots add "D:/MyProjects"
  ```

### `429 authRateLimited`
- **Symptom:** Repeated login attempts return HTTP 429.
- **Root Cause:** Login protection enforces a cooldown interval per IP (`loginIntervalSeconds`).
- **Fix:** Wait for the `retryAfterSeconds` duration returned in the error response before retrying.

### Resetting Forgotten Password
- **Rule:** CompileGateway credentials can **only** be modified from the local loopback interface (`127.0.0.1` / `::1`), which is always exempt from authentication.
- **Fix:** Log in directly on the machine running the daemon and update the credentials:
  ```bash
  cgw set user alice
  ```

← [Home](Home)
