//! HTTP server for the WebUI (axum): static assets + status/config/login/client/project/build APIs.
//!
//! The port is bound by `app::bootstrap` first, so this only hands the ready listener to axum,
//! avoiding a race between probing and listening.
//!
//! **No Tauri here**: the native file picker is injected via [`SharedDialog`]; headless passes `None`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{self, Session, COOKIE_NAME, SESSION_TTL};
use crate::clients::{self, AuthMethod, Client, ClientStore};
use crate::config::{Config, Listen};
use crate::contract;
use crate::workflow;
use crate::dialog::{DialogDone, DialogKind, SharedDialog};
use crate::env;
use crate::fileops;
use crate::i18n::{self, Lang};
use crate::jobs;
use crate::jsonschema;
use crate::known_hosts;
use crate::log;
use crate::logfile;
use crate::manifest::{self, SigningMode};
use crate::paths::{self, Location, Side};
use crate::projects::{self, Batch, BatchStep, Project, ProjectStore, Task};
use crate::ssh::{self, Cancel};
use crate::sync;
use crate::tree;
use rust_embed::RustEmbed;
use std::collections::BTreeMap;

// ------------------------------------------------------------------ shared state

pub struct WebState {
    pub config_path: PathBuf,
    pub config: RwLock<Config>,
    pub lang: RwLock<Lang>,
    pub port: u16,
    pub url: RwLock<String>,
    /// The listen mode actually bound (config changes need a restart, so warnings use the live value).
    pub bound_listen: Listen,
    pub sessions: Mutex<HashMap<String, Session>>,
    /// Login rate limit: source IP -> last attempt time.
    pub attempts: Mutex<HashMap<IpAddr, Instant>>,
    /// Clients (SSH targets), persisted in clients.json.
    pub clients_path: PathBuf,
    pub clients: RwLock<Vec<Client>>,
    /// Registered projects, persisted in projects.json.
    pub projects_path: PathBuf,
    pub projects: RwLock<Vec<Project>>,
    /// Async job registry (§10.1); also enforces the per-project lock of §7.5.
    pub jobs: jobs::Registry,
    /// Cached environment reports (client id -> report). Detection needs a round trip to the
    /// target, so `POST .../env` stores it here and `GET` reads it back.
    pub env_cache: RwLock<HashMap<String, env::EnvReport>>,
    /// Native file picker. `None` when headless; the API then reports an honest error.
    pub dialog: Option<SharedDialog>,
    /// Runtime of the built-in MCP adapter (second port, see `crate::mcp`).
    pub mcp: crate::mcp::McpRuntime,
    /// Last seen manifest-directory fingerprint (used to report diffs on refresh).
    pub manifest_fingerprints: RwLock<manifest::Snapshot>,
    /// Runs waiting in the queue (§7.6.6). Keyed by jobId; ordering comes from the pending
    /// records in `jobs::Registry`, so only "how to run" is stored here.
    queued: Mutex<HashMap<String, QueuedRun>>,
}



impl WebState {
    pub fn new(
        config_path: PathBuf,
        config: Config,
        lang: Lang,
        port: u16,
        url: String,
        dialog: Option<SharedDialog>,
    ) -> Self {
        let bound_listen = config.listen;
        let clients_path = clients::store_path(&config_path);
        let store = ClientStore::load(&clients_path);
        let projects_path = projects::store_path(&config_path);
        let project_store = ProjectStore::load(&projects_path);
        let queue_path = queue_store_path(&config_path);
        let stored_queued = load_queued_runs(&queue_path);
        let active_ids: Vec<String> = stored_queued.keys().cloned().collect();
        // §12.1: recent job state survives a daemon restart, so attach the store and read history back
        let jobs = jobs::Registry::default();
        jobs.attach_store(jobs::store_path(&config_path));
        jobs.restore_with_active_queue(&active_ids);

        let mut queued_map = HashMap::new();
        for (job_id, stored) in stored_queued {
            let is_pending = jobs
                .get(&job_id)
                .and_then(|j| j.get("state").and_then(|s| s.as_str().map(|st| st == "pending")))
                .unwrap_or(false);
            if is_pending {
                let cancel = jobs
                    .cancel_flag(&job_id)
                    .map(Cancel::from_flag)
                    .unwrap_or_default();
                let plan = match stored.plan {
                    StoredQueuedPlan::Task { task } => QueuedPlan::Task(Box::new(TaskRun {
                        task,
                        job_id: job_id.clone(),
                        cancel,
                    })),
                    StoredQueuedPlan::Batch {
                        batch,
                        from_step,
                        snapshot,
                        produced,
                    } => QueuedPlan::Batch(Box::new(BatchRun {
                        batch,
                        from_step,
                        job_id: job_id.clone(),
                        cancel,
                        snapshot,
                        produced,
                    })),
                };
                queued_map.insert(
                    job_id,
                    QueuedRun {
                        project_id: stored.project_id,
                        client_id: stored.client_id,
                        plan,
                    },
                );
            }
        }
        save_queued_runs(&queue_path, &snapshot_queued(&queued_map));

        // Snapshot the disk once at startup, so the first refresh reports what changed since
        // boot instead of flagging all four templates as added.
        let manifest_fingerprints = RwLock::new(manifest::snapshot(&config_path));
        WebState {
            config_path,
            config: RwLock::new(config),
            lang: RwLock::new(lang),
            port,
            url: RwLock::new(url),
            bound_listen,
            sessions: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            clients_path,
            clients: RwLock::new(store.clients),
            projects_path,
            projects: RwLock::new(project_store.projects),
            jobs,
            env_cache: RwLock::new(HashMap::new()),
            dialog,
            mcp: crate::mcp::McpRuntime::new(),
            manifest_fingerprints,
            queued: Mutex::new(queued_map),
        }
    }

    /// Last seen manifest-directory fingerprint.
    pub fn manifest_snapshot(&self) -> manifest::Snapshot {
        crate::lock::read(&self.manifest_fingerprints).clone()
    }

    /// Host directories we may touch (the home directory plus `allowedRoots`).
    ///
    /// **Computed every call**: config changes must take effect at once; it only reads a few attributes.
    pub fn host_roots(&self) -> paths::HostRoots {
        let extra = crate::lock::read(&self.config).allowed_roots.clone();
        paths::HostRoots::resolve(&extra)
    }

    pub fn set_manifest_snapshot(&self, snapshot: manifest::Snapshot) {
        *crate::lock::write(&self.manifest_fingerprints) = snapshot;
    }

    /// Current MCP adapter status (for the status and config pages).
    pub fn mcp_status(&self) -> crate::mcp::Status {
        self.mcp.status(&crate::lock::read(&self.config).mcp)
    }

    /// MCP enabled on wan without credentials: the same persistent warning as the daemon itself.
    pub fn mcp_wan_without_auth(&self) -> bool {
        let config = crate::lock::read(&self.config);
        config.mcp.enabled && config.mcp.listen.allows_remote() && !config.auth.is_configured()
    }

    /// Cached environment report, if any.
    pub fn cached_env(&self, client_id: &str) -> Option<env::EnvReport> {
        crate::lock::read(&self.env_cache).get(client_id).cloned()
    }

    /// Grab a project's execution slot and register a running job (§7.5 + §10.1).
    /// Returns a structured busy error when taken. Production paths use [`Self::start_job_sourced`].
    #[cfg(test)]
    fn start_job(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
    ) -> Result<jobs::JobHandle, ApiError> {
        self.start_job_sourced(project_id, client_id, kind, title, None, log::SOURCE_DAEMON)
    }

    /// Host-level mutual exclusion lock check for a specific client.
    /// Returns true if either the global `config.host_lock` is on or this client has `host_lock` enabled.
    pub fn is_host_locked_for_client(&self, client_id: &str) -> bool {
        if crate::lock::read(&self.config).host_lock {
            return true;
        }
        crate::lock::read(&self.clients)
            .iter()
            .find(|c| c.id == client_id)
            .map(|c| c.host_lock)
            .unwrap_or(false)
    }

    /// As above, but records the batch and the request source (the `X-Client` of §10).
    fn start_job_sourced(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
        batch_id: Option<String>,
        source: &str,
    ) -> Result<jobs::JobHandle, ApiError> {
        // Host-level lock can be global (config.host_lock) or per-client (client.host_lock)
        let host_lock = self.is_host_locked_for_client(client_id);
        self.jobs
            .start_sourced(
                project_id,
                client_id,
                kind,
                title,
                jobs::StartOptions {
                    batch_id,
                    source,
                    host_lock,
                },
            )
            .map_err(|busy| {
                let t = i18n::text(self.lang());
                if busy.host {
                    // Another project on the same client blocks us; name it so the caller knows
                    // both switching hosts and queueing are valid ways out.
                    return ApiError::host_busy(
                        client_id,
                        &busy.holder_project_id,
                        &busy.job_id,
                        busy.kind,
                        busy.running_seconds,
                        t.host_busy
                            .replace("{project}", &busy.holder_project_id)
                            .replace("{kind}", busy.kind)
                            .replace("{job}", &busy.job_id),
                    );
                }
                ApiError::busy(
                    project_id,
                    &busy.job_id,
                    busy.kind,
                    busy.running_seconds,
                    t.project_busy
                        .replace("{kind}", busy.kind)
                        .replace("{job}", &busy.job_id),
                )
            })
    }

    /// Project-level tasks currently running (for the UI).
    pub fn running_json(&self) -> serde_json::Value {
        self.jobs.running_json()
    }

    /// Queued tasks (for the UI, §7.6.6).
    pub fn queued_json(&self) -> serde_json::Value {
        self.jobs.queued_json()
    }

    fn persist_queued(&self) {
        let path = queue_store_path(&self.config_path);
        // Convert to the serializable form under the lock, then serialize/write after
        // releasing it (S11): `save_queued_runs` emits a sizable document, so holding the
        // lock through it would stall every queue mutation.
        let snapshot = snapshot_queued(&crate::lock::lock(&self.queued));
        save_queued_runs(&path, &snapshot);
    }

    /// Store a queued run.
    fn enqueue_run(&self, run: QueuedRun) {
        let job_id = match &run.plan {
            QueuedPlan::Task(task) => task.job_id.clone(),
            QueuedPlan::Batch(batch) => batch.job_id.clone(),
        };
        crate::lock::lock(&self.queued).insert(job_id, run);
        self.persist_queued();
    }

    /// Take a queued run's plan (called after it is promoted to running).
    fn take_queued(&self, job_id: &str) -> Option<QueuedRun> {
        let run = crate::lock::lock(&self.queued).remove(job_id);
        if run.is_some() {
            self.persist_queued();
        }
        run
    }

    /// Drop a queued run (cancelled, or the queue record is gone).
    fn drop_queued(&self, job_id: &str) -> bool {
        let removed = crate::lock::lock(&self.queued).remove(job_id).is_some();
        if removed {
            self.persist_queued();
        }
        removed
    }

    pub fn clients_snapshot(&self) -> Vec<Client> {
        crate::lock::read(&self.clients).clone()
    }

    /// Write the current list back to clients.json (caller must release the write lock first).
    pub fn persist_clients(&self) -> std::io::Result<()> {
        let store = ClientStore {
            clients: self.clients_snapshot(),
        };
        store.save(&self.clients_path)
    }

    pub fn projects_snapshot(&self) -> Vec<Project> {
        crate::lock::read(&self.projects).clone()
    }

    pub fn persist_projects(&self) -> std::io::Result<()> {
        let store = ProjectStore {
            projects: self.projects_snapshot(),
        };
        store.save(&self.projects_path)
    }

    pub fn lang(&self) -> Lang {
        *crate::lock::read(&self.lang)
    }

    pub fn url(&self) -> String {
        crate::lock::read(&self.url).clone()
    }

    /// wan mode with no credentials configured: keep warning.
    pub fn wan_without_auth(&self) -> bool {
        let config = crate::lock::read(&self.config);
        self.bound_listen.allows_remote() && !config.auth.is_configured()
    }
}

/// Build the tokio runtime shared by the WebUI and MCP listeners.
///
/// Both listeners run on the same machine and share the same blocking work (packing, unpacking,
/// host-side tree walks). Defaults are chosen for a small always-on box rather than a desktop:
///
/// * `worker_threads` — the default (= logical CPUs) starves HTTP handlers on a 2-core host as
///   soon as a couple of jobs run; a floor of 4 keeps WebUI/MCP responsive.
/// * `max_blocking_threads` — the default is 512, but this crate routes blocking IO through
///   `spawn_blocking` (archive packing, `rm -rf`, tree walks). 32 is far above what one daemon
///   needs while keeping the thread count bounded.
pub fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .max(4);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .max_blocking_threads(32)
        .enable_all()
        .build()
}

/// Start the HTTP server on its own thread and tokio runtime.
pub fn spawn(listener: std::net::TcpListener, state: Arc<WebState>) -> std::io::Result<()> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("cgw-http".to_string())
        .spawn(move || {
            let runtime = match build_runtime() {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("[ERROR] tokio runtime: {err}");
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(err) => {
                        eprintln!("[ERROR] tokio listener: {err}");
                        return;
                    }
                };
                let app = router(state);
                let serve = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                );
                if let Err(err) = serve.await {
                    eprintln!("[ERROR] http server: {err}");
                }
            });
        })?;
    Ok(())
}

fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/api/status", get(api_status))
        .route("/api/config", get(api_get_config).post(api_set_config))
        .route("/api/mcp", get(api_mcp_status))
        .route("/api/mcp/tools", get(api_mcp_tools))
        .route("/api/login", post(api_login))
        .route("/api/logout", post(api_logout))
        .route("/api/clients", get(api_list_clients).post(api_save_client))
        .route("/api/clients/{id}", axum::routing::delete(api_delete_client))
        .route("/api/clients/{id}/test", post(api_test_client))
        .route("/api/clients/{id}/logs", get(api_client_logs))
        .route("/api/clients/{id}/basedir", post(api_set_base_dir))
        .route("/api/clients/{id}/hostkey", post(api_set_host_key))
        .route(
            "/api/clients/{id}/hostkey/import",
            post(api_import_known_hosts),
        )
        .route("/api/clients/{id}/known-hosts", get(api_export_known_hosts))
        .route(
            "/api/clients/{id}/env",
            get(api_get_env).post(api_detect_env),
        )
        .route("/api/env/script", get(api_get_env_script))
        .route("/api/projects", get(api_list_projects).post(api_create_project))
        .route(
            "/api/projects/{id}",
            axum::routing::delete(api_delete_project),
        )
        .route("/api/projects/{id}/push", post(api_push_project))
        .route("/api/projects/{id}/pull", post(api_pull_project))
        .route("/api/projects/{id}/export", post(api_export_project))
        .route("/api/projects/{id}/exports", get(api_list_exports))
        .route("/api/projects/{id}/exports/{name}", get(api_download_export))
        .route("/api/projects/{id}/tree", get(api_project_tree))
        .route("/api/projects/{id}/file", get(api_project_file))
        .route("/api/projects/{id}/tasks", get(api_list_tasks).post(api_save_task))
        .route(
            "/api/projects/{id}/tasks/{task_id}",
            axum::routing::delete(api_delete_task).patch(api_patch_task),
        )
        .route(
            "/api/projects/{id}/tasks/{task_id}/preview",
            post(api_preview_task),
        )
        .route("/api/projects/{id}/tasks/{task_id}/run", post(api_run_task))
        .route(
            "/api/projects/{id}/tasks/{task_id}/enqueue",
            post(api_enqueue_task),
        )
        .route(
            "/api/projects/{id}/batches",
            get(api_list_batches).post(api_save_batch),
        )
        .route(
            "/api/projects/{id}/batches/{batch_id}",
            axum::routing::delete(api_delete_batch).patch(api_patch_batch),
        )
        .route(
            "/api/projects/{id}/batches/{batch_id}/run",
            post(api_run_batch),
        )
        .route(
            "/api/projects/{id}/batches/{batch_id}/enqueue",
            post(api_enqueue_batch),
        )
        .route("/api/jobs", get(api_list_jobs).delete(api_clear_jobs))
        .route("/api/jobs/clear", post(api_clear_jobs))
        .route("/api/jobs/{job_id}", get(api_get_job))
        .route("/api/jobs/{job_id}/logs", get(api_job_logs))
        .route("/api/jobs/{job_id}/cancel", post(api_cancel_job))
        .route("/api/jobs/{job_id}/retry", post(api_retry_job))
        .route("/api/logs/days", get(api_log_days))
        .route("/api/logs/tail", get(api_log_tail))
        .route("/api/logs/{day}", get(api_log_day))
        .route("/api/browse", get(api_browse))
        .route("/api/dialog/pick-folder", post(api_pick_folder))
        .route("/api/dialog/pick-file", post(api_pick_file))
        .route("/api/manifests", get(api_list_manifests))
        .route("/api/schema", get(api_schema_list))
        .route("/api/schema/{name}", get(api_schema_named))
        .route("/api/workflows", get(api_list_workflows))
        .route("/api/workflows/templates", get(api_workflow_templates))
        .route("/api/workflows/reset", post(api_workflow_reset))
        .route("/api/workflows/{id}/raw", get(api_workflow_raw))
        .route(
            "/api/workflows/{id}",
            axum::routing::delete(api_workflow_delete).put(api_workflow_put),
        )
        .route(
            "/api/projects/{id}/workflows/{preset}/apply",
            post(api_apply_workflow),
        )
        .route("/api/manifests/templates", get(api_manifest_templates))
        // Legacy path kept: scripts and agents already use it
        .route("/api/manifests/presets", get(api_manifest_templates))
        .route("/api/manifests/reload", post(api_manifest_reload))
        .route("/api/manifests/reset", post(api_manifest_reset))
        .route("/api/manifests/detect", post(api_manifest_detect))
        .route("/api/manifests/{id}/raw", get(api_manifest_raw))
        .route(
            "/api/manifests/{id}",
            axum::routing::delete(api_manifest_delete).put(api_manifest_put),
        )
        .with_state(state)
        .fallback(static_assets)
}

// ------------------------------------------------------------------ static assets

/// WebUI build output (`webui-src` builds to `webui/dist` with Vite), embedded at compile time.
#[derive(RustEmbed)]
#[folder = "../webui/dist"]
struct WebAssets;

async fn static_assets(uri: axum::http::Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    // An unmatched API path is a 404; never fall back to the frontend page
    if raw.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }

    let path = if raw.is_empty() { "index.html" } else { raw };
    match WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

// ------------------------------------------------------------------ access control

/// §4.1: loopback passes; other sources need login once credentials exist. The user explicitly
/// asked that lan / wan not force credentials, so remote sources also pass when none are set.
enum Access {
    Local,
    Anonymous,
    Authed,
    Denied,
}

fn access(state: &WebState, headers: &HeaderMap, addr: SocketAddr) -> Access {
    if addr.ip().is_loopback() {
        return Access::Local;
    }
    let configured = crate::lock::read(&state.config).auth.is_configured();
    if !configured {
        return Access::Anonymous;
    }
    match session_user(state, headers) {
        Some(_) => Access::Authed,
        None => Access::Denied,
    }
}

fn session_user(state: &WebState, headers: &HeaderMap) -> Option<String> {
    let token = cookie_value(headers, COOKIE_NAME)?;
    let mut sessions = crate::lock::lock(&state.sessions);
    sessions.retain(|_, s| s.alive());
    sessions
        .get(&token)
        .filter(|s| s.alive())
        .map(|s| s.username.clone())
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

fn is_authorized(access: &Access) -> bool {
    !matches!(access, Access::Denied)
}

// ------------------------------------------------------------------ errors

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    extra: Option<Value>,
}

impl ApiError {
    fn unauthorized() -> Self {
        ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "login required".to_string(),
            extra: None,
        }
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
            extra: None,
        }
    }

    fn internal(message: &str) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: message.to_string(),
            extra: None,
        }
    }

    fn rate_limited(retry_after_seconds: u64) -> Self {
        ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rateLimited",
            message: format!("retry after {retry_after_seconds}s"),
            extra: Some(json!({ "retryAfterSeconds": retry_after_seconds })),
        }
    }

    fn not_found(code: &'static str) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            code,
            message: "not found".to_string(),
            extra: None,
        }
    }

    /// Like [`ApiError::not_found`] but with a readable message (the UI shows it verbatim).
    fn not_found_message(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            code,
            message: message.into(),
            extra: None,
        }
    }

    fn forbidden(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::FORBIDDEN,
            code,
            message: message.into(),
            extra: None,
        }
    }

    /// Something on the target side failed (SSH, timeout, remote command).
    ///
    /// Not a server bug, so 502 rather than 500, letting callers tell request / daemon / target apart.
    fn upstream(code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_GATEWAY,
            code,
            message: message.into(),
            extra: None,
        }
    }

    /// Project busy (§7.5): carries projectId / jobId / suggested retry time.
    fn busy(
        project_id: &str,
        job_id: &str,
        kind: &str,
        running_seconds: u64,
        message: String,
    ) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            code: "projectBusy",
            message,
            extra: Some(json!({
                "projectId": project_id,
                "jobId": job_id,
                "kind": kind,
                "runningSeconds": running_seconds,
                "retryAfterSeconds": 10,
            })),
        }
    }

    /// **Host-level** busy (§7.6.6): another project is running on the same client.
    ///
    /// A separate code from `projectBusy` because the remedy differs: switching hosts or queueing both work.
    fn host_busy(
        client_id: &str,
        holder_project_id: &str,
        job_id: &str,
        kind: &str,
        running_seconds: u64,
        message: String,
    ) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            code: "hostBusy",
            message,
            extra: Some(json!({
                "clientId": client_id,
                "projectId": holder_project_id,
                "jobId": job_id,
                "kind": kind,
                "runningSeconds": running_seconds,
                "retryAfterSeconds": 10,
            })),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": { "code": self.code, "message": self.message }
        });
        if let Some(extra) = self.extra {
            if let Some(obj) = body["error"].as_object_mut() {
                if let Some(extra) = extra.as_object() {
                    for (k, v) in extra {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (self.status, Json(body)).into_response()
    }
}

// ------------------------------------------------------------------ API

async fn api_status(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Json<Value> {
    let access = access(&state, &headers, addr);
    let config = crate::lock::read(&state.config).clone();
    let lang = state.lang();
    let t = i18n::text(lang);

    let username = match &access {
        Access::Local | Access::Authed => config.auth.username.clone(),
        _ => String::new(),
    };

    Json(json!({
        "app": t.app_name,
        "version": env!("CARGO_PKG_VERSION"),
        "hello": t.hello,
        "pageNote": t.page_note,
        "port": state.port,
        "listen": config.listen.code(),
        "url": state.url(),
        "language": lang.code(),
        "accent": crate::theme::accent_color(),
        "configPath": state.config_path.display().to_string(),
        "auth": {
            "required": config.auth.is_configured(),
            "username": username,
        },
        "client": {
            "loopback": addr.ip().is_loopback(),
            "authenticated": matches!(access, Access::Authed) || matches!(access, Access::Local),
        },
        "warning": if state.wan_without_auth() { json!("wanWithoutAuth") } else { Value::Null },
        "mcp": state.mcp.status(&config.mcp).to_json(),
        "mcpWarning": if state.mcp_wan_without_auth() { json!("mcpWanWithoutAuth") } else { Value::Null },
        "restartNote": {
            "port": state.port,
            "listen": config.listen.code(),
        }
    }))
}

async fn api_get_config(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    if !is_authorized(&access(&state, &headers, addr)) {
        return Err(ApiError::unauthorized());
    }
    let config = crate::lock::read(&state.config).clone();
    Ok(Json(config_json(&state, &config)))
}

fn config_json(state: &WebState, config: &Config) -> Value {
    let mcp = state.mcp.status(&config.mcp);
    json!({
        "port": config.port.unwrap_or(0),
        "language": config.language,
        "listen": config.listen.code(),
        "username": config.auth.username,
        "hasPassword": !config.auth.password_hash.is_empty(),
        "loginIntervalSeconds": config.auth.login_interval_seconds,
        "logRetainDays": config.log_retain_days,
        "allowedRoots": config.allowed_roots,
        "hostLock": config.host_lock,
        "hostRoots": state
            .host_roots()
            .all()
            .iter()
            .map(|root| paths::display(root))
            .collect::<Vec<_>>(),
        "mcp": mcp.to_json(),
        "logDir": logfile::dir_for(&state.config_path).display().to_string(),
        "configPath": state.config_path.display().to_string(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigPatch {
    port: Option<u16>,
    language: Option<String>,
    listen: Option<String>,
    username: Option<String>,
    /// Default = unchanged; empty string = clear; non-empty = set a new password.
    password: Option<String>,
    /// Login rate-limit interval in seconds; 0 = no limit. Values above the cap clamp to 24 hours.
    login_interval_seconds: Option<u64>,
    /// Log retention days (design doc §10).
    log_retain_days: Option<u64>,
    /// Built-in MCP adapter (takes effect immediately, no restart).
    mcp: Option<McpPatch>,
    /// Host directories allowed besides the user home (immediate; **full replacement**).
    allowed_roots: Option<Vec<String>>,
    /// **Add/remove** entries in the allow-list (leaves the other entries alone).
    ///
    /// Unlike allowedRoots (full replacement), concurrent add/remove edits never clobber each other --
    /// this is what cgw set roots add/rm uses.
    roots_add: Option<Vec<String>>,
    roots_remove: Option<Vec<String>>,
    /// Host-level mutex: one job at a time per client machine (design doc §7.6.6 end, **immediate**).
    host_lock: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpPatch {
    enabled: Option<bool>,
    listen: Option<String>,
    port: Option<u16>,
}

/// Clean allow-list input: trim whitespace and drop empty strings.
fn clean_roots(roots: &[String]) -> Vec<String> {
    roots
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}
async fn api_set_config(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(patch): Json<ConfigPatch>,
) -> Result<Json<Value>, ApiError> {
    if !is_authorized(&access(&state, &headers, addr)) {
        return Err(ApiError::unauthorized());
    }

    // Design doc §4.4: credentials may only be changed from the local machine. A logged-in remote
    // session can change port/language/listen mode but **cannot** touch username and password.
    let touches_auth = patch.username.is_some() || patch.password.is_some();
    if touches_auth && !addr.ip().is_loopback() {
        let t = i18n::text(state.lang());
        return Err(ApiError::forbidden("authLocalOnly", t.auth_local_only));
    }

    let mut restart_required = false;
    let (lang, listen_code);
    {
        let mut config = crate::lock::write(&state.config);
        // Edit a copy first, then persist and commit: if the MCP entry cannot bind its port,
        // no other field is left half-applied (live in memory but not written to disk).
        let mut next = config.clone();

        if let Some(port) = patch.port {
            if next.port.unwrap_or(0) != port {
                next.port = Some(port);
                restart_required = true;
            }
        }

        if let Some(raw) = patch.listen.as_deref() {
            let parsed = Listen::parse(raw)
                .ok_or_else(|| ApiError::bad_request("invalidListen", raw.to_string()))?;
            if parsed != next.listen {
                next.listen = parsed;
                restart_required = true;
            }
        }

        if let Some(language) = patch.language.as_deref() {
            next.language = language.trim().to_string();
        }

        // Immediate (re-read on every login); no restart needed.
        if let Some(seconds) = patch.login_interval_seconds {
            next.auth.login_interval_seconds =
                seconds.min(crate::auth::MAX_LOGIN_INTERVAL.as_secs());
        }

        // Immediate: also update the file sink's retention days and purge expired files.
        if let Some(days) = patch.log_retain_days {
            next.log_retain_days = logfile::clamp_days(days);
            if let Some(file) = log::core().file() {
                file.set_retain_days(next.log_retain_days);
            }
        }

        if let Some(username) = patch.username.as_deref() {
            next.auth.username = username.trim().to_string();
        }

        match patch.password.as_deref() {
            None => {}
            Some("") => next.auth.password_hash.clear(),
            Some(password) => {
                next.auth.password_hash =
                    auth::hash_password(password).map_err(|e| ApiError::internal(&e))?;
            }
        }

        // An empty username means incomplete credentials: clear the hash instead of leaving half a set.
        if next.auth.username.is_empty() {
            next.auth.password_hash.clear();
        }

        // Immediate: the next browse / project registration / detection uses the new roots.
        if let Some(roots) = patch.allowed_roots.as_ref() {
            next.allowed_roots = clean_roots(roots);
        }
        // Add/remove form used by cgw set roots add/rm (touches nothing else, no read-modify-write race).
        if let Some(roots) = patch.roots_add.as_ref() {
            for root in clean_roots(roots) {
                if !next.allowed_roots.iter().any(|item| item.eq_ignore_ascii_case(&root)) {
                    next.allowed_roots.push(root);
                }
            }
        }
        if let Some(roots) = patch.roots_remove.as_ref() {
            let drop: Vec<String> = clean_roots(roots);
            next.allowed_roots.retain(|item| {
                !drop.iter().any(|candidate| candidate.eq_ignore_ascii_case(item))
            });
        }

        // Host-level mutex: immediate (applied when the next job starts or the queue advances).
        if let Some(host_lock) = patch.host_lock {
            next.host_lock = host_lock;
        }

        // ---- MCP adapter: immediate (the port can change without restarting the process) ---------
        if let Some(mcp) = patch.mcp.as_ref() {
            if let Some(enabled) = mcp.enabled {
                next.mcp.enabled = enabled;
            }
            if let Some(raw) = mcp.listen.as_deref() {
                let parsed = Listen::parse(raw)
                    .ok_or_else(|| ApiError::bad_request("invalidListen", raw.to_string()))?;
                next.mcp.listen = parsed;
            }
            if let Some(port) = mcp.port {
                next.mcp.port = Some(port);
            }
        }
        if next.mcp != config.mcp {
            state
                .mcp
                .apply(&next.mcp, &state, state.port)
                .map_err(|err| ApiError::bad_request("mcpBindFailed", err))?;
        }

        *config = next;
        config
            .save_to(&state.config_path)
            .map_err(|e| ApiError::internal(&e.to_string()))?;

        lang = Lang::resolve(&config.language).0;
        listen_code = config.listen.code().to_string();
    }

    // The language may have changed: sync it to the CLI window and the WebUI.
    *crate::lock::write(&state.lang) = lang;
    log::core().set_lang(lang, Lang::resolve(&crate::lock::read(&state.config).language).1);
    // Keep the process-wide language in step (see `i18n::current`).
    i18n::set_current(lang);
    let t = i18n::text(lang);
    log::log(
        log::INFO,
        format!("{} (listen={listen_code})", t.config_updated),
    );

    Ok(Json(json!({
        "ok": true,
        "restartRequired": restart_required,
        "config": config_json(&state, &crate::lock::read(&state.config)),
    })))
}

/// MCP adapter status (design doc §12 Phase 6). Read by the CLI's `cgw mcp`.
async fn api_mcp_status(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    if !is_authorized(&access(&state, &headers, addr)) {
        return Err(ApiError::unauthorized());
    }
    let config = crate::lock::read(&state.config).clone();
    let mut body = state.mcp.status(&config.mcp).to_json();
    body["warning"] = if state.mcp_wan_without_auth() {
        json!("mcpWanWithoutAuth")
    } else {
        Value::Null
    };
    Ok(Json(body))
}

/// Tool list -- so "the CLI sees exactly what the agent sees".
async fn api_mcp_tools(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    if !is_authorized(&access(&state, &headers, addr)) {
        return Err(ApiError::unauthorized());
    }
    let tools: Vec<Value> = crate::mcp::tools()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect();
    Ok(Json(json!({
        "count": tools.len(),
        "protocolVersions": crate::mcp::PROTOCOL_VERSIONS,
        "tools": tools,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginBody {
    username: String,
    password: String,
}async fn api_login(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<Response, ApiError> {
    let (username, hash, interval) = {
        let config = crate::lock::read(&state.config);
        (
            config.auth.username.clone(),
            config.auth.password_hash.clone(),
            config.auth.login_interval(),
        )
    };

    // Per-source-IP rate limiting (design doc §4.3). The interval comes from
    // auth.loginIntervalSeconds; 0 explicitly disables rate limiting.
    if !interval.is_zero() {
        let mut attempts = crate::lock::lock(&state.attempts);
        attempts.retain(|_, at| at.elapsed() < interval);
        if let Some(last) = attempts.get(&addr.ip()) {
            let wait = interval.saturating_sub(last.elapsed()).as_secs() + 1;
            return Err(ApiError::rate_limited(wait));
        }
        attempts.insert(addr.ip(), Instant::now());
    }

    if hash.is_empty() || username.trim().is_empty() {
        return Err(ApiError::bad_request(
            "authDisabled",
            "no credentials configured",
        ));
    }

    if body.username.trim() != username.as_str() || !auth::verify_password(&body.password, &hash) {
        return Err(ApiError::unauthorized());
    }

    let token = auth::random_token();
    crate::lock::lock(&state.sessions).insert(
        token.clone(),
        Session {
            username,
            expires: Instant::now() + SESSION_TTL,
        },
    );

    // Design doc §4.2: public access goes through a self-hosted reverse proxy, and the daemon
    // never terminates TLS itself. So TLS can only be inferred from the proxy header
    // X-Forwarded-Proto -- mark the session cookie Secure when it says https.
    let secure = is_https_request(&headers);
    let cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax{}; Max-Age={}",
        if secure { "; Secure" } else { "" },
        SESSION_TTL.as_secs()
    );
    let mut response = Json(json!({ "ok": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

/// Whether this request came over HTTPS (direct connections are http, so only a proxy can say so).
///
/// **Only `X-Forwarded-Proto` is trusted**: it is the reverse proxy's convention header. There is
/// no "trust any source" fallback -- a client exposed directly on the internet can forge this
/// header, but that deployment is out of scope (design doc §4.2 requires a self-hosted proxy).
fn is_https_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            // The proxy chain may say `https, http`; take the first entry.
            value
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("https")
        })
        .unwrap_or(false)
}

async fn api_logout(State(state): State<Arc<WebState>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie_value(&headers, COOKIE_NAME) {
        crate::lock::lock(&state.sessions).remove(&token);
    }
    let clear = format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax{}; Max-Age=0",
        if is_https_request(&headers) { "; Secure" } else { "" }
    );
    let mut response = Json(json!({ "ok": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&clear) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

// ------------------------------------------------------------------ clients (SSH targets)

fn authorized(state: &WebState, headers: &HeaderMap, addr: SocketAddr) -> Result<(), ApiError> {
    if is_authorized(&access(state, headers, addr)) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

async fn api_list_clients(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let clients: Vec<Value> = state.clients_snapshot().iter().map(Client::to_json).collect();
    Ok(Json(json!({
        "clients": clients,
        "path": state.clients_path.display().to_string(),
    })))
}

/// The `auth` of `POST /api/clients`. Unlike `clients::ClientAuth`, here
/// `password` / `passphrase` are `Option`: absent = unchanged, empty string = clear.
///
/// `pub(crate)` lets `crate::jsonschema` generate the client schema from it, so
/// "what the API accepts" and "what docs/MCP tools describe" share one struct.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientAuthInput {
    /// `password` | `key`. Empty is treated as `password`.
    #[serde(default)]
    method: String,
    /// Login username; empty uses the current local username.
    #[serde(default)]
    username: String,
    /// Default = unchanged; empty string = clear; non-empty = set a new password.
    #[serde(default)]
    password: Option<String>,
    /// Private key file path (used when `method=key`).
    #[serde(default)]
    key_path: String,
    /// Private key passphrase, same semantics as `password` (default = unchanged, empty = clear).
    /// `ssh-keygen` encrypts keys by default, so this is the common case.
    #[serde(default)]
    passphrase: Option<String>,
}

/// Request body of `POST /api/clients`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientInput {
    /// Empty = create; an existing id = update that entry.
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    host: String,
    /// SSH port; empty/0 is treated as 22.
    #[serde(default)]
    port: u16,
    auth: ClientAuthInput,
    #[serde(default)]
    tags: Vec<String>,
    /// Base directory for sources/artifacts on this machine; empty uses `~/CompileGateway`.
    #[serde(default)]
    base_dir: Option<String>,
    /// Host-level mutual exclusion lock switch for this specific client machine.
    #[serde(default)]
    host_lock: Option<bool>,
}

async fn api_save_client(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ClientInput>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let method = AuthMethod::parse(&input.auth.method)
        .ok_or_else(|| ApiError::bad_request("invalidAuthMethod", input.auth.method.clone()))?;
    let host = input.host.trim().to_string();
    if host.is_empty() {
        return Err(ApiError::bad_request("hostRequired", "host is required"));
    }
    let name = {
        let name = input.name.trim();
        if name.is_empty() {
            host.clone()
        } else {
            name.to_string()
        }
    };
    let port = if input.port == 0 { 22 } else { input.port };
    let tags: Vec<String> = input
        .tags
        .into_iter()
        .map(|tag| tag.trim().to_string())
        .filter(|tag| !tag.is_empty())
        .collect();

    let saved = {
        let mut list = crate::lock::write(&state.clients);
        let index = list.iter().position(|c| c.id == input.id && !input.id.is_empty());

        // Passwords are never returned by the API, so "not sent" means keep the old one.
        let password = match input.auth.password.as_deref() {
            None => index
                .map(|i| list[i].auth.password.clone())
                .unwrap_or_default(),
            Some("") => String::new(),
            Some(value) => value.to_string(),
        };
        // Same for the passphrase: never returned, so not sent means keep it.
        let passphrase = match input.auth.passphrase.as_deref() {
            None => index
                .map(|i| list[i].auth.passphrase.clone())
                .unwrap_or_default(),
            Some("") => String::new(),
            Some(value) => value.to_string(),
        };
        let (id, host_key, host_key_line) = match index {
            Some(i) => (
                list[i].id.clone(),
                list[i].host_key.clone(),
                list[i].host_key_line.clone(),
            ),
            None => (clients::new_id(), String::new(), String::new()),
        };

        let base_dir = input
            .base_dir
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| clients::DEFAULT_BASE_DIR.to_string());
        // The base dir changed, so the previously resolved absolute path is void.
        let base_dir_resolved = match index {
            Some(i) if list[i].base_dir == base_dir => list[i].base_dir_resolved.clone(),
            _ => String::new(),
        };

        let host_lock = match input.host_lock {
            Some(val) => val,
            None => index.map(|i| list[i].host_lock).unwrap_or(false),
        };

        let client = Client {
            id,
            name,
            host,
            port,
            auth: clients::ClientAuth {
                method,
                username: input.auth.username.trim().to_string(),
                password,
                key_path: input.auth.key_path.trim().to_string(),
                passphrase,
            },
            tags,
            base_dir,
            base_dir_resolved,
            host_key,
            host_key_line,
            host_lock,
        };

        match index {
            Some(i) => list[i] = client.clone(),
            None => list.push(client.clone()),
        }
        client
    };

    state
        .persist_clients()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {}", t.client_saved, saved.name));

    Ok(Json(json!({ "ok": true, "client": saved.to_json() })))
}

async fn api_delete_client(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let removed = {
        let mut list = crate::lock::write(&state.clients);
        match list.iter().position(|c| c.id == id) {
            Some(i) => Some(list.remove(i)),
            None => None,
        }
    };
    let removed = removed.ok_or_else(|| ApiError::not_found("clientNotFound"))?;

    state
        .persist_clients()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {}", t.client_deleted, removed.name));

    Ok(Json(json!({ "ok": true })))
}

async fn api_test_client(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let client = state
        .clients_snapshot()
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::not_found("clientNotFound"))?;

    let result = ssh::probe(&client, Duration::from_secs(15)).await;

    // First connection remembers the host fingerprint (TOFU).
    if result.ok {
        if let Some(fingerprint) = &result.host_key {
            if client.host_key != *fingerprint {
                {
                    let mut list = crate::lock::write(&state.clients);
                    if let Some(entry) = list.iter_mut().find(|c| c.id == client.id) {
                        entry.host_key = fingerprint.clone();
                        // Also keep the raw public key line -- needed to export known_hosts.
                        entry.host_key_line = result.host_key_line.clone().unwrap_or_default();
                    }
                }
                let _ = state.persist_clients();
            }
        }
    }

    let t = i18n::text(state.lang());
    log::log(
        if result.ok { log::INFO } else { log::WARN },
        format!("{}: {} - {}", t.client_tested, client.name, result.message),
    );
    // This is the scope shown by the instance window / WebUI instance log page.
    log::log_scoped(
        &clients::log_scope(&client.id),
        if result.ok { log::INFO } else { log::WARN },
        format!("{} - {}", result.message, result.output.trim()),
    );

    Ok(Json(json!({
        "ok": result.ok,
        "message": result.message,
        "output": result.output,
        "hostKey": result.host_key,
        // The key actually presented on mismatch -- the UI offers "trust new fingerprint" from it.
        "presentedKey": result.presented_key,
        "mismatch": result.mismatch,
    })))
}

#[derive(Deserialize)]
struct LogsQuery {
    #[serde(default)]
    since: u64,
}

/// Instance logs (used by the WebUI log page; the instance CLI window gets the same data via Tauri IPC).
async fn api_client_logs(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let exists = state.clients_snapshot().iter().any(|c| c.id == id);
    if !exists {
        return Err(ApiError::not_found("clientNotFound"));
    }

    let scope = clients::log_scope(&id);
    let entries: Vec<_> = log::core()
        .snapshot(&scope)
        .into_iter()
        .filter(|entry| entry.seq > query.since)
        .collect();

    Ok(Json(json!({ "entries": entries })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PickFileBody {
    title: Option<String>,
}

/// Calls the shell-injected native file picker (Windows / Linux / macOS).
/// It goes through the daemon because the WebUI is a browser page with no local capabilities,
/// and the path is later used by the daemon to read the private key, so it should be picked on
/// the daemon machine. Headless environments have no dialog implementation, so this errors out.
async fn api_pick_file(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PickFileBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let t = i18n::text(state.lang());
    let title = body.title.unwrap_or_else(|| t.dialog_pick_key.to_string());
    let path = pick(&state, DialogKind::File, title).await?;
    Ok(Json(json!({ "path": path })))
}

/// Unified dialog call: bridges the trait callback into async.
async fn pick(
    state: &WebState,
    kind: DialogKind,
    title: String,
) -> Result<Option<String>, ApiError> {
    let Some(dialog) = state.dialog.clone() else {
        // Headless daemon: no window, so no dialog can be shown.
        return Err(ApiError::internal(
            "no native dialog in this environment (headless daemon)",
        ));
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let done: DialogDone = Box::new(move |picked| {
        let _ = tx.send(picked);
    });
    dialog.pick(kind, title, done);

    rx.await
        .map_err(|_| ApiError::internal("dialog closed unexpectedly"))
}

// ------------------------------------------------------------------ base directory

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BaseDirBody {
    base_dir: String,
    /// Whether to connect and verify with `mkdir -p` (default: verify).
    #[serde(default = "default_true")]
    verify: bool,
}

fn default_true() -> bool {
    true
}

async fn api_set_base_dir(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<BaseDirBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let base_dir = body.base_dir.trim().to_string();
    if base_dir.is_empty() {
        return Err(ApiError::bad_request("baseDirRequired", "base dir is required"));
    }

    let mut client = state
        .clients_snapshot()
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::not_found("clientNotFound"))?;

    let scope = clients::log_scope(&id);
    let mut resolved = String::new();
    let mut message = String::new();

    if body.verify {
        let mut probe_client = client.clone();
        probe_client.base_dir = base_dir.clone();
        let result = ssh::ensure_base_dir(&probe_client, &base_dir, Duration::from_secs(20)).await;

        if let Some(fingerprint) = result.host_key.clone() {
            if client.host_key != fingerprint {
                client.host_key = fingerprint;
                client.host_key_line = result.host_key_line.clone().unwrap_or_default();
            } else if let Some(line) = result.host_key_line.clone().filter(|line| !line.is_empty()) {
                client.host_key_line = line;
            }
        }

        log::log_scoped(
            &scope,
            if result.ok { log::INFO } else { log::WARN },
            format!("{} - {}", result.message, result.output.trim()),
        );

        if !result.ok {
            // Do not persist a failed verification, so an unreachable base dir is never stored.
            return Ok(Json(json!({
                "ok": false,
                "message": if result.output.trim().is_empty() {
                    result.message
                } else {
                    format!("{} ({})", result.message, result.output.trim())
                },
            })));
        }
        resolved = result.resolved.unwrap_or_default();
        message = result.message;
    }

    client.base_dir = base_dir;
    client.base_dir_resolved = resolved.clone();
    let saved = client.clone();
    {
        let mut list = crate::lock::write(&state.clients);
        if let Some(entry) = list.iter_mut().find(|c| c.id == saved.id) {
            *entry = saved.clone();
        }
    }
    state
        .persist_clients()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(
        log::INFO,
        format!("{}: {} -> {}", t.base_dir_updated, saved.name, resolved),
    );

    Ok(Json(json!({
        "ok": true,
        "message": if message.is_empty() { resolved.clone() } else { message },
        "client": saved.to_json(),
    })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct HostKeyBody {
    /// Fingerprint to trust (usually the one presented by a test connection).
    /// Empty = clear the record (the next connection goes through first trust again).
    fingerprint: String,
    /// Optional: the raw public key line for this key. If given it is stored (needed by
    /// `known_hosts` export); when the fingerprint changes without a new line the old one is
    /// cleared too -- the previous key's line must not hang under the new fingerprint.
    key_line: Option<String>,
}

/// `POST /api/clients/{id}/hostkey` -- manage the recorded host fingerprint.
///
/// Why this endpoint exists: refusing on a changed fingerprint is right (TOFU), but having
/// **no way out** is not -- rebuilding a macOS VM, reinstalling, or swapping machines all
/// change host keys routinely. Previously `clients.json` had to be edited by hand; now:
///   * with `fingerprint` -> trust it (overwrite the record);
///   * without it -> clear the record, so the next connection is treated as first trust.
///
/// This is **security sensitive** (it waives MITM protection once), so it demands explicit
/// intent: the caller must name the fingerprint to trust; no casual one-click pass-through.
async fn api_set_host_key(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<HostKeyBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let fingerprint = body.fingerprint.trim().to_string();
    // Fingerprint shape check: reject accidental blanks and obviously wrong values.
    if !fingerprint.is_empty()
        && !(fingerprint.starts_with("SHA256:") && fingerprint.len() > 10)
    {
        return Err(ApiError::bad_request(
            "badFingerprint",
            format!("指纹应当形如 SHA256:xxxx,收到的是 {fingerprint}"),
        ));
    }

    let updated = {
        let mut list = crate::lock::write(&state.clients);
        let entry = list
            .iter_mut()
            .find(|client| client.id == id)
            .ok_or_else(|| ApiError::not_found("clientNotFound"))?;
        let previous = entry.host_key.clone();
        entry.host_key = fingerprint.clone();
        match body.key_line.as_deref().map(str::trim) {
            // A key line was given, so use it.
            Some(line) if !line.is_empty() => entry.host_key_line = line.to_string(),
            // New fingerprint without a key line: the old line belongs to the old key and must go.
            _ if previous != fingerprint => entry.host_key_line = String::new(),
            _ => {}
        }
        (entry.clone(), previous)
    };

    state
        .persist_clients()
        .map_err(|err| ApiError::internal(&err.to_string()))?;

    let t = i18n::text(state.lang());
    let (client, previous) = updated;
    let scope = clients::log_scope(&client.id);
    if fingerprint.is_empty() {
        log::log_scoped(
            &scope,
            log::WARN,
            format!("{}: {previous}", t.host_key_forgotten),
        );
    } else {
        log::log_scoped(
            &scope,
            log::WARN,
            format!("{}: {fingerprint}", t.host_key_trusted),
        );
    }

    Ok(Json(json!({
        "ok": true,
        "client": client.to_json(),
        "previous": previous,
    })))
}

// ------------------------------------------------------------------ known_hosts import / export

/// Maximum size of a `known_hosts` file. These are plain text, a few KB at most;
/// the cap only stops a stray few-hundred-MB file from stalling the daemon.
const KNOWN_HOSTS_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct KnownHostsImportBody {
    /// File contents (read by the CLI first -- the file may live on the **user's** machine).
    raw: String,
    /// Path for the daemon to read itself (used by the WebUI, whose picker browses the daemon machine).
    path: String,
    /// Whether to allow replacing a **different** key already recorded.
    force: bool,
}

/// `POST /api/clients/{id}/hostkey/import` -- set this machine's key from a `known_hosts` file.
///
/// This is offline pre-trust: no need to connect first and copy the fingerprint by hand.
/// Three non-negotiables (all security related):
///
/// * **reject on any `@revoked` match** -- that key is explicitly revoked;
/// * **never guess when the file holds more than one key** -- list the candidates and let the caller pick;
/// * **replacing an existing record requires explicit `force`** -- otherwise it is a silent MITM swap.
async fn api_import_known_hosts(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<KnownHostsImportBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let client = find_client(&state, &id)?;

    let text = if !body.raw.trim().is_empty() {
        body.raw.clone()
    } else if !body.path.trim().is_empty() {
        let path = std::path::PathBuf::from(body.path.trim());
        let meta = std::fs::metadata(&path)
            .map_err(|err| ApiError::bad_request("fileUnreadable", format!("{}: {err}", path.display())))?;
        if meta.len() > KNOWN_HOSTS_MAX_BYTES {
            return Err(ApiError::bad_request(
                "fileTooLarge",
                format!("{} 有 {} 字节,超过上限", path.display(), meta.len()),
            ));
        }
        std::fs::read_to_string(&path)
            .map_err(|err| ApiError::bad_request("fileUnreadable", format!("{}: {err}", path.display())))?
    } else {
        return Err(ApiError::bad_request(
            "knownHostsEmpty",
            "要么给 raw(文件内容),要么给 path(让 daemon 去读)",
        ));
    };

    let parsed = known_hosts::parse(&text);
    let found = known_hosts::lookup(&parsed, &client.host, client.port);

    let mut notes = found.notes.clone();

    if let Some(revoked) = found.revoked.first() {
        return Err(ApiError::bad_request(
            "hostKeyRevoked",
            format!(
                "文件里第 {} 行把 {} 标记成 @revoked(已吊销),不能拿它当主机钥匙。指纹:{}",
                revoked.line,
                revoked.patterns,
                revoked.fingerprint
            ),
        ));
    }
    if found.candidates.is_empty() {
        if !found.authorities.is_empty() {
            notes.push(
                "命中的是 @cert-authority(给主机签证书的 CA),不是主机钥匙;本工具不支持主机证书"
                    .to_string(),
            );
        }
        let mut message = format!(
            "在文件里没找到 {}:{} 的钥匙({} 行读进来 {} 条记录,{} 行看不懂)",
            client.host,
            client.port,
            text.lines().count(),
            parsed.entries.len(),
            parsed.problems.len()
        );
        if !notes.is_empty() {
            message.push_str(";");
            message.push_str(&notes.join(";"));
        }
        return Err(ApiError::bad_request("hostKeyNotFound", message));
    }
    if !found.agrees() {
        return Err(ApiError::bad_request(
            "hostKeyAmbiguous",
            format!(
                "文件里有 {} 把不同的钥匙都能对上 {}:{} —— 这种情况不猜,请你确认用哪一把:{}",
                found.fingerprints().len(),
                client.host,
                client.port,
                found.fingerprints().join(" / ")
            ),
        ));
    }

    let entry = found.candidates[0];
    let key_type = entry.key.algorithm().as_str().to_string();
    let already = client.host_key == entry.fingerprint;
    if !already && !client.host_key.is_empty() && !body.force {
        return Err(ApiError::bad_request(
            "hostKeyMismatch",
            format!(
                "记录里已经有一把不同的钥匙,文件里是另一把 —— 直接替换等于把中间人防护让掉一次。\
                 确认要换的话加 --force(CLI)/ force(接口)。记录:{} 文件:{}",
                client.host_key, entry.fingerprint
            ),
        ));
    }

    let changed = !already
        || client.host_key_line != entry.key_line;
    if changed {
        let mut list = crate::lock::write(&state.clients);
        if let Some(target) = list.iter_mut().find(|item| item.id == id) {
            target.host_key = entry.fingerprint.clone();
            target.host_key_line = entry.key_line.clone();
        }
        drop(list);
        state
            .persist_clients()
            .map_err(|err| ApiError::internal(&err.to_string()))?;
        // Security-relevant actions are always logged (same as trusting/clearing a fingerprint).
        log::log_scoped(
            &clients::log_scope(&id),
            log::WARN,
            format!(
                "从 known_hosts 导入主机钥匙:第 {} 行 {} {}",
                entry.line,
                key_type,
                entry.fingerprint
            ),
        );
    }

    let client = find_client(&state, &id)?;
    Ok(Json(json!({
        "ok": true,
        "alreadyTrusted": already,
        "line": entry.line,
        "keyType": key_type,
        "fingerprint": entry.fingerprint,
        "keyLine": entry.key_line,
        "notes": notes,
        "problems": parsed.problems,
        "client": client.to_json(),
    })))
}

/// `GET /api/clients/{id}/known-hosts` -- export the recorded trust as one `known_hosts` line.
///
/// A fingerprint alone cannot render the standard format (it is a one-way digest), so:
/// with the raw key line we emit the standard line, otherwise a comment line on how to add it.
async fn api_export_known_hosts(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let client = find_client(&state, &id)?;

    if client.host_key.is_empty() {
        return Err(ApiError::bad_request(
            "hostKeyUnknown",
            format!(
                "{} 还没有记录任何主机钥匙 —— 先连一次(测试连接),或者从 known_hosts 导入",
                client.name
            ),
        ));
    }

    let has_line = !client.host_key_line.trim().is_empty();
    let line = if has_line {
        known_hosts::line_for(&client.host, client.port, &client.host_key_line)
    } else {
        known_hosts::comment_line_for(&client.host, client.port, &client.host_key)
    };
    let key_type = client
        .host_key_line
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    Ok(Json(json!({
        "ok": true,
        "hasKeyLine": has_line,
        "keyType": key_type,
        "fingerprint": client.host_key,
        "line": line,
        "warning": if has_line {
            Value::Null
        } else {
            json!("这条记录只有指纹,导不出标准 known_hosts 格式(指纹是单向摘要)。测试一次连接,或者用 --import 导入一份 known_hosts,就能补上公钥原文。")
        },
        "client": client.to_json(),
    })))
}

// ------------------------------------------------------------------ environment detection (design doc §8)

/// Reads the cache: returns `{ ok: true, report: null }` until detected, so the UI can prompt a run.
async fn api_get_env(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    if find_client(&state, &id).is_err() {
        return Err(ApiError::not_found("clientNotFound"));
    }
    Ok(Json(json!({
        "ok": true,
        "report": state.cached_env(&id),
    })))
}

/// Actually connects and runs detection, caching the report.
/// Returns the raw environment probe script without connecting to any client (CLI: `cgw env --script`).
async fn api_get_env_script(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    Ok(Json(json!({
        "ok": true,
        "script": env::probe_script(),
    })))
}

async fn api_detect_env(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let client = find_client(&state, &id)?;
    let t = i18n::text(state.lang());
    let scope = clients::log_scope(&client.id);

    log::log_scoped(&scope, log::INFO, t.env_started.to_string());

    let report = env::detect(&client, &Cancel::default()).await;

    log::log_scoped(
        &scope,
        if report.ok { log::INFO } else { log::WARN },
        format!("{}: {}", t.env_finished, report.message),
    );

    crate::lock::write(&state.env_cache)
        .insert(client.id.clone(), report.clone());

    Ok(Json(json!({ "ok": true, "report": report })))
}

// ------------------------------------------------------------------ project registration

#[derive(Deserialize)]
struct ProjectsQuery {
    #[serde(default)]
    client_id: Option<String>,
}

async fn api_list_projects(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<ProjectsQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let projects: Vec<Value> = state
        .projects_snapshot()
        .iter()
        .filter(|p| {
            query
                .client_id
                .as_deref()
                .map(|id| p.client_id == id)
                .unwrap_or(true)
        })
        .map(Project::to_json)
        .collect();

    Ok(Json(json!({
        "projects": projects,
        "path": state.projects_path.display().to_string(),
        // Running project-level tasks (§7.5); the UI disables buttons from this.
        "running": state.running_json(),
        // Queued tasks (§7.6.6): `projectId -> [{jobId, kind, title}]`, head of queue first.
        "queued": state.queued_json(),
    })))
}

/// Request body of `POST /api/projects`.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectInput {
    /// Owning client id.
    client_id: String,
    /// Source directory (absolute path) on the **daemon machine**.
    source_path: String,
    /// Project name; empty uses the last segment of the directory.
    #[serde(default)]
    name: Option<String>,
}

async fn api_create_project(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(input): Json<ProjectInput>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let source_path = input.source_path.trim().to_string();
    if source_path.is_empty() {
        return Err(ApiError::bad_request("sourceRequired", "source path is required"));
    }
    // Same root policy as browse / detect (design doc §6.1). This used to check nothing,
    // so browse could reject a path that this endpoint accepted (acceptance notes §7).
    let source_path = state
        .host_roots()
        .confine(std::path::Path::new(&source_path))
        .map(|path| paths::display(&path))
        .map_err(|err| match err {
            paths::PathError::Escape(message) => ApiError::forbidden("outsideRoot", message),
            paths::PathError::Syntax(message) => ApiError::bad_request("badPath", message),
        })?;
    if !std::path::Path::new(&source_path).is_dir() {
        return Err(ApiError::bad_request(
            "sourceMissing",
            format!("目录不存在:{source_path}"),
        ));
    }

    let client = state
        .clients_snapshot()
        .into_iter()
        .find(|c| c.id == input.client_id)
        .ok_or_else(|| ApiError::not_found("clientNotFound"))?;

    let name = input
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| projects::folder_name(&source_path));
    if name.is_empty() {
        return Err(ApiError::bad_request("nameRequired", "project name is required"));
    }

    let slug = projects::slug(&name);
    let remote_path = projects::join_remote(&client.base_dir, &slug);

    {
        let list = crate::lock::read(&state.projects);
        if list
            .iter()
            .any(|p| p.client_id == client.id && p.source_path == source_path)
        {
            return Err(ApiError::bad_request(
                "projectExists",
                "this folder is already registered on that client",
            ));
        }
    }

    let project = Project {
        id: projects::new_id(),
        name,
        client_id: client.id.clone(),
        source_path,
        remote_path: remote_path.clone(),
        excludes: projects::DEFAULT_EXCLUDES
            .iter()
            .map(|value| value.to_string())
            .collect(),
        added_at: projects::now_ms(),
        last_sync_at: 0,
        last_sync_ok: false,
        last_sync_message: String::new(),
        tasks: Vec::new(),
        batches: Vec::new(),
        last_build_at: 0,
        last_build_ok: false,
        last_build_message: String::new(),
        last_artifact: String::new(),
    };

    {
        let mut list = crate::lock::write(&state.projects);
        list.push(project.clone());
    }
    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(
        log::INFO,
        format!("{}: {} -> {remote_path}", t.project_added, project.name),
    );

    // Also create the remote directory; failure does not block registration (push re-creates it).
    let mut probe_client = client.clone();
    let result = ssh::run(
        &probe_client,
        &format!("mkdir -p {}", ssh::shell_path(&remote_path)),
        Duration::from_secs(15),
    )
    .await;
    let (created, note) = if result.ok {
        (true, t.project_dir_ready.to_string())
    } else {
        (false, format!("{} {}", t.project_dir_failed, result.message))
    };
    if let Some(fingerprint) = result.host_key {
        probe_client.host_key = fingerprint;
        let line = result.host_key_line.clone().unwrap_or_default();
        {
            let mut list = crate::lock::write(&state.clients);
            if let Some(entry) = list.iter_mut().find(|c| c.id == probe_client.id) {
                entry.host_key = probe_client.host_key.clone();
                entry.host_key_line = line;
            }
        }
        let _ = state.persist_clients();
    }

    log::log_scoped(
        &clients::log_scope(&client.id),
        if created { log::INFO } else { log::WARN },
        format!("{}: {} - {note}", t.project_added, project.name),
    );

    Ok(Json(json!({
        "ok": true,
        "project": project.to_json(),
        "remoteCreated": created,
        "message": note,
    })))
}

async fn api_delete_project(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let removed = {
        let mut list = crate::lock::write(&state.projects);
        match list.iter().position(|p| p.id == id) {
            Some(index) => Some(list.remove(index)),
            None => None,
        }
    };
    let removed = removed.ok_or_else(|| ApiError::not_found("projectNotFound"))?;

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {}", t.project_deleted, removed.name));

    Ok(Json(json!({ "ok": true })))
}

/// Pick a host folder (used to import a project).
async fn api_pick_folder(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<PickFileBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let t = i18n::text(state.lang());
    let title = body.title.unwrap_or_else(|| t.dialog_pick_folder.to_string());
    let path = pick(&state, DialogKind::Folder, title).await?;
    Ok(Json(json!({ "path": path })))
}

// ------------------------------------------------------------------ source sync

fn find_project(state: &WebState, id: &str) -> Result<Project, ApiError> {
    state
        .projects_snapshot()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| ApiError::not_found("projectNotFound"))
}

fn find_client(state: &WebState, id: &str) -> Result<Client, ApiError> {
    state
        .clients_snapshot()
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| ApiError::not_found("clientNotFound"))
}

/// The push excludes actually in effect:
///   the project's own excludes + the fixed never-push-local-artifacts list (§6.5)
///   + `cache.paths` from every manifest the project uses (§6.2: remote caches, never overwritten).
fn effective_excludes(state: &WebState, project: &Project) -> Vec<String> {
    let mut out: Vec<String> = project.excludes.clone();
    out.extend(sync::NEVER_PUSH.iter().map(|name| name.to_string()));
    for task in &project.tasks {
        if let Ok(parsed) = manifest::load(&state.config_path, &task.manifest) {
            out.extend(parsed.cache.paths);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Pull destination: `<home>/CompileGateway/<slug>` (symmetric with the remote convention).
fn host_artifacts_dir(project: &Project) -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("CompileGateway").join(projects::slug(&project.name))
}

/// Sync wrap-up: remember the host fingerprint, write back last_sync_*, log to the instance scope.
fn record_sync(
    state: &WebState,
    project_id: &str,
    client_id: &str,
    host_key: Option<ssh::HostKey>,
    ok: bool,
    message: &str,
    files: u64,
    bytes: u64,
) -> Option<Project> {
    if let Some(seen) = host_key {
        {
            let mut list = crate::lock::write(&state.clients);
            if let Some(entry) = list.iter_mut().find(|c| c.id == client_id) {
                if entry.host_key != seen.fingerprint {
                    // Key changed: the old key line belongs to the old key.
                    entry.host_key = seen.fingerprint;
                    entry.host_key_line = seen.line.unwrap_or_default();
                } else if let Some(line) = seen.line.filter(|line| !line.is_empty()) {
                    entry.host_key_line = line;
                }
            }
        }
        let _ = state.persist_clients();
    }

    let updated = {
        let mut list = crate::lock::write(&state.projects);
        let entry = list.iter_mut().find(|p| p.id == project_id)?;
        // **Only a successful sync advances `lastSyncAt`**: the `build` precheck asks
        // whether source was ever pushed, so a failure must not silence it. The failure
        // itself is still recorded in `lastSyncOk` / `lastSyncMessage`.
        if ok {
            entry.last_sync_at = projects::now_ms();
        }
        entry.last_sync_ok = ok;
        entry.last_sync_message = message.to_string();
        entry.clone()
    };
    let _ = state.persist_projects();

    let t = i18n::text(state.lang());
    log::log_scoped(
        &clients::log_scope(client_id),
        if ok { log::INFO } else { log::WARN },
        format!(
            "{}: {} - {} ({} / {})",
            if ok { t.sync_ok } else { t.sync_failed },
            updated.name,
            message,
            files,
            sync::human_bytes(bytes)
        ),
    );

    Some(updated)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PushBody {
    /// Delete the remote directory before unpacking (the design doc's "overwrite").
    clean: bool,
    /// Push as-is, without newline normalization.
    ///
    /// By default shebang scripts are normalized from CRLF to LF: a Windows-checked-out
    /// `gradlew` pushed straight to a Mac fails with `/bin/sh^M: no such file or directory`
    /// (acceptance notes §4.4). Binaries are never touched.
    keep_crlf: bool,
}

async fn api_push_project(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PushBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let source = PathBuf::from(&project.source_path);

    // Design doc §7.5 + §10.1: register a job and return at once, run in background.
    let handle = state.start_job_sourced(
        &project.id,
        &client.id,
        "push",
        "推送源码",
        None,
        &request_source(&headers),
    )?;
    let job_id = handle.id.clone();
    let cancel = Cancel::from_flag(handle.cancel);

    // Design doc §6.5 (host-local artifacts are never pushed) + §6.2 (cache.paths participate in excludes).
    let excludes = effective_excludes(&state, &project);
    let scopes = job_scopes(&client.id, &job_id);
    // Design doc §10: every log line this job writes carries its initiator (webui / cli / mcp).
    log::bind_sources(&scopes, &state.jobs.source(&job_id));
    let remote = project.remote_path.clone();
    let clean = body.clean;
    let keep_crlf = body.keep_crlf;
    let state2 = state.clone();
    let job_for_task = job_id.clone();
    let project_id = project.id.clone();
    let client_id = client.id.clone();
    let excludes_for_task = excludes.clone();
    let lang = state.lang();

    log::log_multi(&scopes, log::INFO, format!("推送开始 -> {remote}"));

    tokio::spawn(async move {
        let outcome = sync::push_with(
            &client,
            &source,
            &remote,
            &excludes_for_task,
            clean,
            keep_crlf,
            Duration::from_secs(900),
            &cancel,
            lang,
        )
        .await;

        let cancelled = outcome.cancelled || cancel.requested();
        let ok = outcome.ok && !cancelled;
        state2.jobs.finish(
            &job_for_task,
            ok,
            cancelled,
            &outcome.message,
            -1,
            &[],
            outcome.files,
            outcome.bytes,
        );
        log::log_multi(
            &scopes,
            if ok {
                log::INFO
            } else if cancelled {
                log::WARN
            } else {
                log::ERROR
            },
            format!("推送结束:{}", outcome.message),
        );
        record_sync(
            &state2,
            &project_id,
            &client_id,
            outcome.host_key.clone(),
            ok,
            &outcome.message,
            outcome.files,
            outcome.bytes,
        );
    });

    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "excludes": excludes,
        "project": project.to_json(),
    })))
}

// ------------------------------------------------------------------ export archive (design doc §9)

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ExportBody {
    /// `remote` (default) or `host`.
    source: String,
    /// Path relative to the project root; empty = the latest build artifact (same side).
    path: String,
    /// Output file name; empty = `<project name>-<unix seconds>.zip` (naming from §9).
    name: String,
}

/// The project's `exports/` directory (all exported archives land here).
fn host_exports_dir(project: &Project) -> PathBuf {
    host_artifacts_dir(project).join("exports")
}

/// `POST /api/projects/{id}/export` — the **Export shortcut** from design doc §9.
///
/// A synthesized "create or trigger one export task": never written into the project's task
/// list, runs once, lands at `<host managed dir>/exports/<name>.zip`, then served for
/// download by [`api_download_export`] (§9: "download of the packed archive only").
async fn api_export_project(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ExportBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let roots = roots_for(&project);

    let side = if body.source.trim().is_empty() {
        Side::Remote
    } else {
        Side::parse(&body.source).map_err(|err| ApiError::bad_request("badSource", err))?
    };

    // Path: when omitted, use the latest build artifact. **Error out instead of quietly
    // packing the whole project** — that can be tens of GB the user never asked to zip.
    let path = if body.path.trim().is_empty() {
        let last = project
            .last_artifact
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("");
        let stripped = last
            .strip_prefix("remote:")
            .or_else(|| last.strip_prefix("host:"))
            .unwrap_or(last);
        if stripped.is_empty() {
            return Err(ApiError::bad_request(
                "exportNeedsPath",
                "还没有构建产物可以打包;先跑一次构建,或者在文件树里挑一个路径再导出",
            ));
        }
        stripped.to_string()
    } else {
        body.path.trim().to_string()
    };
    // Validate the path syntax first, so a bad path is not discovered only after the job started.
    if paths::normalize_relative(&path)
        .map(|value| value.is_empty())
        .unwrap_or(true)
    {
        return Err(ApiError::bad_request(
            "exportNeedsPath",
            format!("导出的路径不合法(不能是项目根目录本身):{path}"),
        ));
    }

    let name = if body.name.trim().is_empty() {
        format!("{}-{}.zip", project.name.trim(), projects::now_ms() / 1000)
    } else {
        body.name.trim().to_string()
    };

    let handle = state.start_job_sourced(
        &project.id,
        &client.id,
        "export",
        "导出压缩包",
        None,
        &request_source(&headers),
    )?;
    let job_id = handle.id.clone();
    let cancel = Cancel::from_flag(handle.cancel);
    let scopes = job_scopes(&client.id, &job_id);
    log::bind_sources(&scopes, &state.jobs.source(&job_id));

    let source = Location { side, path: path.clone() };
    let name_for_job = name.clone();
    let state2 = state.clone();
    let job_for_task = job_id.clone();
    log::log_multi(
        &scopes,
        log::INFO,
        format!("导出开始:{}:{} -> {name}", side.code(), path),
    );

    tokio::spawn(async move {
        let outcome = fileops::export(
            &client,
            &roots,
            std::slice::from_ref(&source),
            &name_for_job,
            FILE_TIMEOUT,
            &cancel,
            &scopes,
        )
        .await;
        let cancelled = outcome.cancelled || cancel.requested();
        let ok = outcome.ok && !cancelled;
        // Artifact paths go into artifacts; the frontend builds download links from them.
        state2.jobs.finish(
            &job_for_task,
            ok,
            cancelled,
            &outcome.message,
            if ok { 0 } else { -1 },
            &outcome.outputs,
            outcome.files,
            outcome.bytes,
        );
        log::log_multi(
            &scopes,
            if ok {
                log::INFO
            } else if cancelled {
                log::WARN
            } else {
                log::ERROR
            },
            format!("导出结束:{}", outcome.message),
        );
    });

    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "name": name,
        "source": { "side": side.code(), "path": path },
        // Same shape as the push / pull shortcuts, so one frontend polling loop serves all.
        "project": project.to_json(),
    })))
}

/// `GET /api/projects/{id}/exports` — which archives under `exports/` can be downloaded.
async fn api_list_exports(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    let dir = host_exports_dir(&project);

    let mut items: Vec<(u64, Value)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_millis() as u64)
                .unwrap_or(0);
            items.push((
                modified,
                json!({
                    "name": entry.file_name().to_string_lossy(),
                    "bytes": meta.len(),
                    "modifiedAt": modified,
                }),
            ));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));

    Ok(Json(json!({
        "ok": true,
        "dir": dir.display().to_string(),
        "exports": items.into_iter().map(|(_, item)| item).collect::<Vec<_>>(),
    })))
}

/// `GET /api/projects/{id}/exports/{name}` — download one archive.
///
/// Files stay confined to `<host managed dir>/exports/` (`confine_local` normalizes and
/// checks for escapes), so `../` and friends cannot get out.
async fn api_download_export(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    let dir = host_exports_dir(&project);
    let path = paths::confine_local(&dir, &name)
        .map_err(|err| ApiError::bad_request("badExportName", err))?;
    if !path.is_file() {
        return Err(ApiError::not_found_message("exportNotFound", name));
    }
    let bytes = std::fs::read(&path).map_err(|err| ApiError::internal(&err.to_string()))?;

    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "export.zip".to_string());
    Ok((
        [
            (
                header::CONTENT_TYPE,
                if file_name.to_ascii_lowercase().ends_with(".tar.gz")
                    || file_name.to_ascii_lowercase().ends_with(".tgz")
                {
                    "application/gzip".to_string()
                } else {
                    "application/zip".to_string()
                },
            ),
            // File names may contain non-ASCII, so send both an ASCII fallback and RFC 5987 UTF-8
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"; filename*=UTF-8''{}",
                    ascii_fallback_name(&file_name),
                    percent_encode(&file_name)
                ),
            ),
            (header::CONTENT_LENGTH, bytes.len().to_string()),
        ],
        bytes,
    )
        .into_response())
}

/// The ASCII fallback file name inside `Content-Disposition`: non-ASCII becomes `_`.
fn ascii_fallback_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' { c } else { '_' })
        .collect();
    if cleaned.trim_matches('_').is_empty() {
        "export.zip".to_string()
    } else {
        cleaned
    }
}

/// RFC 5987 percent-encoding (keep `attr-char`, encode everything else byte-wise as UTF-8).
fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '!' | '#' | '$' | '&' | '+' | '-' | '.' | '^' | '_' | '`' | '|' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

async fn api_pull_project(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let target = host_artifacts_dir(&project);

    // Design doc §7.5 + §10.1
    let handle = state.start_job_sourced(
        &project.id,
        &client.id,
        "pull",
        "拉取产物",
        None,
        &request_source(&headers),
    )?;
    let job_id = handle.id.clone();
    let cancel = Cancel::from_flag(handle.cancel);
    let scopes = job_scopes(&client.id, &job_id);
    // Design doc §10: log source tagging
    log::bind_sources(&scopes, &state.jobs.source(&job_id));
    let remote = project.remote_path.clone();
    let state2 = state.clone();
    let job_for_task = job_id.clone();
    let project_id = project.id.clone();
    let client_id = client.id.clone();
    let target_for_task = target.clone();
    let lang = state.lang();

    log::log_multi(&scopes, log::INFO, format!("拉取开始 <- {remote}"));

    tokio::spawn(async move {
        let outcome = sync::pull(
            &client,
            &remote,
            &target_for_task,
            Duration::from_secs(900),
            &cancel,
            lang,
        )
        .await;

        let cancelled = outcome.cancelled || cancel.requested();
        let ok = outcome.ok && !cancelled;
        state2.jobs.finish(
            &job_for_task,
            ok,
            cancelled,
            &outcome.message,
            -1,
            &[],
            outcome.files,
            outcome.bytes,
        );
        log::log_multi(
            &scopes,
            if ok {
                log::INFO
            } else if cancelled {
                log::WARN
            } else {
                log::ERROR
            },
            format!("拉取结束:{}", outcome.message),
        );
        record_sync(
            &state2,
            &project_id,
            &client_id,
            outcome.host_key.clone(),
            ok,
            &outcome.message,
            outcome.files,
            outcome.bytes,
        );
    });

    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "target": target.display().to_string(),
        "project": project.to_json(),
    })))
}

// ------------------------------------------------------------------ project file tree (design doc §7.4)

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct TreeQuery {
    /// `remote` (default) or `host`
    side: String,
    /// Path relative to the project root; default = the root directory
    path: String,
}

/// `GET /api/projects/{id}/file?side=&path=` — controlled read of one file (the
/// "add a controlled read/download API later if text viewing is needed" of design doc §7.4).
///
/// Text only; binary returns metadata alone. Capped at [`tree::MAX_READ_BYTES`]; truncation reports `truncated`.
async fn api_project_file(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let side = paths::Side::parse(if query.side.trim().is_empty() {
        paths::SIDE_REMOTE
    } else {
        &query.side
    })
    .map_err(|err| ApiError::bad_request("badSide", err))?;

    let host_root = projects::host_root(&project);
    let file = tree::read(
        &client,
        &project.remote_path,
        &host_root,
        side,
        &query.path,
        &Cancel::default(),
    )
    .await
    .map_err(|err| match err.code {
        "badPath" | "escape" => ApiError::forbidden(err.code, err.message),
        "noRoot" | "noFile" | "notFile" => ApiError::not_found_message(err.code, err.message),
        "sshFailed" | "timeout" => ApiError::upstream(err.code, err.message),
        _ => ApiError::internal(&err.message),
    })?;

    Ok(Json(json!({
        "ok": true,
        "side": file.side,
        "path": file.path,
        "bytes": file.bytes,
        "size": file.size,
        "truncated": file.truncated,
        "binary": file.binary,
        "text": file.text,
    })))
}

/// Read-only, one-level directory listing. The remote side runs a **fixed script plus
/// positional arguments**; the host side uses `std::fs`.
async fn api_project_tree(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TreeQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let side = paths::Side::parse(if query.side.trim().is_empty() {
        paths::SIDE_REMOTE
    } else {
        &query.side
    })
    .map_err(|err| ApiError::bad_request("badSide", err))?;

    // The host-side root may not exist yet — listing it should say "no such directory"
    // rather than surface as an internal error.
    let host_root = projects::host_root(&project);
    let listing = tree::list(
        &client,
        &project.remote_path,
        &host_root,
        side,
        &query.path,
        &Cancel::default(),
    )
    .await
    .map_err(|err| match err.code {
        "badPath" => ApiError::forbidden("badPath", err.message),
        "escape" => ApiError::forbidden("escape", err.message),
        "noRoot" | "noDir" => ApiError::not_found(err.code),
        // SSH unreachable / timeout / remote command failure: the target machine's problem,
        // not the daemon's.
        "sshFailed" => ApiError::upstream("sshFailed", err.message),
        "timeout" => ApiError::upstream("timeout", err.message),
        _ => ApiError::internal(&err.message),
    })?;

    Ok(Json(json!({ "ok": true, "tree": listing })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct BrowseQuery {
    /// **Absolute** directory to browse; default = the user's home directory.
    path: String,
}

/// `GET /api/browse?path=<absolute path>` — list subfolders on the **machine running the daemon**.
///
/// Architectural hard constraint from design doc §6.1: the WebUI may be opened from another
/// device (a tablet, say), and the browser's native file picker selects files on *that*
/// device, not the one holding the source. So directory browsing must be served by the
/// daemon; a headless daemon has no native dialog either, making this the only workable path.
///
/// Boundary: browsing is confined to the **user home directory** (no `C:\Windows\System32`),
/// and relative paths / `..` / symlink escapes are all rejected.
///
/// v1 has no pagination (design doc §6.1: real source directories never have thousands of subfolders).
async fn api_browse(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    // Design doc §6.1: browse / project registration / build-type detection **share one
    // root policy**. The contradiction caught in the acceptance notes (browse rejecting F:
    // while create_project allowed it) came from each site writing its own check.
    let roots = state.host_roots();
    let root = roots
        .first()
        .ok_or_else(|| ApiError::internal("cannot determine the home directory"))?
        .to_path_buf();

    let requested = if query.path.trim().is_empty() {
        root.clone()
    } else {
        let raw = std::path::PathBuf::from(query.path.trim());
        // A user on Windows may end up with a `/`-prefixed form; let PathBuf decide.
        paths::confine_absolute(&root, &raw).or_else(|err| match err {
            // Outside the home directory but inside one of allowedRoots — allow it too
            paths::PathError::Escape(_) => roots.confine(&raw),
            other => Err(other),
        })
        .map_err(|err| match err {
            paths::PathError::Escape(message) => ApiError::forbidden("outsideRoot", message),
            paths::PathError::Syntax(message) => ApiError::bad_request("badPath", message),
        })?
    };

    let read = std::fs::read_dir(&requested)
        .map_err(|err| ApiError::not_found_message("noDir", format!("{}: {err}", requested.display())))?;

    let mut entries: Vec<Value> = Vec::new();
    for item in read.flatten() {
        // Directories only (design doc §6.1 asks for "which subfolders exist")
        let Ok(meta) = std::fs::metadata(item.path()) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().to_string();
        // Hidden directories (leading dot) are listed as usual — not rare in source trees,
        // but sorted last.
        entries.push(json!({
            "name": name,
            "path": paths::display(&item.path()),
        }));
    }
    entries.sort_by(|a, b| {
        let name_a = a["name"].as_str().unwrap_or("");
        let name_b = b["name"].as_str().unwrap_or("");
        let hidden_a = name_a.starts_with('.');
        let hidden_b = name_b.starts_with('.');
        hidden_a
            .cmp(&hidden_b)
            .then_with(|| name_a.to_lowercase().cmp(&name_b.to_lowercase()))
    });

    // At the root there is no parent level
    let parent = if requested == root {
        Value::Null
    } else {
        requested
            .parent()
            .map(|path| json!(paths::display(path)))
            .unwrap_or(Value::Null)
    };

    Ok(Json(json!({
        "ok": true,
        "root": paths::display(&root),
        "path": paths::display(&requested),
        "parent": parent,
        "entries": entries,
    })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct TailQuery {
    /// Log scope: client id, `job-xxx`, or `main` (the daemon's own startup log). Default = main.
    scope: String,
    /// Only entries with a greater seq (incremental polling / scrolling terminal-shell output).
    since: u64,
    /// How much history to return on the first visit.
    limit: usize,
}

/// Read an **in-memory** log scope (seq-tagged, good for incremental tailing).
///
/// Unlike `/api/clients/{id}/logs`, which always selects by client, this selects any scope
/// and defaults to `main` (the daemon's own startup/config log). The terminal shell uses it
/// to flush logs above the prompt, and external clients can finally read the main scope too
/// (previously only the Tauri window could, via IPC).
async fn api_log_tail(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<TailQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let scope = if query.scope.trim().is_empty() {
        log::MAIN_SCOPE.to_string()
    } else {
        query.scope.trim().to_string()
    };

    let mut entries: Vec<log::LogEntry> = log::core()
        .snapshot(&scope)
        .into_iter()
        .filter(|entry| entry.seq > query.since)
        .collect();

    // No `since` means "first look": keep only the last `limit` entries (default 200)
    // instead of dumping the whole buffer.
    let limit = if query.since == 0 {
        if query.limit == 0 {
            200
        } else {
            query.limit
        }
    } else {
        query.limit
    };
    if limit > 0 && entries.len() > limit {
        let start = entries.len() - limit;
        entries.drain(0..start);
    }

    Ok(Json(json!({ "ok": true, "scope": scope, "entries": entries })))
}

// ------------------------------------------------------------------ log history (design doc §10)

/// Request source tag: `X-Client: webui|cli|mcp|...` (the open enum of design doc §10).
fn request_source(headers: &HeaderMap) -> String {
    headers
        .get("x-client")
        .and_then(|value| value.to_str().ok())
        .map(log::normalize_source)
        .unwrap_or_else(|| log::SOURCE_DAEMON.to_string())
}

/// Log dates present on disk (newest first).
async fn api_log_days(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let file = log::core().file();
    let days = file.as_ref().map(|f| f.days()).unwrap_or_default();
    Ok(Json(json!({
        "ok": true,
        "days": days,
        "dir": file.as_ref().map(|f| f.dir().display().to_string()),
        "retainDays": file.as_ref().map(|f| f.retain_days()).unwrap_or(0),
        "failed": file.as_ref().map(|f| f.failed()).unwrap_or(false),
    })))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct DayQuery {
    /// Maximum number of entries to return (taken from the tail).
    limit: usize,
    /// Only this source (`webui` / `cli` / `mcp`); empty = no filter.
    source: String,
    /// Only this log scope (client id or `job-xxx`); empty = no filter.
    scope: String,
}

/// Read one day's persisted log.
async fn api_log_day(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(day): Path<String>,
    Query(query): Query<DayQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let file = log::core()
        .file()
        .ok_or_else(|| ApiError::bad_request("logFileDisabled", "log persistence is off"))?;

    let source = query.source.trim().to_ascii_lowercase();
    let scope = query.scope.trim().to_string();
    // Fetch more than needed under the filters, then truncate — otherwise filtering
    // could leave nothing at all.
    let fetch = if source.is_empty() && scope.is_empty() {
        query.limit
    } else {
        logfile::MAX_READ_ENTRIES
    };

    let entries = file
        .read_day(&day, fetch)
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::InvalidInput => ApiError::bad_request("badDay", err.to_string()),
            _ => ApiError::not_found("logDayNotFound"),
        })?;

    let total = entries.len();
    let mut filtered: Vec<logfile::StoredEntry> = entries
        .into_iter()
        .filter(|entry| source.is_empty() || entry.source == source)
        .filter(|entry| scope.is_empty() || entry.scope == scope)
        .collect();
    if query.limit > 0 && filtered.len() > query.limit {
        let start = filtered.len() - query.limit;
        filtered.drain(0..start);
    }

    Ok(Json(json!({
        "ok": true,
        "day": day,
        "total": total,
        "returned": filtered.len(),
        "entries": filtered,
    })))
}

// ------------------------------------------------------------------ workflow templates (§14.3)

fn workflow_summary(config_path: &std::path::Path, id: &str) -> Value {
    let template = workflow::is_template(id);
    match std::fs::read_to_string(workflow::path_of(config_path, id)) {
        Ok(raw) => match workflow::parse_json(&raw) {
            (Some(preset), errors) => json!({
                "id": id,
                "name": preset.name,
                "description": preset.description,
                "taskCount": preset.tasks.len(),
                "stepCount": preset.steps.len(),
                "variables": preset.variables,
                "tasks": preset.tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                "steps": preset.steps.iter().enumerate().map(|(index, step)| json!({
                    "index": index,
                    "id": step.id,
                    "taskId": step.task_id,
                })).collect::<Vec<_>>(),
                // Which batch id applying to this project would produce (deterministic,
                // so conflicts can be previewed)
                "batchId": workflow::batch_id_for(id),
                "template": template,
                "valid": errors.is_empty(),
                "errors": errors,
            }),
            (None, errors) => json!({
                "id": id,
                "name": id,
                "description": "",
                "taskCount": 0,
                "stepCount": 0,
                "variables": [],
                "tasks": [],
                "steps": [],
                "batchId": workflow::batch_id_for(id),
                "template": template,
                "valid": false,
                "errors": errors,
            }),
        },
        Err(err) => json!({
            "id": id,
            "name": id,
            "description": "",
            "taskCount": 0,
            "stepCount": 0,
            "variables": [],
            "tasks": [],
            "steps": [],
            "batchId": workflow::batch_id_for(id),
            "template": template,
            "valid": false,
            "errors": [err.to_string()],
        }),
    }
}

/// `GET /api/workflows` — which cross-project workflow templates exist (design doc §14.3).
async fn api_list_workflows(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let workflows: Vec<Value> = workflow::list_ids(&state.config_path)
        .iter()
        .map(|id| workflow_summary(&state.config_path, id))
        .collect();
    Ok(Json(json!({
        "workflows": workflows,
        "dir": workflow::dir(&state.config_path).display().to_string(),
        "hint": "apply 到项目:`POST /api/projects/{id}/workflows/{模板 id}/apply`\
                 (body 可选 {variables, overwrite, run, name}),先不带 overwrite 调用就是冲突预览",
    })))
}

/// `GET /api/workflows/{id}` — template source plus parse result.
async fn api_workflow_raw(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let raw = workflow::raw(&state.config_path, &id)
        .map_err(|_| ApiError::not_found("workflowNotFound"))?;
    let (parsed, errors) = workflow::parse_json(&raw);
    Ok(Json(json!({
        "id": id,
        "raw": raw,
        "template": workflow::is_template(&id),
        "valid": errors.is_empty(),
        "errors": errors,
        "parsed": parsed,
    })))
}

/// `PUT /api/workflows/{id}` — save (validate first, then write).
async fn api_workflow_put(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RawBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "invalidWorkflowId",
            "id 只能用字母、数字、- 和 _",
        ));
    }
    let (parsed, errors) = workflow::parse_json(&body.raw);
    if !errors.is_empty() {
        return Err(ApiError::bad_request("workflowInvalid", errors.join("; ")));
    }
    workflow::write_raw(&state.config_path, &id, &body.raw)
        .map_err(|e| ApiError::internal(&e))?;
    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {id}", t.workflow_saved));
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "workflow": parsed.map(|preset| workflow_summary_value(&id, preset)),
    })))
}

fn workflow_summary_value(id: &str, preset: workflow::WorkflowPreset) -> Value {
    json!({
        "id": id,
        "name": preset.name,
        "description": preset.description,
        "taskCount": preset.tasks.len(),
        "stepCount": preset.steps.len(),
        "variables": preset.variables,
        "batchId": workflow::batch_id_for(id),
        "valid": true,
        "errors": [],
    })
}

/// `DELETE /api/workflows/{id}` — delete one template file.
async fn api_workflow_delete(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    if !workflow::path_of(&state.config_path, &id).exists() {
        return Err(ApiError::not_found("workflowNotFound"));
    }
    workflow::remove(&state.config_path, &id)
        .map_err(|_| ApiError::internal("删除工作流模板失败"))?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// `GET /api/workflows/templates` — which bundled templates are still missing.
async fn api_workflow_templates(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let missing: Vec<Value> = workflow::missing_templates(&state.config_path)
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "name": name }))
        .collect();
    Ok(Json(json!({
        "missing": missing,
        "dir": workflow::dir(&state.config_path).display().to_string(),
    })))
}

/// `POST /api/workflows/reset` — restore missing bundled templates (never touches existing ones).
async fn api_workflow_reset(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ResetBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let wanted: Vec<String> = if body.ids.is_empty() {
        workflow::missing_templates(&state.config_path)
            .into_iter()
            .map(|(id, _)| id.to_string())
            .collect()
    } else {
        body.ids
            .into_iter()
            .filter(|id| !workflow::path_of(&state.config_path, id).exists())
            .collect()
    };
    let restored = workflow::restore_templates(&state.config_path, &wanted);
    let t = i18n::text(state.lang());
    log::log(
        log::INFO,
        format!("{}: {}", t.workflow_restored, restored.join(", ")),
    );
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

/// Request body for `POST /api/projects/{id}/workflows/{preset}/apply`.
#[derive(Deserialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ApplyWorkflowInput {
    /// Values for the template parameters (only names the template declares).
    pub variables: BTreeMap<String, String>,
    /// Actually write it (default **false** = conflict preview only).
    pub apply: bool,
    /// On conflict, overwrite the same-named task / batch (design doc §14.3 item 2).
    pub overwrite: bool,
    /// Run it once right after applying.
    pub run: bool,
    /// Batch display name; empty falls back to the template name.
    pub name: String,
}

/// `POST /api/projects/{id}/workflows/{preset}/apply` — instantiate a template into a project.
///
/// **Previews by default** (`apply: false`): returns the task list, batch id and conflicts so
/// the user/AI can see "what will appear and what collides" before calling again with
/// `apply: true` (the conflict preview required by §14.3 item 2).
async fn api_apply_workflow(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, preset_id)): Path<(String, String)>,
    Json(body): Json<ApplyWorkflowInput>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    let preset = workflow::load(&state.config_path, &preset_id)
        .map_err(|err| ApiError::bad_request("workflowInvalid", err))?;

    let mut overrides = BTreeMap::new();
    for (key, value) in &body.variables {
        overrides.insert(key.clone(), value.clone());
    }
    let batch_name = if body.name.trim().is_empty() {
        None
    } else {
        Some(body.name.clone())
    };
    let plan = workflow::plan(&preset_id, &preset, &project, &overrides, batch_name)
        .map_err(|errors| ApiError::bad_request("workflowInvalid", errors.join("; ")))?;

    let preview = json!({
        "presetId": preset_id,
        "batchId": plan.batch.id,
        "batchName": plan.batch.name,
        "tasks": plan.tasks.iter().map(|task| json!({
            "id": task.id,
            "kind": task.kind,
            "name": task.title(),
        })).collect::<Vec<_>>(),
        "steps": plan.batch.steps.iter().map(|step| json!({
            "id": step.id,
            "taskId": step.task_id,
        })).collect::<Vec<_>>(),
        "conflicts": plan.conflicts,
    });

    if !body.apply {
        return Ok(Json(json!({
            "ok": true,
            "applied": false,
            "preview": preview,
            "conflicts": plan.conflicts,
        })));
    }
    if !plan.conflicts.is_empty() && !body.overwrite {
        // Conflicts exist but overwriting is not allowed: don't guess, report them
        // plainly and let the caller decide
        return Err(ApiError {
            status: axum::http::StatusCode::CONFLICT,
            code: "workflowConflict",
            message: format!(
                "项目里已经有同名对象:{}(要覆盖就带 overwrite: true)",
                plan.conflicts.join("、")
            ),
            extra: Some(json!({ "conflicts": plan.conflicts, "preview": preview })),
        });
    }

    // Write: a same-named task is replaced in place (order preserved), same for the batch
    let mut updated = project.clone();
    for task in &plan.tasks {
        match updated.tasks.iter_mut().find(|item| item.id == task.id) {
            Some(slot) => *slot = task.clone(),
            None => updated.tasks.push(task.clone()),
        }
    }
    match updated.batches.iter_mut().find(|item| item.id == plan.batch.id) {
        Some(slot) => *slot = plan.batch.clone(),
        None => updated.batches.push(plan.batch.clone()),
    }
    // `workflow::plan` already checked the instantiated tasks one by one; re-check that
    // batch from the **project's** perspective here (once same-named tasks are overwritten,
    // step references may point at outputs that are no longer what they were).
    validate_batch(&plan.batch, &updated)?;

    {
        let mut list = crate::lock::write(&state.projects);
        let slot = list
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;
        *slot = updated.clone();
    }
    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    let t = i18n::text(state.lang());
    log::log(
        log::INFO,
        format!(
            "{}: {} -> {} ({} 个任务 / {} 步)",
            t.workflow_applied,
            preset_id,
            plan.batch.name,
            plan.tasks.len(),
            plan.batch.steps.len()
        ),
    );

    // Also tag the job's log source
    let mut job_id: Option<String> = None;
    if body.run {
        let client = find_client(&state, &updated.client_id)?;
        let run = prepare_batch_job(
            &state,
            Target {
                project: &updated,
                client: &client,
            },
            &plan.batch,
            0,
            BatchResume::default(),
            &request_source(&headers),
            false,
        )?;
        job_id = Some(run.job_id.clone());
        run_batch(state.clone(), updated.clone(), client, run);
    }

    Ok(Json(json!({
        "ok": true,
        "applied": true,
        "jobId": job_id,
        "preview": preview,
        "conflicts": plan.conflicts,
        "project": updated.to_json(),
    })))
}

// ------------------------------------------------------------------ build-type manifests

fn manifest_summary(config_path: &std::path::Path, id: &str) -> Value {
    let template = manifest::is_template(id);
    match std::fs::read_to_string(manifest::path_of(config_path, id)) {
        Ok(raw) => match manifest::parse_json(&raw) {
            (Some(parsed), errors) => {
                let mut value = parsed.to_json(id, template);
                value["valid"] = json!(errors.is_empty());
                value["errors"] = json!(errors);
                value
            }
            (None, errors) => json!({
                "id": id,
                "displayName": id,
                "platform": "",
                "include": { "paths": [] },
                "variables": [],
                "build": { "stepCount": 0, "artifactPath": "" },
                "cache": { "paths": [] },
                "template": template,
                "valid": false,
                "errors": errors,
            }),
        },
        Err(err) => json!({
            "id": id,
            "displayName": id,
            "platform": "",
            "include": { "paths": [] },
            "variables": [],
            "build": { "stepCount": 0, "artifactPath": "" },
            "cache": { "paths": [] },
            "template": template,
            "valid": false,
            "errors": [err.to_string()],
        }),
    }
}

/// Every manifest that parses (id -> Manifest), for build-type detection.
fn parsable_manifests(config_path: &std::path::Path) -> Vec<(String, manifest::Manifest)> {
    manifest::list_ids(config_path)
        .into_iter()
        .filter_map(|id| {
            std::fs::read_to_string(manifest::path_of(config_path, &id))
                .ok()
                .and_then(|raw| serde_json::from_str::<manifest::Manifest>(&raw).ok())
                .map(|parsed| (id, parsed))
        })
        .collect()
}

async fn api_list_manifests(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let manifests: Vec<Value> = manifest::list_ids(&state.config_path)
        .iter()
        .map(|id| manifest_summary(&state.config_path, id))
        .collect();
    Ok(Json(json!({
        "manifests": manifests,
        "dir": manifest::dir(&state.config_path).display().to_string(),
    })))
}

/// `GET /api/schema` — which schemas can be fetched.
async fn api_schema_list(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    Ok(Json(json!({
        "schemas": jsonschema::NAMES,
        "hint": "GET /api/schema/{名称} 拿具体那一份(manifest / task / batch)",
    })))
}

/// `GET /api/schema/{name}` — JSON Schema generated from the Rust structs (design doc §2).
/// Lets an AI agent self-check before acting: touches no machine and no disk.
async fn api_schema_named(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    jsonschema::named(&name)
        .map(Json)
        .ok_or_else(|| ApiError::not_found_message("schemaNotFound", &name))
}

async fn api_manifest_templates(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let missing: Vec<Value> = manifest::missing_templates(&state.config_path)
        .into_iter()
        .map(|(id, display_name)| json!({ "id": id, "displayName": display_name }))
        .collect();
    Ok(Json(json!({
        "missing": missing,
        "dir": manifest::dir(&state.config_path).display().to_string(),
    })))
}

/// `POST /api/manifests/reload` — a **refresh event**: re-read the build-type directory from disk.
///
/// Why it is needed: the json files under `manifest/` are **your files** — you may have edited
/// them in an editor, pulled them with git, or had an AI write them, none of which the daemon
/// sees. This endpoint does not guess: it rescans the directory, reports the diff against the
/// previous scan (added / changed / removed), and returns the current list too, saving a round trip.
///
/// If the directory is missing it is **created and seeded with the bundled templates** (the same
/// logic as first startup), so "I deleted the directory and want it back" also works.
async fn api_manifest_reload(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    let before = state.manifest_snapshot();
    // The directory may have been deleted entirely — then re-seed the templates (same action as first startup)
    let prepared = manifest::prepare_dir(&state.config_path);
    let after = manifest::snapshot(&state.config_path);
    let diff = manifest::diff(&before, &after);
    state.set_manifest_snapshot(after.clone());

    let manifests: Vec<Value> = after
        .keys()
        .map(|id| manifest_summary(&state.config_path, id))
        .collect();
    let invalid = manifests
        .iter()
        .filter(|item| item["valid"] != json!(true))
        .count();

    if prepared.created || !diff.is_empty() {
        log::log(
            log::INFO,
            crate::i18n::manifest_refreshed_log(
                state.lang(),
                &diff.describe(state.lang()),
                manifests.len(),
                invalid,
            ),
        );
    }

    Ok(Json(json!({
        "ok": true,
        "dir": manifest::dir(&state.config_path).display().to_string(),
        "created": prepared.created,
        "seeded": prepared.seeded,
        "added": diff.added,
        "changed": diff.changed,
        "removed": diff.removed,
        "summary": diff.describe(state.lang()),
        "total": manifests.len(),
        "invalid": invalid,
        "manifests": manifests,
    })))
}

#[derive(Deserialize)]
struct ResetBody {
    #[serde(default)]
    ids: Vec<String>,
}

async fn api_manifest_reset(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ResetBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    // Write back only what is missing: never touch files that already exist
    let wanted: Vec<String> = if body.ids.is_empty() {
        manifest::missing_templates(&state.config_path)
            .into_iter()
            .map(|(id, _)| id.to_string())
            .collect()
    } else {
        body.ids
            .into_iter()
            .filter(|id| !manifest::path_of(&state.config_path, id).exists())
            .collect()
    };

    let restored = manifest::restore_templates(&state.config_path, &wanted);
    let t = i18n::text(state.lang());
    log::log(
        log::INFO,
        format!("{}: {}", t.preset_restored, restored.join(", ")),
    );
    Ok(Json(json!({ "ok": true, "restored": restored })))
}

async fn api_manifest_raw(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let path = manifest::path_of(&state.config_path, &id);
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| ApiError::not_found("manifestNotFound"))?;
    Ok(Json(json!({
        "id": id,
        "raw": raw,
        "template": manifest::is_template(&id),
    })))
}

#[derive(Deserialize)]
struct RawBody {
    raw: String,
}

async fn api_manifest_put(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RawBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "invalidManifestId",
            "id 只能用字母、数字、- 和 _",
        ));
    }

    let (parsed, errors) = manifest::parse_json(&body.raw);
    if !errors.is_empty() {
        let mut error = ApiError::bad_request("invalidManifest", errors.join("; "));
        error.extra = Some(json!({ "details": errors }));
        return Err(error);
    }

    manifest::write_raw(&state.config_path, &id, &body.raw)
        .map_err(|e| ApiError::internal(&e))?;

    let display = parsed
        .as_ref()
        .map(|m| m.display_name.clone())
        .unwrap_or_else(|| id.clone());
    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {display}", t.manifest_saved));

    Ok(Json(json!({
        "ok": true,
        "manifest": manifest_summary(&state.config_path, &id),
    })))
}

async fn api_manifest_delete(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    manifest::remove(&state.config_path, &id)
        .map_err(|_| ApiError::not_found("manifestNotFound"))?;
    let t = i18n::text(state.lang());
    log::log(log::INFO, format!("{}: {id}", t.manifest_deleted));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetectBody {
    source_path: String,
}

async fn api_manifest_detect(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DetectBody>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    // Same root policy as browse: only directories browse can see can be probed here
    let source = state
        .host_roots()
        .confine(std::path::Path::new(body.source_path.trim()))
        .map_err(|err| match err {
            paths::PathError::Escape(message) => ApiError::forbidden("outsideRoot", message),
            paths::PathError::Syntax(message) => ApiError::bad_request("badPath", message),
        })?;
    if !source.is_dir() {
        return Err(ApiError::bad_request(
            "sourceNotFound",
            "宿主机上没有这个目录",
        ));
    }
    let matches = manifest::detect(&source, &parsable_manifests(&state.config_path));
    Ok(Json(json!({ "matches": matches })))
}

// ------------------------------------------------------------------ build tasks

fn project_json(state: &WebState, id: &str) -> Result<Value, ApiError> {
    find_project(state, id).map(|p| p.to_json())
}

/// Request body for `POST /api/projects/{id}/tasks` (design doc §7.1).
///
/// `pub` because `crate::jsonschema` generates the task schema from it, so "what the API
/// accepts" and "what the docs say" stay one struct; `Serialize` / `Clone` serve workflow
/// presets (a preset task is the **same** shape, stored as JSON and then instantiated).
#[derive(Debug, Clone, serde::Serialize, Deserialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskInput {
    /// Empty = create; an existing id = overwrite that entry.
    pub id: String,
    /// `build` / `push` / `pull` / `copy` / `move` / `delete` / `export`.
    /// **Empty means `build`** (compatibility with early projects.json holding only build tasks).
    pub kind: String,
    /// Display name, the one used in lists.
    pub name: String,
    // build
    /// Which build type (design doc §6.2), i.e. the file name `manifest/<id>.json`.
    #[schemars(length(min = 1))]
    pub manifest: String,
    /// Variable name -> value. Required variables are declared by the manifest itself;
    /// a missing one is rejected with `variableRequired` before execution.
    pub variables: BTreeMap<String, String>,
    /// `signed` | `unsigned` (design doc §5).
    pub signing_mode: String,
    /// Single artifact path, supports `{{variables}}`. `artifacts` wins when non-empty.
    pub artifact_path: String,
    /// Artifact path list (design doc §7.1): one build may emit several artifacts.
    pub artifacts: Vec<String>,
    /// Which Xcode this build uses (`.app` path); empty = the target machine's global choice (§8).
    pub xcode_path: String,
    // push
    /// Clear the remote project directory before pushing (the "overwrite" of design doc §9).
    pub clean: bool,
    /// Push as-is, without CRLF -> LF normalization (default false = normalize).
    ///
    /// A Windows-checked-out `gradlew` is CRLF and fails on a Mac because of the `\r` in its
    /// shebang (`/bin/sh^M: no such file or directory`). Only files with `#!` are touched.
    pub keep_crlf: bool,
    // copy / move
    /// Source. `path` is **relative to the project root**.
    pub from: Location,
    /// Destination.
    pub to: Location,
    /// Whether to overwrite an existing destination (used by copy / move / pull).
    pub overwrite: bool,
    // delete
    /// What to delete (for delete).
    pub target: Location,
    // pull / export
    /// Sources to pull / pack, **at least one** (for pull / export).
    pub sources: Vec<Location>,
    /// Export output file name (ultimately under `<host>/exports/`).
    pub output_name: String,
    // input / output contract (design doc §7.6.3), shared by all seven kinds
    /// Declared inputs: **values** come from `variables` (falling back to `default` here) and
    /// are referenced elsewhere as `{{inputs.<name>}}`. Types are for validation and hints only.
    pub inputs: BTreeMap<String, projects::InputSpec>,
    /// Declared outputs: once the task **finishes**, existence and type (`file` / `directory`)
    /// are checked against them. A required output that was not produced fails the step
    /// (error code `artifactMissing`).
    pub outputs: BTreeMap<String, projects::OutputSpec>,
}

impl TaskInput {
    pub(crate) fn into_task(self, id: String) -> Task {
        Task {
            id,
            kind: if self.kind.trim().is_empty() {
                projects::KIND_BUILD.to_string()
            } else {
                self.kind.trim().to_string()
            },
            name: self.name.trim().to_string(),
            manifest: self.manifest.trim().to_string(),
            variables: self.variables,
            signing_mode: SigningMode::parse(&self.signing_mode).code().to_string(),
            artifact_path: self.artifact_path.trim().to_string(),
            artifacts: self
                .artifacts
                .into_iter()
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect(),
            xcode_path: self.xcode_path.trim().to_string(),
            clean: self.clean,
            keep_crlf: self.keep_crlf,
            from: self.from,
            to: self.to,
            overwrite: self.overwrite,
            target: self.target,
            sources: self.sources,
            output_name: self.output_name.trim().to_string(),
            inputs: self.inputs,
            outputs: self.outputs,
        }
    }
}

async fn api_save_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<TaskInput>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    if input.kind.is_empty() || input.kind == projects::KIND_BUILD {
        // A build task's manifest must really exist and be valid
        manifest::load(&state.config_path, &input.manifest)
            .map_err(|_| ApiError::bad_request("manifestInvalid", input.manifest.clone()))?;
    }

    {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;

        let existing = project
            .tasks
            .iter()
            .position(|task| !input.id.is_empty() && task.id == input.id);
        let task_id = match existing {
            Some(index) => project.tasks[index].id.clone(),
            None => projects::new_task_id(),
        };

        let task = input.into_task(task_id);
        let errors = task.validate();
        if !errors.is_empty() {
            return Err(ApiError::bad_request("taskInvalid", errors.join("; ")));
        }

        match existing {
            Some(index) => project.tasks[index] = task,
            None => project.tasks.push(task),
        }
    }

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;

    Ok(Json(json!({ "ok": true, "project": project_json(&state, &id)? })))
}

async fn api_delete_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;
        let before = project.tasks.len();
        project.tasks.retain(|task| task.id != task_id);
        if project.tasks.len() == before {
            return Err(ApiError::not_found("taskNotFound"));
        }
        // Batch steps referencing this task are removed too, so none point at thin air
        for batch in project.batches.iter_mut() {
            batch.steps.retain(|step| step.task_id != task_id);
        }
    }

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok(Json(json!({ "ok": true, "project": project_json(&state, &id)? })))
}

/// Top-level field names of a schema — `PATCH` uses them to tell whether a key is known.
///
/// Deliberately computed from the schema instead of a hand-written table: a hand-written one
/// would drift from the structs, while the schema is generated from them (design doc §2).
fn field_names(schema: &str) -> Vec<String> {
    jsonschema::named(schema)
        .and_then(|value| {
            value
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| props.keys().cloned().collect())
        })
        .unwrap_or_default()
}

/// Merge the keys a `PATCH` supplied onto the current object's JSON (design doc §10.1).
///
/// The base comes from the struct's own `Serialize` (**all fields**), not `to_json()` — the
/// latter emits only kind-relevant fields, so merging onto it would silently drop `keepCrlf`.
fn merge_patch(current: &Value, patch: &Value, allowed: &[String]) -> Result<Value, String> {
    let mut base = current.clone();
    let Some(target) = base.as_object_mut() else {
        return Err("当前对象不是 JSON 对象".to_string());
    };
    let Some(object) = patch.as_object() else {
        return Err("PATCH 的请求体必须是一个 JSON 对象".to_string());
    };
    for (key, value) in object {
        if !allowed.iter().any(|name| name == key) {
            return Err(format!("不认识的字段:{key}"));
        }
        target.insert(key.clone(), value.clone());
    }
    // `id` belongs to the path, not the request body
    target.remove("id");
    Ok(base)
}

/// Save-time validation for a batch (design doc §7.3 / §7.6.5).
///
/// The rules live in [`workflow::validate_steps`] and are **shared by project batches and
/// workflow presets**, so "saves into a project but cannot be applied" cannot happen.
/// All this does is turn `(error code, message)` into the API-layer [`ApiError`].
fn validate_batch(batch: &Batch, project: &Project) -> Result<(), ApiError> {
    workflow::validate_steps(&project.tasks, &batch.steps).map_err(|(code, message)| {
        if code == "batchInvalid" {
            ApiError::bad_request("batchInvalid", message)
        } else {
            ApiError::bad_request(code, message)
        }
    })
}

/// `GET /api/projects/{id}/tasks` — list the project's tasks (design doc §10.1).
///
/// Previously callers had to pull the whole project out of `GET /api/projects` and pick
/// tasks themselves — wasteful for anyone (especially an agent) who only wants the list.
async fn api_list_tasks(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    Ok(Json(json!({
        "projectId": project.id,
        "tasks": project.tasks.iter().map(Task::to_json).collect::<Vec<_>>(),
    })))
}

/// `PATCH /api/projects/{id}/tasks/{task_id}` — change only the given fields (design doc §10.1).
///
/// Unlike `POST /tasks`, which **replaces the whole entry** (unsupplied fields reset to their
/// defaults), `PATCH` touches only the keys provided: renaming should not wipe `inputs` / `keepCrlf`.
async fn api_patch_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let allowed = field_names("task");

    let task = {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;
        let index = project
            .tasks
            .iter()
            .position(|task| task.id == task_id)
            .ok_or_else(|| ApiError::not_found("taskNotFound"))?;

        let current = serde_json::to_value(&project.tasks[index])
            .map_err(|err| ApiError::internal(&err.to_string()))?;
        let merged = merge_patch(&current, &patch, &allowed)
            .map_err(|err| ApiError::bad_request("unknownField", err))?;
        let mut task: Task = serde_json::from_value(merged)
            .map_err(|err| ApiError::bad_request("taskInvalid", err.to_string()))?;
        task.id = task_id.clone();

        if task.kind() == projects::KIND_BUILD {
            manifest::load(&state.config_path, &task.manifest)
                .map_err(|_| ApiError::bad_request("manifestInvalid", task.manifest.clone()))?;
        }
        let errors = task.validate();
        if !errors.is_empty() {
            return Err(ApiError::bad_request("taskInvalid", errors.join("; ")));
        }
        project.tasks[index] = task.clone();
        task
    };

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "task": task.to_json(),
        "project": project_json(&state, &id)?,
    })))
}

/// `GET /api/projects/{id}/batches` — list batch workflows (design doc §10.1).
async fn api_list_batches(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    Ok(Json(json!({
        "projectId": project.id,
        "batches": project.batches.iter().map(Batch::to_json).collect::<Vec<_>>(),
    })))
}

/// `PATCH /api/projects/{id}/batches/{batch_id}` — patch only the supplied fields (design doc §10.1).
async fn api_patch_batch(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, batch_id)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let allowed = field_names("batch");

    let batch = {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;
        let index = project
            .batches
            .iter()
            .position(|batch| batch.id == batch_id)
            .ok_or_else(|| ApiError::not_found("batchNotFound"))?;

        let current = serde_json::to_value(&project.batches[index])
            .map_err(|err| ApiError::internal(&err.to_string()))?;
        let merged = merge_patch(&current, &patch, &allowed)
            .map_err(|err| ApiError::bad_request("unknownField", err))?;
        let mut batch: Batch = serde_json::from_value(merged)
            .map_err(|err| ApiError::bad_request("batchInvalid", err.to_string()))?;
        batch.id = batch_id.clone();

        validate_batch(&batch, project)?;
        project.batches[index] = batch.clone();
        batch
    };

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "batch": batch.to_json(),
        "project": project_json(&state, &id)?,
    })))
}

/// Fetch (project, client, task, manifest); shared by three endpoints.
fn resolve_task(
    state: &WebState,
    id: &str,
    task_id: &str,
) -> Result<(Project, Client, Task, Option<manifest::Manifest>), ApiError> {
    let project = find_project(state, id)?;
    let client = find_client(state, &project.client_id)?;
    let task = project
        .task(task_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("taskNotFound"))?;

    let parsed = if task.kind() == projects::KIND_BUILD {
        let manifest = manifest::load(&state.config_path, &task.manifest)
            .map_err(|e| ApiError::bad_request("manifestInvalid", e))?;
        Some(manifest)
    } else {
        None
    };
    Ok((project, client, task, parsed))
}

// ------------------------------------------------------------------ task execution engine

/// Result of executing one task (or one step of a batch).
struct StepOutcome {
    ok: bool,
    cancelled: bool,
    message: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    exit_code: i32,
    artifacts: Vec<String>,
    files: u64,
    bytes: u64,
    /// Structured failure reason (design doc §10.1): `artifactMissing` / `inputInvalid` / `taskInvalid` ...
    code: Option<&'static str>,
}

impl StepOutcome {
    fn fail(message: impl Into<String>) -> StepOutcome {
        StepOutcome {
            ok: false,
            cancelled: false,
            message: message.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            exit_code: -1,
            artifacts: Vec::new(),
            files: 0,
            bytes: 0,
            code: None,
        }
    }

    /// Failure with a structured error code — lets the API / MCP side tell which kind of problem it is (design doc §10.1).
    fn fail_code(code: &'static str, message: impl Into<String>) -> StepOutcome {
        let mut outcome = StepOutcome::fail(message);
        outcome.code = Some(code);
        outcome
    }

    fn sync(outcome: sync::SyncOutcome, artifacts: Vec<String>) -> StepOutcome {
        StepOutcome {
            ok: outcome.ok,
            cancelled: outcome.cancelled,
            message: outcome.message,
            inputs: Vec::new(),
            outputs: Vec::new(),
            exit_code: if outcome.ok { 0 } else { -1 },
            artifacts,
            files: outcome.files,
            bytes: outcome.bytes,
            code: None,
        }
    }
}

const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const SYNC_TIMEOUT: Duration = Duration::from_secs(900);
const FILE_TIMEOUT: Duration = Duration::from_secs(600);

/// Task kind -> `&'static str`, for the jobs registry (its `kind` field requires a static string).
fn kind_static(kind: &str) -> &'static str {
    projects::KINDS
        .iter()
        .copied()
        .find(|known| *known == kind)
        .unwrap_or("task")
}

fn roots_for(project: &Project) -> fileops::Roots {    fileops::Roots {
        remote: project.remote_path.clone(),
        host: projects::host_root(project),
    }
}

/// Execute one task. Registers no job and writes no job record — that is the caller's job (single task / batch).
async fn execute_task(
    state: &Arc<WebState>,
    project: &Project,
    client: &Client,
    task: &Task,
    // Input values used the last time this job ran (design doc §7.6.7). Normal runs pass `None`;
    // `retry` passes the snapshot so that "edit a task variable, then retry" cannot silently swap inputs.
    snapshot: Option<&BTreeMap<String, String>>,
    scopes: &[String],
    cancel: &Cancel,
) -> StepOutcome {
    let t = i18n::text(state.lang());
    let roots = roots_for(project);

    // ---- input contract: resolve + expand + re-validate (§7.6.3 / §7.6.4)--------
    let inputs = match contract::resolve_inputs(task, snapshot) {
        Ok(values) => values,
        Err(errors) => {
            let message = format!("{}:{}", t.input_invalid, errors.join("; "));
            log::log_multi(scopes, log::ERROR, message.clone());
            return StepOutcome::fail_code("inputInvalid", message);
        }
    };
    let resolved = match contract::resolved(task, &inputs) {
        Ok(value) => value,
        Err(errors) => {
            let message = format!("{}:{}", t.input_invalid, errors.join("; "));
            log::log_multi(scopes, log::ERROR, message.clone());
            return StepOutcome::fail_code("inputInvalid", message);
        }
    };
    let mut declared = match contract::resolved_outputs(&resolved, &inputs) {
        Ok(value) => value,
        Err(errors) => {
            let message = format!("{}:{}", t.input_invalid, errors.join("; "));
            log::log_multi(scopes, log::ERROR, message.clone());
            return StepOutcome::fail_code("inputInvalid", message);
        }
    };
    // From here on always use the expanded copy (the stored one keeps the template text).
    let task = &resolved;

    let mut outcome = match task.kind() {
        projects::KIND_BUILD => {
            // Pre-flight check (acceptance notes §4.2): `build` does **not** push automatically. If the
            // project has never been pushed, the remote dir is likely empty and the build fails at the first
            // step (`cd` / `chmod gradlew`) with a message that looks like a broken environment. Say so up
            // front and point at the two right ways to do it.
            if project.last_sync_at == 0 {
                log::log_multi(
                    scopes,
                    log::WARN,
                    format!(
                        "这个项目还没推送过源码(lastSyncAt = 0)。build 任务不会自动 push —— \
                         如果远端目录是空的,构建会在第一步失败。请先 `cgw push {}`,\
                         或者建一个 push -> build 的 batch(推荐,一次点完)。",
                        project.id
                    ),
                );
            }
            let manifest = match manifest::load(&state.config_path, &task.manifest) {
                Ok(manifest) => manifest,
                Err(err) => {
                    return StepOutcome::fail(format!("{}: {err}", t.manifest_invalid))
                }
            };
            for step in &manifest.build.steps {
                if let (Some(step_id), Some(outputs)) = (step.id(), step.outputs()) {
                    for (name, spec) in outputs {
                        let rendered = manifest::render_template(&manifest, &task.variables, &spec.path);
                        let trimmed = rendered.trim().to_string();
                        if !trimmed.is_empty() {
                            declared.push(contract::DeclaredOutput {
                                name: format!("{step_id}:{name}"),
                                side: paths::Side::Remote,
                                path: trimmed,
                                directory: spec.wants_directory(),
                                required: spec.is_required(),
                            });
                        }
                    }
                }
            }
            let signing = SigningMode::parse(&task.signing_mode);
            // List named steps in the log (design doc §7.6.9 item 3): on a build failure the remote only
            // prints a bare `xcodebuild: error: ...`, so without this you cannot tell which one "step 4" was.
            if manifest.build.steps.iter().any(|step| step.id().is_some()) {
                let labels: Vec<String> = manifest
                    .build
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| match step.id() {
                        Some(id) => format!("{}({})", id.trim(), index + 1),
                        None => format!("第 {} 步", index + 1),
                    })
                    .collect();
                log::log_multi(
                    scopes,
                    log::INFO,
                    format!(
                        "构建分 {} 步:{}",
                        manifest.build.steps.len(),
                        labels.join(" -> ")
                    ),
                );
            }
            let outcome = crate::build::run(
                client,
                &project.remote_path,
                &manifest,
                &task.variables,
                signing,
                &task.artifact_templates(),
                &task.build_env(),
                BUILD_TIMEOUT,
                scopes,
                cancel,
            )
            .await;
            StepOutcome {
                ok: outcome.ok,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs: vec![format!("remote:{}", project.remote_path)],
                outputs: outcome
                    .artifacts
                    .iter()
                    .map(|artifact| format!("remote:{artifact}"))
                    .collect(),
                exit_code: outcome.exit_code,
                artifacts: outcome.artifacts,
                files: 0,
                bytes: 0,
                code: None,
            }
        }
        projects::KIND_PUSH => {
            let excludes = effective_excludes(state, project);
            let source = PathBuf::from(&project.source_path);
            let outcome = sync::push_with(
                client,
                &source,
                &project.remote_path,
                &excludes,
                task.clean,
                task.keep_crlf,
                SYNC_TIMEOUT,
                cancel,
                state.lang(),
            )
            .await;
            // Write back lastSyncAt / lastSyncOk / host fingerprint (the build pre-flight check in design doc
            // §7.1 relies on it).
            //
            // Previously only the "quick push" endpoint `/api/projects/{id}/push` wrote this: pushes via a
            // task or batch did not, so a `push -> build` batch could push successfully and still have build
            // report "this project has never pushed sources (lastSyncAt = 0)" (acceptance notes 2026-09-28 §4 item 1).
            record_sync(
                state,
                &project.id,
                &client.id,
                outcome.host_key.clone(),
                outcome.ok && !outcome.cancelled && !cancel.requested(),
                &outcome.message,
                outcome.files,
                outcome.bytes,
            );
            let mut result = StepOutcome::sync(outcome, Vec::new());
            result.inputs = vec![format!("host:{}", project.source_path)];
            result.outputs = vec![format!("remote:{}", project.remote_path)];
            result
        }
        projects::KIND_PULL => {
            if task.sources.is_empty() {
                // No sources given: fall back to pulling the whole remote project back
                let target = projects::host_root(project);
                let outcome = sync::pull(
                    client,
                    &project.remote_path,
                    &target,
                    SYNC_TIMEOUT,
                    cancel,
                    state.lang(),
                )
                .await;
                // Record sync state just like push (same convention as `/api/projects/{id}/pull`).
                // A successful pull implies the remote dir really exists and has content, so it is also
                // correct that the build pre-flight check (`lastSyncAt == 0`) stops warning afterwards.
                record_sync(
                    state,
                    &project.id,
                    &client.id,
                    outcome.host_key.clone(),
                    outcome.ok && !outcome.cancelled && !cancel.requested(),
                    &outcome.message,
                    outcome.files,
                    outcome.bytes,
                );
                let mut result = StepOutcome::sync(outcome, Vec::new());
                result.inputs = vec![format!("remote:{}", project.remote_path)];
                result.outputs = vec![format!("host:{}", target.display())];
                result
            } else {
                pull_sources(state, client, project, task, &roots, scopes, cancel).await
            }
        }
        projects::KIND_COPY => {
            let outcome = fileops::copy(
                client,
                &roots,
                &task.from,
                &task.to,
                task.overwrite,
                FILE_TIMEOUT,
                cancel,
                scopes,
            )
            .await;
            StepOutcome {
                ok: outcome.ok,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs: outcome.inputs,
                outputs: outcome.outputs,
                exit_code: if outcome.ok { 0 } else { -1 },
                artifacts: Vec::new(),
                files: outcome.files,
                bytes: outcome.bytes,
                code: None,
            }
        }
        projects::KIND_MOVE => {
            let outcome = fileops::move_path(
                client,
                &roots,
                &task.from,
                &task.to,
                task.overwrite,
                FILE_TIMEOUT,
                cancel,
                scopes,
            )
            .await;
            StepOutcome {
                ok: outcome.ok,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs: outcome.inputs,
                outputs: outcome.outputs,
                exit_code: if outcome.ok { 0 } else { -1 },
                artifacts: Vec::new(),
                files: outcome.files,
                bytes: outcome.bytes,
                code: None,
            }
        }
        projects::KIND_DELETE => {
            let outcome =
                fileops::delete(client, &roots, &task.target, FILE_TIMEOUT, cancel, scopes).await;
            StepOutcome {
                ok: outcome.ok,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs: outcome.inputs,
                outputs: outcome.outputs,
                exit_code: if outcome.ok { 0 } else { -1 },
                artifacts: Vec::new(),
                files: outcome.files,
                bytes: outcome.bytes,
                code: None,
            }
        }
        projects::KIND_EXPORT => {
            let name = if task.output_name.trim().is_empty() {
                format!("{}-{}.zip", project.name, projects::now_ms() / 1000)
            } else {
                task.output_name.clone()
            };
            let outcome = fileops::export(
                client,
                &roots,
                &task.sources,
                &name,
                FILE_TIMEOUT,
                cancel,
                scopes,
            )
            .await;
            // The zip produced by export is this task's artifact; put it in artifacts to stay consistent with batch
            let artifacts = outcome.outputs.clone();
            StepOutcome {
                ok: outcome.ok,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs: outcome.inputs,
                outputs: outcome.outputs,
                exit_code: if outcome.ok { 0 } else { -1 },
                artifacts,
                files: outcome.files,
                bytes: outcome.bytes,
                code: None,
            }
        }
        other => StepOutcome::fail(format!("未知的任务类型:{other}")),
    };

    // ---- output contract: declared artifacts must really exist (§7.6.3)----------------------
    // "A Task must not be judged successful by shell exit code alone" — exit code 0 only means the command did not error out.
    if outcome.ok && !declared.is_empty() {
        let (verified, problems, optional_missing) =
            verify_outputs(client, project, &declared).await;
        if !problems.is_empty() {
            let message = format!("{}:{}", t.artifacts_missing, problems.join("; "));
            log::log_multi(scopes, log::ERROR, message.clone());
            outcome.ok = false;
            outcome.exit_code = -1;
            outcome.code = Some("artifactMissing");
            outcome.message = message;
        } else {
            if !optional_missing.is_empty() {
                outcome.message = format!(
                    "{};可选输出没产出:{}",
                    outcome.message,
                    optional_missing.join("、")
                );
            }
            if !verified.is_empty() {
                // Verified artifacts are more trustworthy than a path guessed from the template — use them as-is
                let plain: Vec<String> = verified
                    .iter()
                    .map(|item| {
                        item.split_once(':')
                            .map(|(_, rest)| rest.to_string())
                            .unwrap_or_else(|| item.clone())
                    })
                    .collect();
                outcome.outputs = verified;
                outcome.artifacts = plain;
            }
        }
    }

    outcome
}

/// Verify outputs against their declaration (design doc §7.6.3): existence + type.
///
/// The remote side checks all `remote` entries in a single SSH call; the host side reads the filesystem directly.
/// Returns `(verified, required problems, optional missing)` — a missing optional only records a note, it is not a failure.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

async fn verify_outputs(
    client: &Client,
    project: &Project,
    declared: &[contract::DeclaredOutput],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let roots = roots_for(project);
    let mut verified: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    let mut optional_missing: Vec<String> = Vec::new();

    let mut record = |label: String, ok: bool, detail: String, required: bool| {
        if ok {
            verified.push(label);
        } else if required {
            problems.push(format!("{label} {detail}"));
        } else {
            optional_missing.push(format!("{label} {detail}"));
        }
    };

    // Host side
    for item in declared.iter().filter(|item| item.side == Side::Host) {
        let label = format!("host:{}", item.path);
        match paths::confine_local(&roots.host, &item.path) {
            Ok(abs) => match std::fs::symlink_metadata(&abs) {
                Ok(meta) => {
                    let is_dir = meta.file_type().is_dir();
                    if item.directory && !is_dir {
                        record(label, false, "存在但不是目录".to_string(), item.required);
                    } else if !item.directory && is_dir {
                        record(
                            label,
                            false,
                            "存在但是目录,期望文件".to_string(),
                            item.required,
                        );
                    } else {
                        record(label, true, String::new(), item.required);
                    }
                }
                Err(_) => record(label, false, "不存在".to_string(), item.required),
            },
            Err(err) => record(
                label,
                false,
                format!("越界:{err}"),
                item.required,
            ),
        }
    }

    // Remote side: one command checks everything and prints machine-readable `__CGW_OUT__ <index> <type>`
    let remote: Vec<&contract::DeclaredOutput> = declared
        .iter()
        .filter(|item| item.side == Side::Remote)
        .collect();
    if !remote.is_empty() {
        let root = ssh::shell_path(&project.remote_path);
        let checks: Vec<String> = remote
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let path = ssh::shell_path(&item.path);
                format!(
                    "if [ -d {path} ]; then echo \"__CGW_OUT__ {index} dir\"; \
                     elif [ -f {path} ]; then echo \"__CGW_OUT__ {index} file\"; \
                     else echo \"__CGW_OUT__ {index} missing\"; fi"
                )
            })
            .collect();
        let command = format!("cd {root} && {}", checks.join("; "));
        let probe = ssh::run(client, &command, VERIFY_TIMEOUT).await;
        if !probe.ok {
            // The check itself did not run: must not be treated as "artifact present", report it honestly as a problem
            for item in &remote {
                let label = format!("remote:{}", item.path);
                record(
                    label,
                    false,
                    format!("核对失败:{}", probe.message),
                    item.required,
                );
            }
        } else {
            for (index, item) in remote.iter().enumerate() {
                let label = format!("remote:{}", item.path);
                let found = probe.output.lines().find_map(|line| {
                    let rest = line.trim().strip_prefix("__CGW_OUT__ ")?;
                    let mut parts = rest.split_whitespace();
                    let current: usize = parts.next()?.parse().ok()?;
                    let kind = parts.next()?.to_string();
                    (current == index).then_some(kind)
                });
                match found.as_deref() {
                    Some("dir") if item.directory => record(label, true, String::new(), item.required),
                    Some("dir") => record(
                        label,
                        false,
                        "存在但是目录,期望文件".to_string(),
                        item.required,
                    ),
                    Some("file") if !item.directory => {
                        record(label, true, String::new(), item.required)
                    }
                    Some("file") => record(
                        label,
                        false,
                        "存在但是文件,期望目录".to_string(),
                        item.required,
                    ),
                    _ => record(label, false, "不存在".to_string(), item.required),
                }
            }
        }
    }

    (verified, problems, optional_missing)
}

/// `pull` task: move the listed sources into the host directory named by `to`.
async fn pull_sources(
    state: &Arc<WebState>,
    client: &Client,
    project: &Project,
    task: &Task,
    roots: &fileops::Roots,
    scopes: &[String],
    cancel: &Cancel,
) -> StepOutcome {
    let _ = state;
    let target_rel = match paths::normalize_relative(&task.to.path) {
        Ok(value) => value,
        Err(err) => return StepOutcome::fail(format!("to 不合法:{err}")),
    };
    // No `to` given: fall back to `<host>/.artifacts/`
    let target_rel = if target_rel.is_empty() {
        ".artifacts".to_string()
    } else {
        target_rel
    };

    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut files = 0u64;
    let mut bytes = 0u64;

    for source in &task.sources {
        let rel = match paths::normalize_relative(&source.path) {
            Ok(value) if !value.is_empty() => value,
            Ok(_) => return StepOutcome::fail("sources 里不允许出现根目录本身"),
            Err(err) => return StepOutcome::fail(format!("sources 不合法:{err}")),
        };
        let base = rel.rsplit_once('/').map(|(_, n)| n).unwrap_or(&rel);
        let dest = Location {
            side: Side::Host,
            path: format!("{target_rel}/{base}"),
        };
        let outcome = fileops::copy(
            client,
            roots,
            source,
            &dest,
            task.overwrite,
            FILE_TIMEOUT,
            cancel,
            scopes,
        )
        .await;

        files += outcome.files;
        bytes += outcome.bytes;
        inputs.extend(outcome.inputs);
        outputs.extend(outcome.outputs);
        if !outcome.ok {
            return StepOutcome {
                ok: false,
                cancelled: outcome.cancelled,
                message: outcome.message,
                inputs,
                outputs,
                exit_code: -1,
                artifacts: Vec::new(),
                files,
                bytes,
                code: None,
            };
        }
    }

    let artifacts = vec![format!("host:{}", projects::host_root(project).join(&target_rel).display())];
    StepOutcome {
        ok: true,
        cancelled: false,
        message: format!("已回调 {} 个来源到 {}", task.sources.len(), target_rel),
        inputs,
        outputs: outputs.clone(),
        exit_code: 0,
        artifacts,
        files,
        bytes,
        code: None,
    }
}

// ------------------------------------------------------------------ single task

async fn api_preview_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let (project, _client, task, parsed) = resolve_task(&state, &id, &task_id)?;

    // Preview must show the **expanded** copy — otherwise the agent sees the literal `{{inputs.x}}`
    // while something else is what actually runs (design doc §7.6.3 / §7.6.4).
    let inputs = contract::resolve_inputs(&task, None)
        .map_err(|errors| ApiError::bad_request("inputInvalid", errors.join("; ")))?;
    let task = contract::resolved(&task, &inputs)
        .map_err(|errors| ApiError::bad_request("inputInvalid", errors.join("; ")))?;
    let declared = contract::resolved_outputs(&task, &inputs)
        .map_err(|errors| ApiError::bad_request("inputInvalid", errors.join("; ")))?;

    let mut response = match task.kind() {
        projects::KIND_BUILD => {
            let parsed = parsed.ok_or_else(|| ApiError::internal("missing manifest"))?;
            let signing = SigningMode::parse(&task.signing_mode);
            match crate::build::preview(
                &parsed,
                &task.variables,
                signing,
                &project.remote_path,
                &task.build_env(),
            ) {
                Ok((command, defaults)) => {
                    let artifacts = task.resolve_artifacts(&parsed);
                    let artifacts = if artifacts.is_empty() {
                        defaults
                    } else {
                        artifacts
                    };
                    Ok(Json(json!({
                        "kind": task.kind(),
                        "command": command,
                        "artifactPath": artifacts.first().cloned().unwrap_or_default(),
                        "artifacts": artifacts,
                        "xcodePath": task.xcode_path,
                    })))
                }
                Err(missing) => Err(ApiError::bad_request(
                    "variableRequired",
                    format!("缺少必填变量:{}", missing.join(", ")),
                )),
            }
        }
        projects::KIND_COPY | projects::KIND_MOVE => Ok(Json(json!({
            "kind": task.kind(),
            "command": "",
            "from": task.from.to_json(),
            "to": task.to.to_json(),
            "overwrite": task.overwrite,
        }))),
        projects::KIND_DELETE => Ok(Json(json!({
            "kind": task.kind(),
            "command": "",
            "target": task.target.to_json(),
        }))),
        projects::KIND_PULL | projects::KIND_EXPORT => Ok(Json(json!({
            "kind": task.kind(),
            "command": "",
            "sources": task.sources.iter().map(Location::to_json).collect::<Vec<_>>(),
            "to": task.to.to_json(),
            "outputName": task.output_name,
        }))),
        projects::KIND_PUSH => Ok(Json(json!({
            "kind": task.kind(),
            "command": "",
            "clean": task.clean,
        }))),
        other => Err(ApiError::bad_request("taskInvalid", other.to_string())),
    }?;

    // Also return the resolved inputs and declared outputs: that tells the agent which values the run will
    // use and what will be verified afterwards (design doc §7.6.3).
    if let Some(object) = response.0.as_object_mut() {
        if !inputs.is_empty() {
            object.insert("inputs".to_string(), json!(inputs));
        }
        if !declared.is_empty() {
            object.insert(
                "outputs".to_string(),
                json!(declared
                    .iter()
                    .map(|item| json!({
                        "name": item.name,
                        "side": item.side.code(),
                        "path": item.path,
                        "type": if item.directory { "directory" } else { "file" },
                        "required": item.required,
                    }))
                    .collect::<Vec<_>>()),
            );
        }
    }
    Ok(response)
}

/// `POST /api/projects/{id}/tasks/{task_id}/enqueue` — queue for execution (design doc §7.6.6).
///
/// The only difference from `/run`: when the project is busy it does **not** return 409, it appends to the queue.
/// The existing `/run` semantics (`projectBusy`) are **unchanged to the letter** — §7.6.6 item 6 requires
/// "keep the existing immediate-execution endpoint, add an explicit enqueue behavior".
async fn api_enqueue_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let (project, client, task, _parsed) = resolve_task(&state, &id, &task_id)?;
    let run = prepare_task_job(
        &state,
        Target {
            project: &project,
            client: &client,
        },
        &task,
        &request_source(&headers),
        true,
    )?;
    let job_id = run.job_id.clone();
    let position = state.jobs.queue_position(&job_id);
    state.enqueue_run(QueuedRun {
        project_id: project.id.clone(),
        client_id: client.id.clone(),
        plan: QueuedPlan::Task(Box::new(run)),
    });
    // If the project is free right now it starts immediately; if busy it just queues
    pump_queues(&state);
    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "queued": true,
        "queuePosition": position,
        "project": project.to_json(),
    })))
}

/// `POST /api/projects/{id}/batches/{batch_id}/enqueue` — queue a batch for execution.
async fn api_enqueue_batch(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, batch_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let batch = project
        .batch(&batch_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("batchNotFound"))?;

    let run = prepare_batch_job(
        &state,
        Target {
            project: &project,
            client: &client,
        },
        &batch,
        0,
        BatchResume::default(),
        &request_source(&headers),
        true,
    )?;
    let job_id = run.job_id.clone();
    let position = state.jobs.queue_position(&job_id);
    state.enqueue_run(QueuedRun {
        project_id: project.id.clone(),
        client_id: client.id.clone(),
        plan: QueuedPlan::Batch(Box::new(run)),
    });
    pump_queues(&state);
    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "queued": true,
        "queuePosition": position,
        "project": project.to_json(),
    })))
}

async fn api_run_task(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let (project, client, task, _parsed) = resolve_task(&state, &id, &task_id)?;
    let run = prepare_task_job(
        &state,
        Target {
            project: &project,
            client: &client,
        },
        &task,
        &request_source(&headers),
        false,
    )?;
    let job_id = run.job_id.clone();
    run_task(state.clone(), project.clone(), client, run);

    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "project": project.to_json(),
    })))
}

/// Everything a **single task** run needs (computed when the job record is created, used directly at execution).
struct TaskRun {
    task: Task,
    job_id: String,
    cancel: Cancel,
}

/// Everything a **batch** run needs (see [`BatchResume`] and design doc §7.6.5/§7.6.7).
struct BatchRun {
    batch: Batch,
    from_step: usize,
    job_id: String,
    cancel: Cancel,
    /// Input snapshot for each step (keys are `<taskId>.<input name>`).
    snapshot: BTreeMap<String, String>,
    /// Step outputs that have already been verified.
    produced: BTreeMap<String, String>,
}

/// A queued run: remembers "what to run" and executes once the project frees up (design doc §7.6.6).
struct QueuedRun {
    project_id: String,
    client_id: String,
    plan: QueuedPlan,
}

enum QueuedPlan {
    Task(Box<TaskRun>),
    Batch(Box<BatchRun>),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredQueuedRun {
    pub job_id: String,
    pub project_id: String,
    pub client_id: String,
    pub plan: StoredQueuedPlan,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StoredQueuedPlan {
    Task {
        task: Task,
    },
    Batch {
        batch: Batch,
        from_step: usize,
        snapshot: BTreeMap<String, String>,
        produced: BTreeMap<String, String>,
    },
}

pub fn queue_store_path(config_path: &std::path::Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("queued_runs.json")
}

fn load_queued_runs(path: &std::path::Path) -> HashMap<String, StoredQueuedRun> {
    if !path.exists() {
        return HashMap::new();
    }
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<HashMap<String, StoredQueuedRun>>(&raw).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

/// Convert the live queue into its serializable form. Call this **while holding the
/// `queued` lock**; serialize and write the result only after releasing it (S11).
fn snapshot_queued(queued: &HashMap<String, QueuedRun>) -> HashMap<String, StoredQueuedRun> {
    let mut stored: HashMap<String, StoredQueuedRun> = HashMap::new();
    for (job_id, run) in queued {
        let plan = match &run.plan {
            QueuedPlan::Task(task) => StoredQueuedPlan::Task {
                task: task.task.clone(),
            },
            QueuedPlan::Batch(batch) => StoredQueuedPlan::Batch {
                batch: batch.batch.clone(),
                from_step: batch.from_step,
                snapshot: batch.snapshot.clone(),
                produced: batch.produced.clone(),
            },
        };
        stored.insert(
            job_id.clone(),
            StoredQueuedRun {
                job_id: job_id.clone(),
                project_id: run.project_id.clone(),
                client_id: run.client_id.clone(),
                plan,
            },
        );
    }
    stored
}

fn save_queued_runs(path: &std::path::Path, stored: &HashMap<String, StoredQueuedRun>) {
    match serde_json::to_string_pretty(stored) {
        Ok(json) => {
            if let Some(parent) = path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    eprintln!("[WARN] queue persist: mkdir {}: {e}", parent.display());
                }
            }
            if let Err(e) = std::fs::write(path, json) {
                eprintln!("[WARN] queue persist: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("[WARN] queue persist: serialize: {e}"),
    }
}

/// Start the head of the queue once the project frees up (design doc §7.6.6).
///
/// Only one at a time — a project allows just one job (§7.5). Three places call this:
/// a new enqueue request, the end of a previous job, and after cancelling a queued item. That is why the queue moves on by itself.
fn pump_queue(state: &Arc<WebState>, project_id: &str) {
    loop {
        // Atomically promote the queue head to running if nothing else runs; failure means the project/host is busy or the queue is empty
        let Some(job_id) = state
            .jobs
            .promote_next_with(project_id, |cid| state.is_host_locked_for_client(cid))
        else {
            return;
        };
        let Some(run) = state.take_queued(&job_id) else {
            // The record is still there but its "how to run" description is gone (leftover after a restart): mark it clearly and move on to the next one
            state.jobs.abandon(&job_id, "队列记录缺失,已跳过");
            continue;
        };
        let Some(project) = find_project(state, &run.project_id).ok() else {
            state.jobs.abandon(&job_id, "项目已不存在");
            continue;
        };
        let Some(client) = find_client(state, &run.client_id).ok() else {
            state.jobs.abandon(&job_id, "客户机已不存在");
            continue;
        };
        match run.plan {
            QueuedPlan::Task(task) => run_task(state.clone(), project, client, *task),
            QueuedPlan::Batch(batch) => run_batch(state.clone(), project, client, *batch),
        }
        return;
    }
}

/// Pump every queue (design doc §7.6.6).
///
/// With host-level mutual exclusion enabled, "which machine became free" may let a queued item of
/// **another project** go — so on completion we cannot pump only the project that just finished. When the
/// host lock is off each project is pumped independently and the behavior is identical to pumping one.
pub fn pump_queues(state: &Arc<WebState>) {
    for project_id in state.jobs.projects_with_pending() {
        pump_queue(state, &project_id);
    }
}

/// Which client and which project one execution lands on (both `prepare_*` functions need this pair).
struct Target<'a> {
    project: &'a Project,
    client: &'a Client,
}

/// Create a single-task job record and register what it needs (step plan + input snapshot).
///
/// `queued` = record it as queued (no project-level mutual exclusion check); otherwise it is the immediate-execution path.
fn prepare_task_job(
    state: &Arc<WebState>,
    target: Target<'_>,
    task: &Task,
    source: &str,
    queued: bool,
) -> Result<TaskRun, ApiError> {
    let Target { project, client } = target;
    let title = task.title();
    let kind = kind_static(task.kind());
    let handle = if queued {
        state
            .jobs
            .start_queued(&project.id, &client.id, kind, &title, None, source)
    } else {
        state.start_job_sourced(&project.id, &client.id, kind, &title, None, source)?
    };
    let job_id = handle.id.clone();
    let cancel = Cancel::from_flag(handle.cancel);
    log::bind_sources(
        &job_scopes(&client.id, &job_id),
        &state.jobs.source(&job_id),
    );
    // A single task also registers one step record, so the frontend does not need two renderers for the two shapes
    state.jobs.plan_steps(
        &job_id,
        &[(0, task.id.clone(), title, task.kind().to_string())],
    );
    // Input snapshot (design doc §7.6.7): record which version of the input values this run actually used.
    // Resolution failures are not caught here — `execute_task` reports them with a more complete message.
    if let Ok(values) = contract::resolve_inputs(task, None) {
        let mut snapshot = BTreeMap::new();
        contract::merge_snapshot(&mut snapshot, &task.id, &values);
        state.jobs.set_inputs(&job_id, snapshot);
    }
    Ok(TaskRun {
        task: task.clone(),
        job_id,
        cancel,
    })
}

/// Run a single task; on completion write the result into the job record and the project state.
fn run_task(state: Arc<WebState>, project: Project, client: Client, run: TaskRun) {
    let TaskRun {
        task,
        job_id,
        cancel,
    } = run;
    let scopes = job_scopes(&client.id, &job_id);
    let t = i18n::text(state.lang());
    let title = task.title();
    let kind = task.kind().to_string();

    log::log_multi(
        &scopes,
        log::INFO,
        format!("{}: {} ({})", t.build_started, title, project.remote_path),
    );

    tokio::spawn(async move {
        state.jobs.begin_step(&job_id, 0);
        let outcome = execute_task(&state, &project, &client, &task, None, &scopes, &cancel).await;
        let cancelled = outcome.cancelled || cancel.requested();
        let ok = outcome.ok && !cancelled;

        state.jobs.finish_step(
            &job_id,
            0,
            ok,
            cancelled,
            &outcome.message,
            outcome.inputs.clone(),
            outcome.outputs.clone(),
            outcome.exit_code,
            outcome.code.unwrap_or(""),
        );
        state.jobs.finish(
            &job_id,
            ok,
            cancelled,
            &outcome.message,
            outcome.exit_code,
            &outcome.artifacts,
            outcome.files,
            outcome.bytes,
        );

        log::log_multi(
            &scopes,
            if ok {
                log::INFO
            } else if cancelled {
                log::WARN
            } else {
                log::ERROR
            },
            format!("{}: {}", t.build_finished, outcome.message),
        );

        if kind == projects::KIND_BUILD || !outcome.artifacts.is_empty() {
            let artifacts = outcome.artifacts.clone();
            let is_build = kind == projects::KIND_BUILD;
            let mut list = crate::lock::write(&state.projects);
            if let Some(entry) = list.iter_mut().find(|p| p.id == project.id) {
                if is_build {
                    entry.last_build_at = projects::now_ms();
                    entry.last_build_ok = ok;
                    entry.last_build_message = outcome.message.clone();
                }
                if ok && !artifacts.is_empty() {
                    entry.last_artifact = artifacts.join("\n");
                }
            }
            drop(list);
            let _ = state.persist_projects();
        }

        // Execution right for this project has been released — run the next one if the queue is not empty (design doc §7.6.6)
        pump_queues(&state);
    });
}

// ------------------------------------------------------------------ batch workflows

/// Request body of `POST /api/projects/{id}/batches` (design doc §7.3).
#[derive(Deserialize, Default, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", default)]
#[schemars(extend("required" = ["steps"]))]
pub(crate) struct BatchInput {
    /// Empty = create new; an existing id = overwrite that entry.
    pub id: String,
    /// Display name; when empty, `batch` is used.
    pub name: String,
    /// Steps executed in order, **at least one** (CLI / WebUI / API all agree).
    #[schemars(length(min = 1))]
    pub steps: Vec<BatchStep>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct BatchRunBody {
    /// Step to start from (design doc §7.3, "retry from the failed step"). Default = from the beginning.
    from_step: Option<usize>,
}

async fn api_save_batch(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<BatchInput>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;

        // An empty batch is meaningless (CLI and WebUI both refuse to create one, while the API used to
        // silently accept a batch that does nothing — inconsistency across the three is worse than being
        // permissive), and an empty taskId used to be dropped silently; now it is an error.
        // The rules share one function with `PATCH` so the two sides cannot drift.
        let existing = project
            .batches
            .iter()
            .position(|batch| !input.id.is_empty() && batch.id == input.id);
        let batch_id = match existing {
            Some(index) => project.batches[index].id.clone(),
            None => projects::new_batch_id(),
        };

        let batch = Batch {
            id: batch_id,
            name: if input.name.trim().is_empty() {
                "batch".to_string()
            } else {
                input.name.trim().to_string()
            },
            steps: input.steps,
        };
        validate_batch(&batch, project)?;

        if batch.steps.is_empty() {
            return Err(ApiError::bad_request("batchEmpty", "steps is empty"));
        }

        match existing {
            Some(index) => project.batches[index] = batch,
            None => project.batches.push(batch),
        }
    }

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok(Json(json!({ "ok": true, "project": project_json(&state, &id)? })))
}

async fn api_delete_batch(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, batch_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;

    {
        let mut list = crate::lock::write(&state.projects);
        let project = list
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| ApiError::not_found("projectNotFound"))?;
        let before = project.batches.len();
        project.batches.retain(|batch| batch.id != batch_id);
        if project.batches.len() == before {
            return Err(ApiError::not_found("batchNotFound"));
        }
    }

    state
        .persist_projects()
        .map_err(|e| ApiError::internal(&e.to_string()))?;
    Ok(Json(json!({ "ok": true, "project": project_json(&state, &id)? })))
}

async fn api_run_batch(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((id, batch_id)): Path<(String, String)>,
    body: Option<Json<BatchRunBody>>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let project = find_project(&state, &id)?;
    let client = find_client(&state, &project.client_id)?;
    let batch = project
        .batch(&batch_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("batchNotFound"))?;

    let from_step = body.and_then(|Json(body)| body.from_step).unwrap_or(0);
    if from_step >= batch.steps.len() {
        return Err(ApiError::bad_request(
            "stepOutOfRange",
            format!("fromStep={from_step} 超出 batch 的 {} 步", batch.steps.len()),
        ));
    }

    let run = prepare_batch_job(
        &state,
        Target {
            project: &project,
            client: &client,
        },
        &batch,
        from_step,
        BatchResume::default(),
        &request_source(&headers),
        false,
    )?;
    let job_id = run.job_id.clone();
    run_batch(state.clone(), project.clone(), client, run);
    Ok(Json(json!({
        "ok": true,
        "jobId": job_id,
        "project": project.to_json(),
    })))}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct RetryBody {
    /// Explicitly start from the beginning; default = start from the failed step (design doc §7.3).
    from_step: Option<usize>,
    from_start: bool,
}

async fn api_retry_job(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    body: Option<Json<RetryBody>>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let body = body.map(|Json(body)| body).unwrap_or_default();

    let (project_id, batch_id, _title, retry_from) = state
        .jobs
        .meta(&job_id)
        .ok_or_else(|| ApiError::not_found("jobNotFound"))?;

    let project = find_project(&state, &project_id)?;
    let client = find_client(&state, &project.client_id)?;

    let from_step = if body.from_start {
        0
    } else {
        body.from_step.unwrap_or(retry_from)
    };

    // A single task without a batch: just re-run the same task
    let Some(batch_id) = batch_id else {
        return Err(ApiError::bad_request("notABatch", job_id));
    };
    let batch = project
        .batch(&batch_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("batchNotFound"))?;

    if from_step >= batch.steps.len() {
        return Err(ApiError::bad_request(
            "stepOutOfRange",
            format!("fromStep={from_step} 超出 batch 的 {} 步", batch.steps.len()),
        ));
    }

    // Input snapshot (design doc §7.6.7): a retry reuses the **previous** version of the input values and does not follow the task variables.
    // Step outputs (design doc §7.6.5): the earlier steps do not run again this round, and reference resolution relies on this record.
    let resume = BatchResume {
        snapshot: state.jobs.inputs(&job_id).unwrap_or_default(),
        produced: state.jobs.produced(&job_id),
    };
    let run = prepare_batch_job(
        &state,
        Target {
            project: &project,
            client: &client,
        },
        &batch,
        from_step,
        resume,
        &request_source(&headers),
        false,
    )?;
    let new_job = run.job_id.clone();
    run_batch(state.clone(), project.clone(), client, run);
    Ok(Json(json!({
        "ok": true,
        "jobId": new_job,
        "retriedFrom": from_step,
        "project": project.to_json(),
    })))
}

/// The two records handed back on a batch retry (design doc §7.6.5 / §7.6.7).
///
/// On a first execution both are empty; `retry` reads them back from the previous job —
/// changing only one of them (say, restoring only the input snapshot) would resolve references to two
/// inconsistent sets of paths, so they are passed together as one unit.
#[derive(Default)]
struct BatchResume {
    /// Input values used last time (§7.6.7).
    snapshot: BTreeMap<String, String>,
    /// Step outputs already verified last time (§7.6.5).
    produced: BTreeMap<String, String>,
}

/// Create a batch job record and register what it needs (step plan + input snapshot + produced record).
///
/// `queued` = record it as queued (no project-level mutual exclusion check); otherwise it is the immediate-execution path.
fn prepare_batch_job(
    state: &Arc<WebState>,
    target: Target<'_>,
    batch: &Batch,
    from_step: usize,
    resume: BatchResume,
    source: &str,
    queued: bool,
) -> Result<BatchRun, ApiError> {
    let Target { project, client } = target;
    let title = if from_step > 0 {
        format!("{} (从第 {} 步重试)", batch.name, from_step + 1)
    } else {
        batch.name.clone()
    };

    let handle = if queued {
        state.jobs.start_queued(
            &project.id,
            &client.id,
            "batch",
            &title,
            Some(batch.id.clone()),
            source,
        )
    } else {
        state.start_job_sourced(
            &project.id,
            &client.id,
            "batch",
            &title,
            Some(batch.id.clone()),
            source,
        )?
    };
    let job_id = handle.id.clone();
    // Design doc §10: every log of the whole batch carries the initiator
    log::bind_sources(&job_scopes(&client.id, &job_id), &state.jobs.source(&job_id));

    // Register only the steps to be run, but keep their indices aligned with the batch.
    let mut plan: Vec<(usize, String, String, String)> = Vec::new();
    for (index, step) in batch.steps.iter().enumerate().skip(from_step) {
        match project.task(&step.task_id) {
            Some(task) => plan.push((
                index,
                task.id.clone(),
                task.title(),
                task.kind().to_string(),
            )),
            None => {
                // Validated when the batch was saved; this is a safety net
                plan.push((
                    index,
                    step.task_id.clone(),
                    step.task_id.clone(),
                    "missing".to_string(),
                ));
            }
        }
    }
    state.jobs.plan_steps(&job_id, &plan);

    // Input snapshot (design doc §7.6.7): on a retry reuse the one passed in; on a first execution resolve a fresh one.
    // Stored as `<taskId>.<input name>`, one task per step, so each step reads its own.
    let effective_snapshot = match Some(resume.snapshot.clone()).filter(|map| !map.is_empty()) {
        Some(values) => values,
        None => {
            let mut all = BTreeMap::new();
            for (index, step) in batch.steps.iter().enumerate().skip(from_step) {
                let Some(task) = project.task(&step.task_id) else {
                    continue;
                };
                let mut task = task.clone();
                // Step-level variable overrides are part of the inputs; the snapshot must record the ones actually used
                if let Some(step) = batch.steps.get(index) {
                    for (key, value) in &step.variables {
                        task.variables.insert(key.clone(), value.clone());
                    }
                }
                if let Ok(values) = contract::resolve_inputs(&task, None) {
                    // Values that still contain step references are **not** recorded in the snapshot yet: they
                    // can only be resolved after the earlier step has actually run (design doc §7.6.5).
                    // Recording an unexpanded template would be worse — the snapshot takes precedence over
                    // `variables` and would override the value expanded at run time.
                    let resolved_now: BTreeMap<String, String> = values
                        .into_iter()
                        .filter(|(_, value)| !value.contains("{{steps."))
                        .collect();
                    contract::merge_snapshot(&mut all, &task.id, &resolved_now);
                }
            }
            all
        }
    };
    state
        .jobs
        .set_inputs(&job_id, effective_snapshot.clone());
    state.jobs.set_produced(&job_id, resume.produced.clone());

    Ok(BatchRun {
        batch: batch.clone(),
        from_step,
        job_id,
        cancel: Cancel::from_flag(handle.cancel),
        snapshot: effective_snapshot,
        produced: resume.produced,
    })
}

/// Execute a batch job in order (design doc §7.3):
/// the whole batch holds one project-level lock (guaranteed at job creation), stops at any failing step, and marks later steps as skipped.
fn run_batch(state: Arc<WebState>, project: Project, client: Client, run: BatchRun) {
    let BatchRun {
        batch,
        from_step,
        job_id,
        cancel,
        snapshot: effective_snapshot,
        produced,
    } = run;
    let scopes = job_scopes(&client.id, &job_id);
    let t = i18n::text(state.lang());

    log::log_multi(
        &scopes,
        log::INFO,
        format!(
            "{}: {}({} 步,从第 {} 步开始)",
            t.batch_started,
            batch.name,
            batch.steps.len(),
            from_step + 1
        ),
    );

    tokio::spawn(async move {
        // Step outputs already verified; later steps use them to resolve `{{steps.x.outputs.y}}` (§7.6.5)
        let mut produced = produced;
        let mut ok = true;
        let mut cancelled = false;
        let mut first_failure: Option<(usize, String)> = None;
        let mut artifacts: Vec<String> = Vec::new();
        let mut total_files = 0u64;
        let mut total_bytes = 0u64;

        for (index, step) in batch.steps.iter().enumerate().skip(from_step) {
            if cancel.requested() {
                cancelled = true;
                state.jobs.skip_steps_from(&job_id, index);
                break;
            }

            let Some(task) = project.task(&step.task_id).cloned() else {
                ok = false;
                state.jobs.begin_step(&job_id, index);
                state.jobs.finish_step(
                    &job_id,
                    index,
                    false,
                    false,
                    "任务已不存在",
                    Vec::new(),
                    Vec::new(),
                    -1,
                    "taskNotFound",
                );
                state.jobs.skip_steps_from(&job_id, index + 1);
                first_failure = Some((index, format!("第 {} 步:任务不存在", index + 1)));
                break;
            };

            // Step-level variable overrides (build tasks) + step output references (design doc §7.6.5).
            // A referenced value must be a real path **verified by an earlier step** — if it cannot be resolved,
            // fail right here instead of feeding the literal `{{steps.…}}` to the task (which would fail far from the scene).
            let mut task = task;
            if !step.variables.is_empty() {
                let mut failure: Option<String> = None;
                for (key, value) in &step.variables {
                    match contract::expand_steps(value, &produced) {
                        Ok(expanded) => {
                            task.variables.insert(key.clone(), expanded);
                        }
                        Err(err) => {
                            failure = Some(err);
                            break;
                        }
                    }
                }
                if let Some(err) = failure {
                    ok = false;
                    state.jobs.begin_step(&job_id, index);
                    state.jobs.finish_step(
                        &job_id,
                        index,
                        false,
                        false,
                        &err,
                        Vec::new(),
                        Vec::new(),
                        -1,
                        "stepRefMissing",
                    );
                    state.jobs.skip_steps_from(&job_id, index + 1);
                    first_failure = Some((index, format!("第 {} 步:{err}", index + 1)));
                    break;
                }
            }

            state.jobs.begin_step(&job_id, index);
            log::log_multi(
                &scopes,
                log::INFO,
                format!("[{}/{}] {} ({})", index + 1, batch.steps.len(), task.title(), task.kind()),
            );

            let step_snapshot = contract::snapshot_for(&task.id, &effective_snapshot);
            let outcome = execute_task(
                &state,
                &project,
                &client,
                &task,
                Some(&step_snapshot),
                &scopes,
                &cancel,
            )
            .await;
            let step_cancelled = outcome.cancelled || cancel.requested();
            let step_ok = outcome.ok && !step_cancelled;
            total_files += outcome.files;
            total_bytes += outcome.bytes;

            state.jobs.finish_step(
                &job_id,
                index,
                step_ok,
                step_cancelled,
                &outcome.message,
                outcome.inputs.clone(),
                outcome.outputs.clone(),
                outcome.exit_code,
                outcome.code.unwrap_or(""),
            );
            log::log_multi(
                &scopes,
                if step_ok {
                    log::INFO
                } else if step_cancelled {
                    log::WARN
                } else {
                    log::ERROR
                },
                format!(
                    "[{}/{}] {}: {}",
                    index + 1,
                    batch.steps.len(),
                    task.title(),
                    outcome.message
                ),
            );

            if step_ok {
                // Publish this step's **verified** outputs for later steps to reference (design doc §7.6.5).
                // Only those that really passed verification count — declared outputs that produced nothing are not in `outcome.outputs`.
                if !step.id.trim().is_empty() {
                    if let Ok(values) = contract::resolve_inputs(&task, Some(&step_snapshot)) {
                        if let Ok(declared) = contract::resolved_outputs(&task, &values) {
                            for item in declared {
                                let label = format!("{}:{}", item.side.code(), item.path);
                                if outcome.outputs.iter().any(|seen| seen == &label) {
                                    let key = format!("{}.outputs.{}", step.id.trim(), item.name);
                                    produced.insert(key.clone(), item.path.clone());
                                    state.jobs.publish_output(&job_id, key, item.path.clone());
                                }
                            }
                        }
                    }
                    if task.kind() == projects::KIND_BUILD {
                        if let Ok(manifest) = manifest::load(&state.config_path, &task.manifest) {
                            for manifest_step in &manifest.build.steps {
                                if let (Some(step_id), Some(outputs)) = (manifest_step.id(), manifest_step.outputs()) {
                                    for (out_name, spec) in outputs {
                                        let rendered = manifest::render_template(&manifest, &task.variables, &spec.path);
                                        let trimmed = rendered.trim();
                                        let label = format!("remote:{trimmed}");
                                        if outcome.outputs.iter().any(|seen| seen == &label) {
                                            let key_task = format!("{}.outputs.{}", step.id.trim(), out_name);
                                            produced.insert(key_task.clone(), trimmed.to_string());
                                            state.jobs.publish_output(&job_id, key_task, trimmed.to_string());

                                            let key_step = format!("{}.outputs.{}", step_id.trim(), out_name);
                                            produced.insert(key_step.clone(), trimmed.to_string());
                                            state.jobs.publish_output(&job_id, key_step, trimmed.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                artifacts.extend(outcome.artifacts);
                continue;
            }

            cancelled = step_cancelled;
            ok = false;
            first_failure = Some((index, outcome.message.clone()));
            state.jobs.skip_steps_from(&job_id, index + 1);
            break;
        }

        let message = if ok {
            format!("{}: {}", t.batch_finished, batch.name)
        } else if cancelled {
            format!("{}: {}", t.batch_cancelled, batch.name)
        } else {
            match &first_failure {
                Some((index, detail)) => format!(
                    "{}: 第 {} 步失败 - {detail}",
                    t.batch_failed,
                    index + 1
                ),
                None => t.batch_failed.to_string(),
            }
        };

        state.jobs.finish(
            &job_id,
            ok,
            cancelled,
            &message,
            if ok { 0 } else { -1 },
            &artifacts,
            total_files,
            total_bytes,
        );
        log::log_multi(
            &scopes,
            if ok {
                log::INFO
            } else if cancelled {
                log::WARN
            } else {
                log::ERROR
            },
            message.clone(),
        );

        if !artifacts.is_empty() {
            let mut list = crate::lock::write(&state.projects);
            if let Some(entry) = list.iter_mut().find(|p| p.id == project.id) {
                entry.last_artifact = artifacts.join("\n");
            }
            drop(list);
            let _ = state.persist_projects();
        }

        // Execution right released — run the next one if the queue is not empty (design doc §7.6.6)
        pump_queues(&state);
    });
}

// ------------------------------------------------------------------ job (design doc §10.1)

/// Logs of execution tasks go to both the "instance page" and the "job page".
fn job_scopes(client_id: &str, job_id: &str) -> Vec<String> {
    vec![clients::log_scope(client_id), jobs::log_scope(job_id)]
}

async fn api_list_jobs(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    Ok(Json(json!({ "jobs": state.jobs.list() })))
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ClearJobsQuery {
    project_id: Option<String>,
}

async fn api_clear_jobs(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<ClearJobsQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    let pruned = state
        .jobs
        .prune_finished(query.project_id.as_deref().map(str::trim).filter(|s| !s.is_empty()));
    Ok(Json(json!({ "ok": true, "pruned": pruned })))
}

async fn api_get_job(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    state
        .jobs
        .get(&job_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("jobNotFound"))
}

async fn api_job_logs(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    if state.jobs.get(&job_id).is_none() {
        return Err(ApiError::not_found("jobNotFound"));
    }

    let entries: Vec<_> = log::core()
        .snapshot(&jobs::log_scope(&job_id))
        .into_iter()
        .filter(|entry| entry.seq > query.since)
        .collect();

    Ok(Json(json!({ "entries": entries })))
}

async fn api_cancel_job(
    State(state): State<Arc<WebState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    authorized(&state, &headers, addr)?;
    // A queued job takes a different path: no executor is waiting on the `cancel` flag,
    // so what is needed is "remove it from the queue + mark it cancelled explicitly" (design doc §7.6.6 item 3).
    if state.jobs.state_of(&job_id) == Some(jobs::JobState::Pending) {
        state.drop_queued(&job_id);
        if state.jobs.abandon(&job_id, "排队中被取消") {
            return match state.jobs.get(&job_id) {
                Some(job) => Ok(Json(json!({ "ok": true, "job": job }))),
                None => Err(ApiError::not_found("jobNotFound")),
            };
        }
    }
    match state.jobs.request_cancel(&job_id) {
        Ok(job) => Ok(Json(json!({ "ok": true, "job": job }))),
        Err(jobs::CancelError::NotFound) => Err(ApiError::not_found("jobNotFound")),
        Err(jobs::CancelError::Finished) => Err(ApiError::bad_request(
            "jobFinished",
            "这个 job 已经结束了",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn temp_state(name: &str) -> (Arc<WebState>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cgw-server-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");
        let state = Arc::new(WebState::new(
            config_path,
            Config::default(),
            Lang::EnUS,
            1,
            "http://localhost:1".to_string(),
            None,
        ));
        (state, dir)
    }

    /// Design doc §7.5: the same project may not run concurrently; different projects do not affect each other.
    /// The project lock must be acquirable again after a job ends.
    #[test]
    fn project_lock_rejects_concurrent_runs() {
        let (state, dir) = temp_state("lock");

        let first = state.start_job("p-1", "c-1", "build", "t");
        assert!(first.is_ok(), "the first one should get the lock");
        let first = match first {
            Ok(handle) => handle,
            Err(_) => panic!("the first one should get the lock"),
        };

        match state.start_job("p-1", "c-1", "push", "t") {
            Ok(_) => panic!("the same project should not run concurrently"),
            Err(err) => {
                assert_eq!(err.code, "projectBusy");
                assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
            }
        }

        // Other projects are unaffected
        assert!(state.start_job("p-2", "c-1", "build", "t").is_ok());

        // It can be acquired again after the job ends
        state
            .jobs
            .finish(&first.id, true, false, "done", 0, &[], 0, 0);
        assert!(state.start_job("p-1", "c-1", "pull", "t").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Design doc §7.6.6: when the project is busy, enqueue joins the queue and, once it frees up, starts **by itself** in FIFO order.
    ///
    /// A manually registered running job deliberately holds the project here — this does not rely on "who runs
    /// faster", so the case is deterministic on any machine.
    #[test]
    fn enqueued_run_waits_for_the_project_then_starts_itself() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, dir) = temp_state("queue");

            // A client that cannot be reached: push fails fast, but the whole job flow runs through completely
            let client = Client {
                id: "c-q".to_string(),
                name: "q".to_string(),
                host: "127.0.0.1".to_string(),
                port: 1,
                ..Client::default()
            };
            crate::lock::write(&state.clients).push(client.clone());

            let source = dir.join("src");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("a.txt"), b"hi").unwrap();

            let task = Task {
                id: "t-push".to_string(),
                name: "q push".to_string(),
                kind: projects::KIND_PUSH.to_string(),
                ..Task::default()
            };
            let project = Project {
                id: "p-q".to_string(),
                name: "q".to_string(),
                client_id: "c-q".to_string(),
                source_path: source.display().to_string(),
                remote_path: "/tmp/q".to_string(),
                tasks: vec![task.clone()],
                ..Project::default()
            };
            crate::lock::write(&state.projects).push(project.clone());

            // Hold the project manually (no timing dependency)
            let holder = match state.start_job("p-q", "c-q", "build", "holder") {
                Ok(handle) => handle,
                Err(err) => panic!("the placeholder job should start:{}", err.code),
            };

            // Enqueue: the project is busy, so it must stay pending instead of erroring
            let run = prepare_task_job(
                &state,
                Target {
                    project: &project,
                    client: &client,
                },
                &task,
                "webui",
                true,
            )
            .map_err(|err| err.code)
            .expect("enqueue must not be blocked by mutual exclusion");
            let queued_id = run.job_id.clone();
            state.enqueue_run(QueuedRun {
                project_id: project.id.clone(),
                client_id: client.id.clone(),
                plan: QueuedPlan::Task(Box::new(run)),
            });
            pump_queues(&state);
            assert_eq!(
                state.jobs.state_of(&queued_id),
                Some(jobs::JobState::Pending),
                "the project is busy, the queued item must not start"
            );
            assert_eq!(state.jobs.queue_position(&queued_id), Some(1));
            assert!(state.queued_json()["p-q"].is_array(), "queued items must be listable");

            // The placeholder job ends -> the queue head promotes itself
            state
                .jobs
                .finish(&holder.id, true, false, "done", 0, &[], 0, 0);
            pump_queues(&state);
            assert_eq!(
                state.jobs.state_of(&queued_id),
                Some(jobs::JobState::Running),
                "once the project frees up, the queued item should start"
            );

            // Wait for it to really finish (unreachable target -> failed, but the flow ran to completion)
            for _ in 0..400 {
                if state
                    .jobs
                    .state_of(&queued_id)
                    .map(|state| state.is_terminal())
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            let job = state.jobs.get(&queued_id).unwrap();
            assert_eq!(job["state"], "failed", "{job}");
            assert!(job["finishedAt"].as_u64().unwrap() > 0);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// What host-level mutual exclusion looks like at the **API layer**: 409 `hostBusy`, carrying clientId / holding project / job
    /// (design doc §7.6.6, end). The lock's own queueing and promotion behavior is covered in
    /// `jobs::tests::host_lock_serialises_projects_on_one_client`.
    #[test]
    fn host_lock_reports_host_busy_with_the_holder() {
        let (state, dir) = temp_state("hostlock");
        crate::lock::write(&state.config).host_lock = true;

        let holder = match state.start_job("p-1", "c-1", "build", "t") {
            Ok(handle) => handle,
            Err(err) => panic!("the first project should start:{}", err.code),
        };

        match state.start_job("p-2", "c-1", "build", "t") {
            Ok(_) => panic!("a second project must not be admitted on the same client machine"),
            Err(err) => {
                assert_eq!(err.code, "hostBusy", "must be distinct from projectBusy");
                assert_eq!(err.status, axum::http::StatusCode::CONFLICT);
                let extra = err.extra.expect("structured info must be included");
                assert_eq!(extra["clientId"], "c-1");
                assert_eq!(extra["projectId"], "p-1");
                assert_eq!(extra["jobId"], holder.id.as_str());
            }
        }

        // Another client machine still runs (the lock is on the machine, not the project)
        assert!(state.start_job("p-2", "c-2", "build", "t").is_ok());

        // Once turned off the same request goes through — the switch is read live, no restart needed
        crate::lock::write(&state.config).host_lock = false;
        assert!(state.start_job("p-3", "c-1", "build", "t").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Per-client host lock: only the client with `host_lock: true` serialises jobs.
    #[test]
    fn per_client_host_lock_reports_host_busy() {
        let (state, dir) = temp_state("clienthostlock");
        crate::lock::write(&state.config).host_lock = false;

        {
            let mut list = crate::lock::write(&state.clients);
            list.push(Client {
                id: "c-locked".to_string(),
                name: "locked client".to_string(),
                host_lock: true,
                ..Default::default()
            });
            list.push(Client {
                id: "c-unlocked".to_string(),
                name: "unlocked client".to_string(),
                host_lock: false,
                ..Default::default()
            });
        }

        let holder = match state.start_job("p-1", "c-locked", "build", "t") {
            Ok(handle) => handle,
            Err(err) => panic!("first job on locked client should start: {}", err.code),
        };

        match state.start_job("p-2", "c-locked", "build", "t") {
            Ok(_) => panic!("second project must not be admitted on locked client"),
            Err(err) => {
                assert_eq!(err.code, "hostBusy");
                let extra = err.extra.expect("extra info");
                assert_eq!(extra["clientId"], "c-locked");
                assert_eq!(extra["projectId"], "p-1");
                assert_eq!(extra["jobId"], holder.id.as_str());
            }
        }

        assert!(state.start_job("p-3", "c-unlocked", "build", "t").is_ok());
        assert!(state.start_job("p-4", "c-unlocked", "build", "t").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Push excludes = the project's own + the fixed blacklist (§6.5) + the manifest's cache.paths (§6.2).
    #[test]
    fn effective_excludes_merges_builtin_and_cache() {
        let (state, dir) = temp_state("excludes");
        manifest::write_raw(
            &state.config_path,
            "t",
            r#"{"displayName":"t","build":{"steps":["echo x"]},"cache":{"paths":["ios/Pods/"]}}"#,
        )
        .unwrap();

        let mut project = Project::default();
        project.excludes = vec![".git".to_string()];
        project.tasks.push(Task {
            kind: projects::KIND_BUILD.to_string(),
            manifest: "t".to_string(),
            ..Default::default()
        });

        let excludes = effective_excludes(&state, &project);
        assert!(excludes.contains(&".git".to_string()), "the project's own excludes");
        assert!(
            excludes.contains(&"DerivedData".to_string()),
            "host-local artifacts are never pushed"
        );
        assert!(
            excludes.contains(&"ios/Pods/".to_string()),
            "the manifest's cache.paths must take part in excludes"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queued_jobs_survive_daemon_restart_and_recover() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, dir) = temp_state("queuerestore");
            let config_path = state.config_path.clone();
            let config = crate::lock::read(&state.config).clone();
            let lang = state.lang();
            let port = state.port;
            let url = crate::lock::read(&state.url).clone();

            let client = Client {
                id: "c-rec".to_string(),
                name: "recovery client".to_string(),
                ..Default::default()
            };
            let project = Project {
                id: "p-rec".to_string(),
                client_id: "c-rec".to_string(),
                name: "recovery project".to_string(),
                tasks: vec![Task {
                    id: "t-copy".to_string(),
                    kind: projects::KIND_COPY.to_string(),
                    name: "copy task".to_string(),
                    from: paths::Location {
                        side: paths::Side::Host,
                        path: "src".to_string(),
                    },
                    to: paths::Location {
                        side: paths::Side::Remote,
                        path: "dst".to_string(),
                    },
                    ..Default::default()
                }],
                ..Default::default()
            };
            crate::lock::write(&state.clients).push(client.clone());
            crate::lock::write(&state.projects).push(project.clone());
            state.persist_clients().unwrap();
            state.persist_projects().unwrap();

            // 1. Occupy project with a holder job
            let holder = state.start_job("p-rec", "c-rec", "build", "holding").unwrap();

            // 2. Prepare and enqueue a task job
            let target = Target {
                project: &project,
                client: &client,
            };
            let run = prepare_task_job(&state, target, &project.tasks[0], "test", true).unwrap();
            let queued_id = run.job_id.clone();
            state.enqueue_run(QueuedRun {
                project_id: project.id.clone(),
                client_id: client.id.clone(),
                plan: QueuedPlan::Task(Box::new(run)),
            });

            assert_eq!(state.jobs.state_of(&queued_id), Some(jobs::JobState::Pending));
            let queue_file = queue_store_path(&config_path);
            assert!(queue_file.exists(), "queued_runs.json must be persisted");

            // 3. Drop state (simulating shutdown)
            drop(state);

            // 4. Reboot daemon: create new WebState from same config
            let restarted = Arc::new(WebState::new(
                config_path,
                config,
                lang,
                port,
                url,
                None,
            ));

            // 5. Verify pending job is recovered, not cancelled
            assert_eq!(
                restarted.jobs.state_of(&queued_id),
                Some(jobs::JobState::Pending),
                "pending job must survive daemon restart"
            );
            assert_eq!(
                restarted.jobs.queue_position(&queued_id),
                Some(1),
                "queue position must be restored"
            );

            // 6. Free project and pump queues
            restarted.jobs.finish(&holder.id, true, false, "done", 0, &[], 0, 0);
            pump_queues(&restarted);

            assert_eq!(
                restarted.jobs.state_of(&queued_id),
                Some(jobs::JobState::Running),
                "queued job should be promoted to running after queue recovery"
            );

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn batch_step_outputs_published_from_manifest_steps() {
        let (state, dir) = temp_state("stepoutputs");
        manifest::write_raw(
            &state.config_path,
            "m-step",
            r#"{
                "displayName": "Step Output Test",
                "variables": [
                    { "name": "flavor", "default": "prod" }
                ],
                "build": {
                    "steps": [
                        {
                            "id": "link",
                            "run": "echo linking",
                            "outputs": {
                                "app": "out/{{flavor}}/App.app"
                            }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let manifest = manifest::load(&state.config_path, "m-step").unwrap();
        assert_eq!(manifest.build.steps.len(), 1);
        let step = &manifest.build.steps[0];
        let outputs = step.outputs().unwrap();
        assert_eq!(outputs["app"].path, "out/{{flavor}}/App.app");

        // Verify placeholder expansion
        let mut vars = BTreeMap::new();
        vars.insert("flavor".to_string(), "prod".to_string());
        let rendered = manifest::render_template(&manifest, &vars, &outputs["app"].path);
        assert_eq!(rendered, "out/prod/App.app");

        // Verify produced chaining
        let mut produced = BTreeMap::new();
        produced.insert("build.outputs.app".to_string(), rendered.clone());
        produced.insert("link.outputs.app".to_string(), rendered);

        let resolved_by_task = contract::expand_steps("{{steps.build.outputs.app}}", &produced).unwrap();
        assert_eq!(resolved_by_task, "out/prod/App.app");

        let resolved_by_step = contract::expand_steps("{{steps.link.outputs.app}}", &produced).unwrap();
        assert_eq!(resolved_by_step, "out/prod/App.app");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn api_get_env_script_returns_probe_script() {
        let (state, dir) = temp_state("env-script");
        let addr = "127.0.0.1:12345".parse().unwrap();
        let headers = HeaderMap::new();
        let res = api_get_env_script(State(state), ConnectInfo(addr), headers).await.unwrap();
        assert_eq!(res.0["ok"], true);
        let script = res.0["script"].as_str().unwrap();
        assert!(!script.is_empty());
        assert!(script.contains("uname -s"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

