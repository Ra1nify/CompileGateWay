//! MCP adapter built into the daemon (design doc §3, §12 Phase 6).
//!
//! Rationale: the daemon already holds the only core logic, so the tool table exists
//! once, no extra Node process is needed, and the MCP port is a `config.json` entry
//! that applies without a restart.
//!
//! Transport is stateless Streamable HTTP at `POST /mcp`; `GET /mcp` returns 405, and
//! the tools carry no cross-request state, so no session can leak or go stale. Clients
//! that only speak stdio use the thin bridge in `mcp/`, which defines no tools itself.
//! Tool calls go over loopback HTTP (`X-Client: mcp`) to the daemon's own API, so auth,
//! log source marking, project locks and job semantics match WebUI / CLI exactly.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};

use crate::config::{Listen, Mcp};
use crate::httpclient;
use crate::jsonschema;
use crate::server::WebState;

/// Log source marker (design doc §10): the daemon uses it to flag AI-driven actions.
pub const SOURCE: &str = "mcp";

pub const SERVER_NAME: &str = "compilegateway";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Accepted MCP protocol versions. Echo the client's version when it is listed,
/// otherwise answer with the newest one. (The spec makes the server choose.)
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];
const RESOURCE_PREFIX: &str = "cgw://schema/";

// ==================================================================== Runtime

/// Runtime state of the MCP listener.
///
/// Unlike the WebUI port, which axum `serve()` owns and can only change on restart,
/// this listener runs on its own thread and runtime, so `POST /api/config` rebinds it
/// in place.
#[derive(Default)]
pub struct McpRuntime {
    inner: Mutex<Option<Running>>,
}

struct Running {
    port: u16,
    listen: Listen,
    /// Port from config (`None` = auto-pick); used to tell whether config changed.
    requested: Option<u16>,
    /// Set when auto-pick had to give way (default port busy); usually `None`.
    note: Option<String>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: std::thread::JoinHandle<()>,
}

/// Snapshot for `/api/status`, `/api/config` and the startup log.
#[derive(Debug, Clone)]
pub struct Status {
    /// Enabled in config.
    pub enabled: bool,
    /// Actually listening.
    pub running: bool,
    pub port: u16,
    pub listen: Listen,
    pub url: String,
    /// Why the last start failed (port in use, etc.).
    pub error: Option<String>,
    /// Not an error, but worth reporting (e.g. a port fallback).
    pub note: Option<String>,
}

impl Status {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "running": self.running,
            "port": self.port,
            "listen": self.listen.code(),
            "url": self.url,
            "endpoint": if self.running { format!("{}/mcp", self.url) } else { String::new() },
            "error": self.error,
            "note": self.note,
            "defaultPort": DEFAULT_PORT,
            "tools": tools().len(),
            "protocolVersions": PROTOCOL_VERSIONS,
        })
    }
}

impl McpRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(&self, config: &Mcp) -> Status {
        snapshot(&crate::lock::lock(&self.inner), config)
    }

    /// Reconcile the listener with the config; a no-op when nothing changed.
    ///
    /// Failures are returned instead of silently degraded, matching the daemon port:
    /// when a port is taken we say so rather than quietly picking another one.
    pub fn apply(&self, config: &Mcp, web: &Arc<WebState>, daemon_port: u16) -> Result<Status, String> {
        let mut slot = crate::lock::lock(&self.inner);

        if let Some(running) = slot.as_ref() {
            // Auto-picked ports have no concrete value to compare, so compare intent.
            let same = config.enabled
                && running.listen == config.listen
                && running.requested == config.requested_port();
            if same {
                return Ok(snapshot(&slot, config));
            }
        } else if !config.enabled {
            return Ok(snapshot(&slot, config));
        }

        // Remember the running config so a failed start can be rolled back.
        let previous = slot.as_ref().map(|running| Mcp {
            enabled: true,
            listen: running.listen,
            port: running.requested,
        });

        stop_running(slot.take());
        if !config.enabled {
            return Ok(snapshot(&slot, config));
        }

        match start(&mut slot, config, web, daemon_port) {
            Ok(()) => Ok(snapshot(&slot, config)),
            Err(err) => {
                // A failed start must not kill a working MCP: put the previous config
                // back. Typical case: the user set a port another program holds, which
                // means the new config is bad, not that MCP broke.
                if let Some(previous) = previous {
                    let _ = start(&mut slot, &previous, web, daemon_port);
                }
                Err(err)
            }
        }
    }

    /// Used on process exit (only tests and `--` need it today).
    pub fn stop(&self) {
        stop_running(crate::lock::lock(&self.inner).take());
    }
}

/// Bind the port and spawn the thread; `slot` is untouched on failure.
fn start(
    slot: &mut Option<Running>,
    config: &Mcp,
    web: &Arc<WebState>,
    daemon_port: u16,
) -> Result<(), String> {
    let (listener, note) = bind(config)?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("读取监听地址失败:{err}"))?
        .port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let app_state = Arc::new(McpState {
        web: web.clone(),
        port: daemon_port,
        failed_attempts: Mutex::new(HashMap::new()),
    });
    let join = spawn(listener, app_state, rx).map_err(|err| err.to_string())?;
    *slot = Some(Running {
        port,
        listen: config.listen,
        requested: config.requested_port(),
        note,
        stop: Some(tx),
        join,
    });
    Ok(())
}

/// Build a snapshot from the locked entry.
///
/// Never call `status()` while holding `inner`: `std::sync::Mutex` is not reentrant,
/// so that deadlocks on the spot (seen once as a daemon stuck during startup).
fn snapshot(slot: &Option<Running>, config: &Mcp) -> Status {
    match slot.as_ref() {
        Some(running) => Status {
            enabled: config.enabled,
            running: true,
            port: running.port,
            listen: running.listen,
            url: url_for(running.listen, running.port),
            error: None,
            note: running.note.clone(),
        },
        None => Status {
            enabled: config.enabled,
            running: false,
            // When idle, report the requested port: the configured one, else the default.
            port: config.requested_port().unwrap_or(DEFAULT_PORT),
            listen: config.listen,
            url: String::new(),
            error: None,
            note: None,
        },
    }
}

fn url_for(listen: Listen, port: u16) -> String {
    match listen {
        Listen::Lan => format!("http://127.0.0.1:{port}"),
        Listen::Wan => format!("http://{}:{port}", crate::net::local_ipv4()),
    }
}

/// Port tried first when `mcp.port` is empty (or 0).
///
/// An independent constant: not derived from the daemon or WebUI port (never "main
/// port + 1"). Fixed rather than OS-assigned so client URLs stay stable across
/// restarts, and it still gives way if taken.
pub const DEFAULT_PORT: u16 = 3994;

/// Bind the port; the note is non-empty only after an auto-pick fallback.
fn bind(config: &Mcp) -> Result<(std::net::TcpListener, Option<String>), String> {
    let ip = config.listen.bind_ip();

    // Explicit port: must bind, error out if taken (same rule as the daemon port).
    if let Some(port) = config.requested_port() {
        return bind_at(ip, port).map(|listener| (listener, None));
    }

    // Auto: try the default port first, then let the OS assign a free one.
    match bind_at(ip, DEFAULT_PORT) {
        Ok(listener) => Ok((listener, None)),
        Err(_) => {
            let listener = bind_at(ip, 0)?;
            let actual = listener
                .local_addr()
                .map(|addr| addr.port())
                .unwrap_or(0);
            Ok((
                listener,
                Some(format!(
                    "默认端口 {DEFAULT_PORT} 被占用,已改用 {actual}(想固定就设 mcp.port)"
                )),
            ))
        }
    }
}

fn bind_at(ip: std::net::Ipv4Addr, port: u16) -> Result<std::net::TcpListener, String> {
    let addr = SocketAddr::from((ip, port));
    let listener = std::net::TcpListener::bind(addr).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AddrInUse {
            format!("MCP 端口 {port} 已被占用")
        } else {
            format!("MCP 绑定 {addr} 失败:{err}")
        }
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("设置非阻塞失败:{err}"))?;
    Ok(listener)
}

/// Signal and wait briefly; a late thread finishes on its own, and we never hard-kill
/// one, which would rip out in-flight requests.
fn stop_running(running: Option<Running>) {
    let Some(mut running) = running else {
        return;
    };
    if let Some(tx) = running.stop.take() {
        let _ = tx.send(());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while !running.join.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Dedicated thread and tokio runtime, mirroring the WebUI listener.
fn spawn(
    listener: std::net::TcpListener,
    state: Arc<McpState>,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("cgw-mcp".to_string())
        .spawn(move || {
            let runtime = match crate::server::build_runtime() {
                Ok(runtime) => runtime,
                Err(err) => {
                    crate::log::log(crate::log::WARN, format!("MCP tokio runtime: {err}"));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(err) => {
                        crate::log::log(crate::log::WARN, format!("MCP listener: {err}"));
                        return;
                    }
                };
                let app = router(state);
                let serve = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = stop.await;
                });
                if let Err(err) = serve.await {
                    crate::log::log(crate::log::WARN, format!("MCP server: {err}"));
                }
            });
        })
}

// ==================================================================== HTTP

struct McpState {
    web: Arc<WebState>,
    /// The daemon's own port; tool calls target it.
    port: u16,
    /// Login rate limit: source IP -> last failed attempt time (anti brute-force).
    failed_attempts: Mutex<HashMap<IpAddr, Instant>>,
}

fn router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(mcp_post).get(method_not_allowed))
        // Accept the root path too, to survive a misconfigured endpoint.
        .route("/", post(mcp_post).get(root_get))
        .route("/healthz", get(healthz))
        .fallback(not_found)
        .with_state(state)
}

async fn healthz(State(state): State<Arc<McpState>>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "server": SERVER_NAME,
        "version": SERVER_VERSION,
        "mcp": true,
        "daemonPort": state.port,
        "tools": tools().len(),
        "resources": jsonschema::NAMES.len(),
    }))
}

async fn root_get() -> Response {
    text_response(
        StatusCode::OK,
        format!(
            "{SERVER_NAME} MCP {SERVER_VERSION}\n\
             MCP 端点是 POST /mcp(Streamable HTTP,无状态);健康检查 GET /healthz。\n"
        ),
    )
}

async fn method_not_allowed() -> Response {
    // Stateless mode has no server-initiated streams, so the spec makes GET a 405.
    text_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "MCP 端点是 POST /mcp(Streamable HTTP,无状态);GET 不提供 SSE 流。\n".to_string(),
    )
}

async fn not_found() -> Response {
    text_response(StatusCode::NOT_FOUND, "没有这个路径。\n".to_string())
}

fn text_response(status: StatusCode, body: String) -> Response {
    (status, [("content-type", "text/plain; charset=utf-8")], body).into_response()
}

#[derive(Debug)]
enum AuthResult {
    Ok,
    Unauthorized,
    RateLimited(u64),
}

async fn mcp_post(
    State(state): State<Arc<McpState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    match check_auth(&state, &headers, addr) {
        AuthResult::Ok => {}
        AuthResult::RateLimited(wait) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, wait.to_string())],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32001,
                        "message": format!("尝试过于频繁,请在 {wait} 秒后重试 (429 Too Many Requests)"),
                        "data": { "retryAfterSeconds": wait }
                    },
                })),
            )
                .into_response();
        }
        AuthResult::Unauthorized => {
            return (
                StatusCode::UNAUTHORIZED,
                [(
                    header::WWW_AUTHENTICATE,
                    "Basic realm=\"CompileGateway MCP\"",
                )],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32000, "message": "需要访问凭据(HTTP Basic)" },
                })),
            )
                .into_response();
        }
    }

    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("请求体不是合法 JSON:{err}") },
                })),
            )
                .into_response()
        }
    };

    if message.is_array() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32600, "message": "不支持 JSON-RPC 批请求" },
            })),
        )
            .into_response();
    }

    match handle_rpc(&state, &message).await {
        Some(response) => (StatusCode::OK, Json(response)).into_response(),
        // A notification has no response body; 202 is what the spec recommends.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Auth matches the daemon body: loopback sources pass, no configured credentials
/// means no enforcement (but `listen: wan` keeps warning in the log and status page),
/// and with credentials set, non-loopback sources need HTTP Basic.
///
/// Anti brute-force: failed authentication attempts from non-loopback IPs trigger rate
/// limiting using `auth.loginIntervalSeconds`. Subsequent attempts during the cool-off
/// period are immediately rejected with HTTP 429 without executing Argon2.
fn check_auth(state: &McpState, headers: &HeaderMap, addr: SocketAddr) -> AuthResult {
    if addr.ip().is_loopback() {
        return AuthResult::Ok;
    }
    let (configured, username, hash, interval) = {
        let config = crate::lock::read(&state.web.config);
        (
            config.auth.is_configured(),
            config.auth.username.clone(),
            config.auth.password_hash.clone(),
            config.auth.login_interval(),
        )
    };
    if !configured {
        return AuthResult::Ok;
    }

    if !interval.is_zero() {
        let attempts = crate::lock::lock(&state.failed_attempts);
        if let Some(last) = attempts.get(&addr.ip()) {
            if last.elapsed() < interval {
                let wait = interval.saturating_sub(last.elapsed()).as_secs() + 1;
                return AuthResult::RateLimited(wait);
            }
        }
    }

    let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
    else {
        return AuthResult::Unauthorized;
    };
    let Ok(decoded) = base64_decode(raw.trim()) else {
        record_failed_attempt(state, addr.ip(), interval);
        return AuthResult::Unauthorized;
    };
    let Ok(text) = String::from_utf8(decoded) else {
        record_failed_attempt(state, addr.ip(), interval);
        return AuthResult::Unauthorized;
    };
    let Some((user, password)) = text.split_once(':') else {
        record_failed_attempt(state, addr.ip(), interval);
        return AuthResult::Unauthorized;
    };
    if user == username && crate::auth::verify_password(password, &hash) {
        if !interval.is_zero() {
            crate::lock::lock(&state.failed_attempts).remove(&addr.ip());
        }
        AuthResult::Ok
    } else {
        record_failed_attempt(state, addr.ip(), interval);
        AuthResult::Unauthorized
    }
}

fn record_failed_attempt(state: &McpState, ip: IpAddr, interval: Duration) {
    if !interval.is_zero() {
        let mut attempts = crate::lock::lock(&state.failed_attempts);
        attempts.insert(ip, Instant::now());
    }
}

/// Minimal base64 decode for `Authorization: Basic`; 30 lines do not justify a crate.
fn base64_decode(raw: &str) -> Result<Vec<u8>, ()> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in raw.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = value(byte) else {
            return Err(());
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Ok(out)
}

// ==================================================================== JSON-RPC

/// Handle one message; `None` means a notification or client response, so send nothing.
async fn handle_rpc(state: &Arc<McpState>, message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);

    let (Some(id), Some(method)) = (id, method) else {
        // No id = notification; no method = response to our request. Neither is answered.
        return None;
    };

    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let outcome: Result<Value, Value> = match method {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools_json() })),
        "tools/call" => return Some(call_tool(state, id, &params).await),
        "resources/list" => Ok(json!({ "resources": resources_json() })),
        "resources/read" => read_resource(state, &params).await,
        "prompts/list" => Ok(json!({ "prompts": prompts_json() })),
        "prompts/get" => get_prompt(&params),
        other => Err(rpc_error(-32601, format!("不支持的方法:{other}"))),
    };

    Some(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

fn rpc_error(code: i64, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|value| PROTOCOL_VERSIONS.contains(value))
        .unwrap_or("2025-06-18");
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        // First layer of the double safety net: clients differ in whether and how much
        // they forward instructions, so keep only the hard rules and name both resources.
        "instructions":
            "CompileGateway:把项目从 daemon 这台机器同步到 SSH 目标机、在目标机上构建、再把产物拉回来。\n\
             \n\
             **动手之前先读 `cgw://guide`**(一页 playbook:三条典型流程、错误怎么办、\
             什么时候该把操作落成 JSON)。需要改用 `cgw` 命令行、或者想知道退出码含义时,\
             读 `cgw://cli`(80 条命令的用法 + 退出码契约)。任务结构的权威说明在 \
             `cgw://schema/task`。\n\
             \n\
             三条硬规矩:\n\
             1. 执行类工具(cgw_run_task / cgw_run_batch / cgw_retry_job)立刻返回 jobId,\
             用 cgw_job 跟状态、cgw_job_logs 读输出。碰到 409 projectBusy(同项目已有 job 在跑)时:**要么用 `enqueue: true` 排队**\
             (项目一空出来自动开跑),要么等那个 job 结束 —— **不要反复重试到成功为止**;\n\
             2. **build 不会自动 push**:首次跑一个项目请把 push 和 build 串成一个 batch;\n\
             3. 在构建 / 同步 / 打包 / 导出 / 多步文件操作**之前**,先做一次复用判断\
             (ephemeral / project-task / project-batch)并在回复里说明选了哪一档 ——\
             判定条件见 cgw://guide。",
    })
}

// ------------------------------------------------------------------ resources: two prose layers

/// `cgw://guide` -- one-page playbook.
///
/// Tool descriptions stay scattered, while an agent needs the end-to-end path; this
/// used to live only in the README, which agents cannot read.
const GUIDE_URI: &str = "cgw://guide";
/// `cgw://cli` -- this system also ships a self-describing CLI.
///
/// Shell-capable agents (Codex and friends) could already bootstrap from
/// `cgw help --json`, but nothing told them it existed.
const CLI_URI: &str = "cgw://cli";

const GUIDE: &str = r#"# CompileGateway 使用手册(给 AI agent)

> 这份是**流程**,不是字段表。字段表用 `cgw://schema/task`;想改用命令行看 `cgw://cli`。

## 这是什么

一台常开的机器上跑着一个 daemon,它同时听**两个端口**:一个给 WebUI(人看),
一个给 MCP(你)。daemon 负责三件事:

1. 把宿主机上的源码目录**推**到一台 SSH 目标机(通常是一台 macOS 构建机);
2. 在目标机上**跑构建**(命令来自构建类型 manifest);
3. 把产物**拉**回来 / 打包导出。

你不直接碰 SSH 和 rsync —— 所有动作都走工具,daemon 负责落盘、加锁、记日志。

## 三条典型流程

### A. 第一次把一个项目跑起来(最常用)

```
1. cgw_browse                      找一个宿主机上的源码目录
2. cgw_list_clients                看有没有登记过目标机(没有就 cgw_save_client)
3. cgw_detect_manifests            按目录探测该用哪个构建类型
4. cgw_create_project              登记项目(clientId + sourcePath)
5. cgw_save_task   ×2              建 push 任务、建 build 任务
6. cgw_save_batch                  把 push 和 build 串成一个 batch(顺序不能反)
7. cgw_run_batch                   跑它
8. cgw_job / cgw_job_logs          跟状态、读构建输出
```

**第 6 步是关键**:`build` **不会自动 push**。只跑 build 的话远端目录可能是空的,
失败信息看着像环境问题(`chmod: gradlew: No such file or directory`)。
一次建好 batch,以后用户不用你也能在界面上点一下重跑。

### B. 日常重跑

`cgw_list_projects` → `cgw_list_batches <projectId>` → `cgw_run_batch`。
改了构建变量时,先 `cgw_preview_task` 看一眼渲染出来的命令,再跑。

### C. 失败了怎么办

```
1. cgw_job <jobId>            看每一步的结果,找第一个失败的那步(看 retryFromStep)
2. cgw_job_logs <jobId>       读那一步的真实输出 —— 判断是哪一类问题(见下表)
3. 修掉原因(改变量 / 改 manifest / 先 push / 等目标机)
4. cgw_retry_job <jobId>      从失败那一步重试(不重跑已经成功的步骤)
```

**不要**在 `projectBusy` 上重试到成功为止:那说明同一个项目上还有 job 在跑,
先 `cgw_job` 看看它在干什么。也不要盲目 `fromStart`,那会把已经成功的步骤重跑一遍。

## 常见错误怎么读

| 现象 | 实际含义 | 怎么办 |
|---|---|---|
| 409 `projectBusy`(带 jobId) | 同一个项目已经有 job 在跑,daemon 有意不允许并发 | **两种正路**:要么用 cgw_job 跟那个 job 等它结束,要么用 `enqueue: true`(cgw_run_task / cgw_run_batch)排队 —— 项目一空出来会自动开跑(排队计划持久化落盘,daemon 重启后自动恢复)。**不要**反复重试到成功为止 |
| 409 `hostBusy`(带 clientId) | **同一台客户机上另一个项目**在跑,而 `hostLock` 打开了(全局或单机设置生效,那台机器一次只干一件事) | 换一台客户机,或者 `enqueue: true` 排队等它结束 —— 别把它当成项目锁去查那个项目 |
| 429 限流 / `-32001` | WAN 模式下 MCP 鉴权连续失败,触发客户端 IP 防爆破限流 | 检查 `Retry-After` 头部退避重试,确认 `CGW_USERNAME` / `CGW_PASSWORD` 正确 |
| 404 `jobNotFound` / `projectNotFound` | id 写错了,或者对象被删了 | 先 list 一遍拿准 id |
| 400 `variableRequired` | manifest 声明了必填变量,任务里没给 | 看 `cgw://schema/task` 的 variables,或 cgw_preview_task 的报错 |
| 400 `inputInvalid` | 任务的 `inputs` 契约没过:引用了没声明的输入、值不在 `enum` 里、或者展开后路径带 `..` | 在任务的 `inputs` 里声明它,值放进 `variables`;`cgw_preview_task` 能看到展开后的真实路径 |
| 400 `artifactMissing` | 任务声明了必需的 `outputs`,或 manifest 步骤声明了必需的 `outputs`,跑完却没找到(或类型不对) | 确认产物路径写对了,或者把该项 `required` 设成 false(可选输出缺了不算失败) |
| 400 `taskInvalid` / `manifestInvalid` | 结构不对(比如 copy 少了 to、路径带 `..`) | 照 cgw://schema/* 改 |
| 403 `outsideRoot` | 路径不在允许的宿主目录里 | 先 `cgw_set_config` 加 `allowedRoots`,**别到处试路径** |
| 401 / 403 别的 | 需要登录(daemon 设了凭据) | 用 CGW_USERNAME / CGW_PASSWORD 环境变量 |
| build 第一步就失败 | 远端还没同步过源码 | 先跑 push,或者用 push → build 的 batch |
| `bad interpreter: /bin/sh^M` | 脚本是 CRLF(Windows 检出) | push 默认已归一化带 shebang 的脚本;若用了 keepCrlf 就把它关掉 |

## 要不要把这次操作落成 JSON

在**构建、同步、打包、导出、或任何多步文件操作之前**,先做一次判断,并在回复里说明选了哪一档:

| 档位 | 什么时候 | 怎么做 |
|---|---|---|
| `ephemeral` | 一次性:探测环境、诊断、看日志、确认某个路径 | 直接调工具,不写任何东西 |
| `project-task` | 单个可复用动作(只推源码 / 只构建 / 只导出) | cgw_save_task |
| `project-batch` | 多步、有顺序、要复用 | cgw_save_batch |
| `workflow-preset` | **跨项目**复用:这套流程换个项目还要再来一遍 | cgw_save_workflow + cgw_apply_workflow |

跨项目那档(设计文档 §14.3):模板存在 daemon 的 `workflows/` 目录里(自带两份:
首次构建的 push→build→pull、构建→打包导出),`cgw_apply_workflow` 先给**预览**
(会多出哪些任务、batch id、撞了谁),确认后带 `apply: true` 写进项目 —— 之后它就是
普通的任务 + 一条批量工作流,用户能在界面上自己重跑,不需要你参与。

**必须落盘**:用户明说要复用 / 一键执行;步骤多于一步;涉及固定目标机、固定变量或固定产物路径;
预计跨会话、跨项目、或者要交给别人执行。
**用户明确说不用落盘**:照做,但要在回复里说清楚"这次不会出现在项目任务/batch 里"。

### 顺手把输入/输出写成契约(设计文档 §7.6.3)

存任务时,能写就写上这两块 —— 它们不是装饰,会真的被校验:

- `inputs`:声明这次要用的参数(`type` / `default` / `side`),路径字段里写 `{{inputs.名字}}`,
  值放 `variables`。好处是任务被复用、被重试时,"这次用的是哪一版输入"有据可查(job 里存快照)。
- `outputs`:声明这一步该产出什么(`{ type: file|directory, side, path, required }`)。
  **声明了就必须真的产出** —— 跑完会去核对存在性与类型,缺了必需项这一步就是失败
  (错误码 `artifactMissing`),而不是"退出码 0 就算过"。
  只想"有最好、没有也行"就把该项 `required` 设成 `false`。

这两块七种任务类型通用。写完可以用 `cgw_preview_task` 看展开后的真实路径与会被核对的输出。

## 几条别做的事

- 别绕过 daemon 自己 SSH / rsync:那样没有日志、没有项目锁、用户事后也看不到。
- 别跳过 `cgw_preview_task` 就改一堆变量跑构建 —— 它只渲染不执行,很便宜。
- 别把 `manifest/` 下的 json 当只读:那是**用户的文件**,改完记得 `cgw_reload_manifests` 刷一下。
- 别一次问用户五个问题:先用 cgw_browse / cgw_list_* / cgw_status 把能自己查的查掉。

## 想用命令行

如果这轮任务更适合在 shell 里做(比如你自己就能跑 `cgw`),读 `cgw://cli`:
里面有 60 条命令的用法、退出码契约、以及「什么时候该用 CLI 而不是 MCP」。
命令行**绝不交互**,所有命令都能脚本化。
"#;

/// When to use the CLI instead of MCP -- carried inside `cgw://cli` for the agent.
const WHEN_CLI: &str = "\
* 你**能在这台机器上跑 shell**(Codex 这类):除了一次性查询,直接用 `cgw` 往往更省事 —— \
输出是给人看的文本,而 `--json` 是给程序看的;两者和 MCP 工具**走的是同一套 HTTP API**,\
语义完全一致。\n\
* 你要**写脚本 / 做 CI**:用 CLI,它**绝不交互**,退出码是稳定契约(见 exitCodes)。\n\
* 你要**用户自己复现**你刚才做的事:把等价的 `cgw ...` 命令写进回复里,比「点这里那里」清楚。\n\
* 反过来,如果你**没有** shell、或者要用到结构化返回(比如直接拿 JSON 字段做判断),\
就用 MCP 工具 —— 它们的返回体就是同一批接口的 JSON。\n\
* `cgw help --json`(下面这份)是命令面的机器可读形态;`cgw help <命令>` 给单条命令的用法。";

fn resources_json() -> Vec<Value> {
    let mut out: Vec<Value> = vec![
        json!({
            "uri": GUIDE_URI,
            "name": "CompileGateway 使用手册",
            "description": "一页 playbook:**先读这个**。三条典型流程(首次跑起来 / 日常重跑 / 失败重试)、\
                            常见错误对照表、以及「这次操作要不要落成 JSON」的判定条件。",
            "mimeType": "text/markdown",
        }),
        json!({
            "uri": CLI_URI,
            "name": "cgw 命令行对照",
            "description": "`cgw help --json` 的同一份内容(60 条命令的用法 + 退出码契约 + 全局开关),\
                             外加一段「什么时候该用 CLI 而不是 MCP」。命令行形态:`cgw <命令> --url <daemon 地址> --json`。",
            "mimeType": "application/json",
        }),
    ];
    out.extend(jsonschema::NAMES.iter().map(|name| {
        json!({
            "uri": format!("{RESOURCE_PREFIX}{name}"),
            "name": format!("{name} JSON Schema"),
            "description": resource_description(name),
            "mimeType": "application/json",
        })
    }));
    out
}

fn resource_description(name: &str) -> &'static str {
    match name {
        "manifest" => {
            "构建类型 manifest 的 draft-07 JSON Schema。存 manifest(cgw_save_manifest)之前先照它校验。"
        }
        "task" => {
            "任务的 draft-07 JSON Schema。七种 kind 共用一个扁平结构,`oneOf` 分支写明每种 kind \
             必填哪些字段 —— 存任务(cgw_save_task)之前先看一眼。"
        }
        "batch" => "批量工作流的 draft-07 JSON Schema。步骤至少要有一条,步骤里的 taskId 不能为空。",
        "client" => {
            "客户机请求体的 draft-07 JSON Schema(`POST /api/clients`)。auth.password / \
             auth.passphrase 是只写字段:省略 = 不改,空串 = 清除。"
        }
        "project" => "项目登记请求体的 draft-07 JSON Schema(`POST /api/projects`)。",
        _ => "CompileGateway 的 JSON Schema。",
    }
}

async fn read_resource(state: &Arc<McpState>, params: &Value) -> Result<Value, Value> {
    let uri = params.get("uri").and_then(Value::as_str).unwrap_or_default();

    // The two prose resources live in the binary and never touch the daemon: they
    // describe usage, not state.
    if uri == GUIDE_URI {
        return Ok(resource_text(uri, "text/markdown", GUIDE.to_string()));
    }
    if uri == CLI_URI {
        return Ok(resource_text(uri, "application/json", cli_text()));
    }

    let name = uri.strip_prefix(RESOURCE_PREFIX).unwrap_or_default();
    if !jsonschema::NAMES.contains(&name) {
        let available: Vec<String> = resources_json()
            .iter()
            .filter_map(|item| item["uri"].as_str().map(str::to_string))
            .collect();
        return Err(rpc_error(
            -32602,
            format!("未知资源:{uri}(可用:{})", available.join(", ")),
        ));
    }
    // Schemas are fetched on read, so they always match the daemon's current copy.
    match daemon_json(state.port, "GET", &format!("/api/schema/{name}"), None).await {
        Ok(value) => Ok(resource_text(uri, "application/json", to_pretty(&value))),
        Err(err) => Err(rpc_error(-32603, err)),
    }
}

/// How one resource read looks (the MCP `resources/read` reply).
fn resource_text(uri: &str, mime: &str, text: String) -> Value {
    json!({
        "contents": [{ "uri": uri, "mimeType": mime, "text": text }],
    })
}

/// `cgw://cli` content: exactly `cgw help --json`, plus two explanatory fields.
///
/// "Exactly" is the point: it is rendered by [`crate::help`] from the same table, so
/// the command surface cannot be duplicated here.
fn cli_text() -> String {
    let mut value: Value =
        serde_json::from_str(&crate::help::json_for(crate::i18n::Lang::EnUS)).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "source".to_string(),
            Value::from("cgw help --json(和 CLI 同一张表,不是另抄一份)"),
        );
        object.insert("whenToUseCli".to_string(), Value::from(WHEN_CLI));
        object.insert(
            "selfDescribing".to_string(),
            Value::from(
                "单条命令的用法:`cgw help <命令> [子命令]`;对照 WebUI:`cgw help --webui`。\
                 所有命令都不交互,可以安全脚本化。",
            ),
        );
    }
    to_pretty(&value)
}

// ==================================================================== prompts

/// A prompt template: the user picks one and a workflow-shaped hint reaches the model.
///
/// Third teaching layer after instructions and resources; the first two need the model
/// to look, this one the user triggers. The three templates mirror the three flows in
/// `cgw://guide`.
struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// `(argument name, description, required)`
    args: &'static [(&'static str, &'static str, bool)],
    /// Builds the body; arguments come from the `prompts/get` arguments.
    body: fn(&Value) -> Result<String, String>,
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "first-build",
        title: "首次把一个项目跑起来",
        description: "从「挑目录」到「跑通第一次构建」:登记项目 → 建 push/build 任务 → 串成 batch → 跑它。\
                      首次构建必须包含 push(build 不会自动 push)。",
        args: &[
            ("sourcePath", "宿主机上的源码目录绝对路径(省略则先用 cgw_browse 找)", false),
            ("clientId", "目标机 id(省略则用 cgw_list_clients 确认)", false),
        ],
        body: |args| {
            let mut out = String::from(
                "请按 CompileGateway 的标准流程,帮我把这个项目**第一次**跑通。\
                 动手前先读资源 cgw://guide 的「A. 第一次把一个项目跑起来」。\n",
            );
            match arg(args, "sourcePath") {
                Some(path) => out.push_str(&format!("\n- 源码目录:{path}\n")),
                None => out.push_str("\n- 源码目录:我还没定,请你用 cgw_browse 找出来问我\n"),
            }
            match arg(args, "clientId") {
                Some(id) => out.push_str(&format!("- 目标机 id:{id}\n")),
                None => out.push_str("- 目标机:请你用 cgw_list_clients 列出来让我选\n"),
            }
            out.push_str(
                "\n要求:\n\
                 1. 用 cgw_detect_manifests 确认构建类型,别猜;\n\
                 2. 建一个 **push → build** 的 batch(build 不会自动 push,顺序不能反),\n\
                    然后把这个 batch 存下来 —— 以后我不需要你也能在界面上点一下重跑;\n\
                 3. 跑之前用 cgw_preview_task 给我看渲染出来的命令;\n\
                 4. 跑起来之后用 cgw_job / cgw_job_logs 跟到底,别只丢一个 jobId 给我;\n\
                 5. 最后告诉我:batch 的名字、产物的绝对路径、以及下次我自己该点哪里。",
            );
            Ok(out)
        },
    },
    Prompt {
        name: "diagnose-job",
        title: "帮我看看这个 job 为什么失败",
        description: "读 job 的每一步结果和日志,判断是哪一类问题,并给出下一步(重试 / 先同步 / 改变量)。",
        args: &[("jobId", "要诊断的 job id", true)],
        body: |args| {
            let job = arg(args, "jobId").ok_or("缺少参数 jobId")?;
            Ok(format!(
                "job `{job}` 失败了,请按 CompileGateway 的标准流程帮我定位。\
                 先读资源 cgw://guide 的「C. 失败了怎么办」和「常见错误怎么读」。\n\
                 \n要求:\n\
                 1. cgw_job {job} —— 找到**第一个**失败的步骤(看 retryFromStep);\n\
                 2. cgw_job_logs {job} —— 读那一步的真实输出,不要根据 job 的 message 猜;\n\
                 3. 明确告诉我是哪一类:源码没同步 / 目标机环境缺东西 / 构建命令或变量不对 / \
                    目标机连不上 / 我的任务定义有问题;\n\
                 4. 给出**一个**最可能的修法,并说明依据(引用日志里的哪一行);\n\
                 5. 如果可以直接修(比如变量写错了、少了 push),就修完再 cgw_retry_job {job};\n\
                    如果要改的是我的代码或目标机环境,先说清楚再问我。",
            ))
        },
    },
    Prompt {
        name: "pick-manifest",
        title: "这个工程该用哪个构建类型",
        description: "按目录探测命中的构建类型,解释每个命中的依据,并推荐一个(不自动建项目)。",
        args: &[("sourcePath", "宿主机上的工程目录绝对路径", true)],
        body: |args| {
            let path = arg(args, "sourcePath").ok_or("缺少参数 sourcePath")?;
            Ok(format!(
                "宿主机上这个目录 `{path}` 该用哪个构建类型?请:\n\
                 1. cgw_list_manifests 看现有类型(注意 valid=false 的会说明哪里不合法);\n\
                 2. cgw_detect_manifests `{path}` 拿到命中结果;\n\
                 3. 命中的每一个都告诉我它**凭什么**命中(哪些 include.paths 匹配上了);\n\
                 4. 命中多个 / 一个都没命中时,别硬选:说清楚差什么,并给出可选项让我定;\n\
                 5. 只给结论和依据,**不要**顺手建项目或建任务 —— 我确认了再说。",
            ))
        },
    },
];

fn prompts_json() -> Vec<Value> {
    PROMPTS
        .iter()
        .map(|prompt| {
            json!({
                "name": prompt.name,
                "title": prompt.title,
                "description": prompt.description,
                "arguments": prompt.args.iter().map(|(name, description, required)| json!({
                    "name": name,
                    "description": description,
                    "required": required,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn get_prompt(params: &Value) -> Result<Value, Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    let Some(prompt) = PROMPTS.iter().find(|prompt| prompt.name == name) else {
        let available: Vec<&str> = PROMPTS.iter().map(|prompt| prompt.name).collect();
        return Err(rpc_error(
            -32602,
            format!("未知 prompt:{name}(可用:{})", available.join(", ")),
        ));
    };
    let text = (prompt.body)(args).map_err(|message| rpc_error(-32602, message))?;
    Ok(json!({
        "description": prompt.description,
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": text },
        }],
    }))
}

/// Read a string argument (empty counts as absent).
fn arg(args: &Value, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Flatten `cgw_set_config`'s categorized shape into the flat patch the daemon takes.
///
/// The config page and `cgw set` group arguments by category while HTTP wants a flat
/// patch; doing the translation in one place keeps MCP and CLI wording identical.
fn categorized(args: &Value) -> Value {
    let mut out = args.as_object().cloned().unwrap_or_default();

    // port / listen: the two ports are independent and land on `port` / `mcp.*`.
    if let Some(port) = args.get("port").and_then(Value::as_object) {
        // The category object must be removed: with only the mcp half given, sending
        // `{webui,mcp}` as a flat field makes the daemon reject it (it wants a number).
        out.remove("port");
        if let Some(webui) = port.get("webui") {
            out.insert("port".to_string(), webui.clone());
        }
        if let Some(mcp) = port.get("mcp") {
            merge_nested(&mut out, "mcp", "port", mcp.clone());
        }
    }
    if let Some(listen) = args.get("listen").and_then(Value::as_object) {
        out.remove("listen");
        if let Some(webui) = listen.get("webui") {
            out.insert("listen".to_string(), webui.clone());
        }
        if let Some(mcp) = listen.get("mcp") {
            merge_nested(&mut out, "mcp", "listen", mcp.clone());
        }
    }

    // user:{name,password,clear}
    if let Some(user) = args.get("user").and_then(Value::as_object) {
        if user.get("clear").and_then(Value::as_bool) == Some(true) {
            out.insert("username".to_string(), Value::from(""));
            out.insert("password".to_string(), Value::from(""));
        } else {
            if let Some(name) = user.get("name") {
                out.insert("username".to_string(), name.clone());
            }
            if let Some(password) = user.get("password") {
                out.insert("password".to_string(), password.clone());
            }
        }
        out.remove("user");
    }

    // roots:{add,remove,set}
    if let Some(roots) = args.get("roots").and_then(Value::as_object) {
        if let Some(add) = roots.get("add") {
            out.insert("rootsAdd".to_string(), add.clone());
        }
        if let Some(remove) = roots.get("remove") {
            out.insert("rootsRemove".to_string(), remove.clone());
        }
        if let Some(set) = roots.get("set") {
            out.insert("allowedRoots".to_string(), set.clone());
        }
        out.remove("roots");
    }

    Value::Object(out)
}

/// `mcp` is nested: the categorized form must merge into an existing field, not replace it.
fn merge_nested(out: &mut Map<String, Value>, outer: &str, key: &str, value: Value) {
    let entry = out
        .entry(outer.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(object) = entry.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

// ==================================================================== Tool calls

async fn call_tool(state: &Arc<McpState>, id: Value, params: &Value) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let Some(tool) = tools().iter().find(|tool| tool.name == name) else {
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("未知工具:{name}") }],
                "isError": true,
            },
        });
    };

    // cgw_save_task takes a nested task or flat fields; accept both.
    let normalized = if tool.name == "cgw_save_task" && args.get("task").is_none() {
        let mut merged = args.clone();
        if let Some(object) = merged.as_object_mut() {
            object.insert("task".to_string(), task_from_args(&args));
        }
        merged
    } else {
        args
    };

    // Keep the two failures apart: a `route` error means bad arguments, which we can
    // explain ourselves, while the other is the daemon's HTTP error body. Mixing them
    // folds "missing taskId" into `HTTP 0 unknown`, which looks like a daemon bug.
    let (text, structured, is_error) = match (tool.route)(&normalized) {
        Err(message) => (
            format!("参数不对:{message}"),
            json!({ "code": "invalidArguments", "message": message }),
            true,
        ),
        Ok(route) => {
            match daemon_json(state.port, route.method, &route.path, route.body).await {
                Ok(value) => (
                    to_pretty(&summarize(tool.name, &value)),
                    Value::Null,
                    false,
                ),
                Err(raw) => {
                    let failure = Failure::parse(&raw);
                    (failure.describe(), failure.to_json(), true)
                }
            }
        }
    };

    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
    });
    if is_error {
        result["isError"] = Value::Bool(true);
        result["structuredContent"] = json!({ "error": structured });
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Structured information about a failed tool call.
struct Failure {
    status: u16,
    code: String,
    message: String,
    extra: Map<String, Value>,
}

impl Failure {
    /// `raw` is `HTTP <status> <body>` as assembled by `daemon_json`.
    fn parse(raw: &str) -> Failure {
        let trimmed = raw.strip_prefix("HTTP ").unwrap_or(raw);
        let (head, body) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let status = head.parse::<u16>().unwrap_or(0);
        let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let error = parsed.get("error").cloned().unwrap_or(Value::Null);
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(body)
            .to_string();
        let extra = error.as_object().cloned().unwrap_or_default();
        Failure {
            status,
            code,
            message,
            extra,
        }
    }

    fn to_json(&self) -> Value {
        let mut object = self.extra.clone();
        object.insert("code".to_string(), Value::from(self.code.clone()));
        object.insert("message".to_string(), Value::from(self.message.clone()));
        if self.status != 0 {
            object.insert("httpStatus".to_string(), Value::from(self.status));
        }
        Value::Object(object)
    }

    /// Text for humans and models. `projectBusy` deliberately says more: a model's
    /// first reflex on a 409 is to retry, which here can only fail again.
    fn describe(&self) -> String {
        let mut lines = vec![format!("HTTP {} {}", self.status, self.code)];
        if self.code == "projectBusy" {
            lines.push(format!(
                "这个项目正在跑 job {}({}),已运行 {} 秒。同一项目不允许并发 —— \
                 先用 cgw_job 或 cgw_job_logs 跟踪它,稍后再试;不要重试到成功为止。",
                self.extra
                    .get("jobId")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                self.extra
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                self.extra
                    .get("runningSeconds")
                    .and_then(Value::as_u64)
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            ));
        }
        if let Some(seconds) = self.extra.get("retryAfterSeconds").and_then(Value::as_u64) {
            lines.push(format!("建议 {seconds} 秒后再试。"));
        }
        lines.push(self.message.clone());
        lines.join("\n")
    }
}

// ==================================================================== Talking to the daemon

/// Call the daemon's own HTTP API.
///
/// Loopback TCP rather than a direct function call keeps this path identical to
/// WebUI / CLI: auth, `X-Client` marking, rate limits and error bodies all match.
async fn daemon_raw(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, String), String> {
    let addr = format!("127.0.0.1:{port}");
    let method = method.to_string();
    let path = path.to_string();
    let payload = body.map(|value| value.to_string());
    tokio::task::spawn_blocking(move || {
        httpclient::request_as(&addr, &method, &path, payload.as_deref(), SOURCE)
            .map(|response| (response.status, response.body))
    })
    .await
    .map_err(|err| format!("内部调用失败:{err}"))?
}

async fn daemon_json(
    port: u16,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let (status, text) = daemon_raw(port, method, path, body).await?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} {text}"));
    }
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).map_err(|err| format!("daemon 返回的不是 JSON:{err}"))
}

// ==================================================================== Tool table

/// One MCP tool.
///
/// `schema` / `route` are function pointers rather than closures so the whole table can
/// be a `const`: a single tool table becomes a compile-time fact.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema of the tool arguments.
    pub schema: fn() -> Value,
    /// Arguments -> one daemon HTTP request.
    pub route: fn(&Value) -> Result<Route, String>,
}

pub struct Route {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}

pub fn tools() -> &'static [Tool] {
    TOOLS
}

fn tools_json() -> Vec<Value> {
    tools()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.schema)(),
            })
        })
        .collect()
}

const TOOLS: &[Tool] = &[
    // ---------------------------------------------------------- status and config
    Tool {
        name: "cgw_status",
        description: "看 daemon 的状态:版本、访问地址、监听模式、语言、凭据是否已设、\
                      配置文件位置、MCP 适配层跑在哪个端口、以及 wan 无凭据时的安全警告。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/status")),
    },
    Tool {
        name: "cgw_get_config",
        description: "读当前配置(端口、语言、监听模式、登录限流间隔、日志保留天数、允许操作的宿主目录、MCP 开关与端口)。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/config")),
    },
    Tool {
        name: "cgw_set_config",
        description: "改配置(只改给出的字段)。访问凭据只允许在本机(loopback)修改;\
                      daemon 的端口和监听模式要重启才生效,\
                      **mcp 那一组和 allowedRoots 立即生效**(换端口会当场绑,失败会明确报错)。\
                      `allowedRoots` 是 browse / 登记项目 / 探测构建类型共用的宿主目录白名单 ——\
                      源码放在别的盘上时先加进去,否则那三处会一起拒绝。\
                      **分类形状和配置页 / `cgw set` 一一对应**:`port:{webui,mcp}`、`listen:{webui,mcp}`、\
                      `user:{name,password,clear}`、`language`、`loginIntervalSeconds`、`logRetainDays`、\
                      `hostLock`、`roots:{add,remove,set}`、`mcp:{enabled,listen,port}`。\
                      扁平写法(老的 `port` / `username` / `allowedRoots` …)照旧收。",
        schema: || {
            obj(
                "要改的字段,全部可选。**分类形状**见 port / listen / user / roots;老的扁平字段也收",
                json!({
                    "port": obj(
                        "端口:WebUI 与 MCP 是两个**独立**的端口",
                        json!({
                            "webui": num("WebUI 端口,0 = 自动挑(要重启才生效)"),
                            "mcp": num("MCP 端口,0 = 自动挑(**立即生效**)"),
                        }),
                        &[],
                    ),
                    "listen": obj(
                        "监听范围",
                        json!({
                            "webui": enum_("WebUI:lan = 只听 127.0.0.1;wan = 允许其他设备访问", &["lan", "wan"]),
                            "mcp": enum_("MCP:同上,两个端口互相独立", &["lan", "wan"]),
                        }),
                        &[],
                    ),
                    "user": obj(
                        "访问凭据(只有本机 / loopback 能改)",
                        json!({
                            "name": str_("用户名"),
                            "password": str_("密码"),
                            "clear": bool_("true = 清掉用户名和密码,改成免登录"),
                        }),
                        &[],
                    ),
                    "language": enum_("auto | zh-Hans | en-US", &["auto", "zh-Hans", "en-US"]),
                    "loginIntervalSeconds": num("登录限流间隔秒数,0 = 不限流"),
                    "logRetainDays": num("日志保留天数"),
                    "roots": obj(
                        "宿主目录白名单(browse / 登记项目 / 探测共用)",
                        json!({
                            "add": { "type": "array", "items": { "type": "string" }, "description": "加进去(不动其它项)" },
                            "remove": { "type": "array", "items": { "type": "string" }, "description": "去掉" },
                            "set": { "type": "array", "items": { "type": "string" }, "description": "整份覆盖" },
                        }),
                        &[],
                    ),
                    "mcp": obj(
                        "MCP 适配层(立即生效,不用重启)",
                        json!({
                            "enabled": bool_("是否随 daemon 一起提供 MCP"),
                            "listen": enum_("lan = 只听 127.0.0.1;wan = 允许其他设备访问", &["lan", "wan"]),
                            "port": num("MCP 端口,0 = 自动挑"),
                        }),
                        &[],
                    ),
                    "hostLock": bool_(
                        "true = **主机级互斥**:同一台客户机一次只跑一个 job(默认 false)。\
                         不同项目本来可以并行,但同一台 Mac 上的 Xcode / JDK / 设备是共享资源。\
                         打开后别的项目在这台机器上跑着时会返回 hostBusy;用 enqueue 排队则自动等。\
                         立即生效",
                    ),
                    // ---- legacy flat fields (still accepted) --------------------
                    "username": str_("访问用户名(扁平写法,同 user.name)"),
                    "password": str_("访问密码(扁平写法,同 user.password)"),
                    "allowedRoots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "整份覆盖白名单(扁平写法,同 roots.set)",
                    },
                }),
                &[],
            )
        },
        route: |args| Ok(route_body("POST", "/api/config", categorized(args))),
    },

    // ---------------------------------------------------------- clients
    Tool {
        name: "cgw_list_clients",
        description: "列出所有客户机(SSH 目标机):名称、地址、端口、验证方式、标签、基准目录。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/clients")),
    },
    Tool {
        name: "cgw_save_client",
        description: "新建或修改一台客户机。参数和 `POST /api/clients` 的请求体同一份 schema\
                      (见资源 cgw://schema/client)。password / passphrase 省略 = 不改已保存的那个;\
                      私钥口令(ssh-keygen 默认就会加密私钥)走 auth.passphrase;\
                      可传 hostLock (布尔值) 设置单机独占互斥排队锁。",
        schema: || daemon_schema("client"),
        route: |args| Ok(route_body("POST", "/api/clients", args.clone())),
    },
    Tool {
        name: "cgw_delete_client",
        description: "删除一台客户机。",
        schema: || obj("", json!({ "id": str_("客户机 id") }), &["id"]),
        route: |args| Ok(route("DELETE", &format!("/api/clients/{}", encode(&need(args, "id")?)))),
    },
    Tool {
        name: "cgw_test_client",
        description: "真连一次 SSH 测这台客户机。返回 ok / message / output / hostKey。",
        schema: || obj("", json!({ "id": str_("客户机 id") }), &["id"]),
        route: |args| {
            Ok(route(
                "POST",
                &format!("/api/clients/{}/test", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_detect_env",
        description: "检测目标机环境(设计文档 §8):一次 SSH 跑完所有探针,按 platform 分组返回 \
                      Xcode / JDK / Gradle / Flutter / Rust / 通用工具的版本与磁盘空间。慢(最多 90 秒)。",
        schema: || obj("", json!({ "id": str_("客户机 id") }), &["id"]),
        route: |args| {
            Ok(route(
                "POST",
                &format!("/api/clients/{}/env", encode(&need(args, "id")?)),
            ))
        },
    },

    // ---------------------------------------------------------- projects
    Tool {
        name: "cgw_list_projects",
        description: "列出项目登记,以及每个项目当前有没有正在跑的任务(running)。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/projects")),
    },
    Tool {
        name: "cgw_browse",
        description: "列 daemon 那台机器上的子文件夹(设计文档 §6.1),用来挑源码目录。\
                      只允许在用户主目录下浏览。",
        schema: || obj("", json!({ "path": str_("绝对路径;省略 = 用户主目录") }), &[]),
        route: |args| {
            let path = optional(args, "path")
                .map(|value| format!("/api/browse?path={}", encode(&value)))
                .unwrap_or_else(|| "/api/browse".to_string());
            Ok(route("GET", &path))
        },
    },
    Tool {
        name: "cgw_create_project",
        description: "把一个宿主机源码目录登记成项目。参数和 `POST /api/projects` 的请求体同一份 \
                      schema(见资源 cgw://schema/project)。sourcePath 必须是 daemon 那台机器上的\
                      绝对路径(可以用 cgw_browse 找)。",
        schema: || daemon_schema("project"),
        route: |args| Ok(route_body("POST", "/api/projects", args.clone())),
    },
    Tool {
        name: "cgw_delete_project",
        description: "移除一条项目登记(不删任何文件)。",
        schema: || obj("", json!({ "id": str_("项目 id") }), &["id"]),
        route: |args| Ok(route("DELETE", &format!("/api/projects/{}", encode(&need(args, "id")?)))),
    },

    // ---------------------------------------------------------- tasks
    Tool {
        name: "cgw_list_tasks",
        description: "列出项目的触发任务(设计文档 §7.1):七种类型 build / push / pull / copy / \
                      move / delete / export,每种只带自己相关的字段。",
        schema: || obj("", json!({ "projectId": str_("项目 id") }), &["projectId"]),
        route: |_| Ok(route("GET", "/api/projects")),
    },
    Tool {
        name: "cgw_save_task",
        description: "新建或修改一个项目任务(设计文档 §7.1)。id 留空 = 新建。路径只允许相对路径,\
                      不能写 .. 或绝对路径。task 的结构就是 daemon 生成的那份 schema(见资源 \
                      cgw://schema/task,含每种 kind 的必填字段);也接受把 task 的字段平铺在参数里。\
                      **落盘决策**:多步、要复用、要交给用户一键跑的操作请存成任务/batch(而不是每次现调);\
                      一次性探测/诊断就别存。",
        schema: || {
            obj(
                "任务定义",
                json!({
                    "projectId": str_("项目 id"),
                    "task": task_schema(),
                }),
                &["projectId", "task"],
            )
        },
        route: |args| {
            let project = need(args, "projectId")?;
            let task = args.get("task").cloned().unwrap_or_else(|| json!({}));
            Ok(route_body(
                "POST",
                &format!("/api/projects/{}/tasks", encode(&project)),
                task,
            ))
        },
    },
    Tool {
        name: "cgw_delete_task",
        description: "删除一个任务(引用它的 batch 步骤会一并摘掉)。",
        schema: || obj("", json!({ "projectId": str_("项目 id"), "taskId": str_("任务 id") }), &["projectId", "taskId"]),
        route: |args| {
            Ok(route(
                "DELETE",
                &format!(
                    "/api/projects/{}/tasks/{}",
                    encode(&need(args, "projectId")?),
                    encode(&need(args, "taskId")?)
                ),
            ))
        },
    },
    Tool {
        name: "cgw_preview_task",
        description: "只渲染不执行:看这个 build 任务会在远端跑什么命令、产物落在哪。改变量之前先用它确认。",
        schema: || obj("", json!({ "projectId": str_("项目 id"), "taskId": str_("任务 id") }), &["projectId", "taskId"]),
        route: |args| {
            Ok(route(
                "POST",
                &format!(
                    "/api/projects/{}/tasks/{}/preview",
                    encode(&need(args, "projectId")?),
                    encode(&need(args, "taskId")?)
                ),
            ))
        },
    },
    Tool {
        name: "cgw_run_task",
        description: "执行一个任务。**立刻返回 jobId**,不代表已经完成 —— 之后用 cgw_job 跟状态,\
                      用 cgw_job_logs 读日志。同一个项目同时只允许一个任务:被占用时默认返回 409 \
                      projectBusy(带 jobId),或者用 `enqueue: true` **排队**(项目一空出来就自动开跑,\
                      排队中的 job 可以用 cgw_cancel_job 取消)。**注意 build 不会自动 push**:\
                      项目还没同步过就先推源码,或者把 push 和 build 串成一个 batch。",
        schema: || obj("", json!({
            "projectId": str_("项目 id"),
            "taskId": str_("任务 id"),
            "enqueue": bool_("true = 项目忙时排队执行,而不是返回 409 projectBusy"),
        }), &["projectId", "taskId"]),
        route: |args| {
            let suffix = if bool_arg(args, "enqueue") { "enqueue" } else { "run" };
            Ok(route(
                "POST",
                &format!(
                    "/api/projects/{}/tasks/{}/{}",
                    encode(&need(args, "projectId")?),
                    encode(&need(args, "taskId")?),
                    suffix
                ),
            ))
        },
    },

    // ---------------------------------------------------------- batch workflows
    Tool {
        name: "cgw_list_batches",
        description: "列出项目的批量工作流(设计文档 §7.3)。",
        schema: || obj("", json!({ "projectId": str_("项目 id") }), &["projectId"]),
        route: |_| Ok(route("GET", "/api/projects")),
    },
    Tool {
        name: "cgw_save_batch",
        description: "新建或修改批量工作流。steps 是按顺序执行的任务 id 列表,可以跨类型混排\
                      (比如 build → copy → export)。步骤可以带 `id`,后面的步骤就能用\
                      `variables` 里的 `{{steps.<id>.outputs.<输出名>}}` 引用它**核对过**的输出\
                      (设计文档 §7.6.5);从中间重试时引用也能解析(输出记录跟着 job 落盘)。",
        schema: || {
            // `steps` comes straight from the daemon batch schema: hand-copying the
            // field list is how `id` went missing before (caught by a harness case).
            let steps = daemon_schema("batch")
                .get("properties")
                .and_then(|properties| properties.get("steps"))
                .cloned()
                .unwrap_or_else(|| json!({ "type": "array", "items": { "type": "object" } }));
            obj(
                "",
                json!({
                    "projectId": str_("项目 id"),
                    "id": str_("留空 = 新建"),
                    "name": str_("名称"),
                    "steps": steps,
                }),
                &["projectId", "name", "steps"],
            )
        },
        route: |args| {
            let project = need(args, "projectId")?;
            Ok(route_body(
                "POST",
                &format!("/api/projects/{}/batches", encode(&project)),
                json!({
                    "id": args.get("id").cloned().unwrap_or_else(|| json!("")),
                    "name": args.get("name").cloned().unwrap_or(Value::Null),
                    "steps": args.get("steps").cloned().unwrap_or_else(|| json!([])),
                }),
            ))
        },
    },
    Tool {
        name: "cgw_delete_batch",
        description: "删除批量工作流。",
        schema: || obj("", json!({ "projectId": str_("项目 id"), "batchId": str_("batch id 或名字") }), &["projectId", "batchId"]),
        route: |args| {
            Ok(route(
                "DELETE",
                &format!(
                    "/api/projects/{}/batches/{}",
                    encode(&need(args, "projectId")?),
                    encode(&need(args, "batchId")?)
                ),
            ))
        },
    },
    Tool {
        name: "cgw_run_batch",
        description: "执行批量工作流。整个 batch 持同一把项目级锁;任意一步失败就停下,\
                      之后用 cgw_retry_job 从失败步骤重试。项目忙时默认 409 projectBusy,\
                      或者用 `enqueue: true` **排队**(项目一空出来就自动开跑)。",
        schema: || {
            obj(
                "",
                json!({
                    "projectId": str_("项目 id"),
                    "batchId": str_("batch id 或名字"),
                    "fromStep": num("从第几步开始(0 起);省略 = 从头"),
                    "enqueue": bool_("true = 项目忙时排队执行,而不是返回 409 projectBusy"),
                }),
                &["projectId", "batchId"],
            )
        },
        route: |args| {
            let project = need(args, "projectId")?;
            let batch = need(args, "batchId")?;
            let body = match args.get("fromStep") {
                Some(value) if !value.is_null() => json!({ "fromStep": value }),
                _ => json!({}),
            };
            let suffix = if bool_arg(args, "enqueue") { "enqueue" } else { "run" };
            Ok(route_body(
                "POST",
                &format!(
                    "/api/projects/{}/batches/{}/{}",
                    encode(&project),
                    encode(&batch),
                    suffix
                ),
                body,
            ))
        },
    },

    // ---------------------------------------------------------- job
    Tool {
        name: "cgw_list_jobs",
        description: "最近 50 条 job,新的在前。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/jobs")),
    },
    Tool {
        name: "cgw_job",
        description: "查一个 job 的状态、产物和**每一步**的结果(batch 的 steps 数组里带 taskId、\
                      起止时间、输入输出路径、结果)。retryFromStep 是第一个没成功的步骤序号。",
        schema: || obj("", json!({ "jobId": str_("job id") }), &["jobId"]),
        route: |args| Ok(route("GET", &format!("/api/jobs/{}", encode(&need(args, "jobId")?)))),
    },
    Tool {
        name: "cgw_job_logs",
        description: "读一个 job 自己的日志(构建输出就在里面)。",
        schema: || {
            obj(
                "",
                json!({
                    "jobId": str_("job id"),
                    "since": num("只要 seq 大于它的(增量轮询用)"),
                }),
                &["jobId"],
            )
        },
        route: |args| {
            let job = need(args, "jobId")?;
            let query = match args.get("since").and_then(Value::as_u64) {
                Some(since) => format!("?since={since}"),
                None => String::new(),
            };
            Ok(route("GET", &format!("/api/jobs/{}/logs{query}", encode(&job))))
        },
    },
    Tool {
        name: "cgw_cancel_job",
        description: "协作式取消一个还没结束的 job。",
        schema: || obj("", json!({ "jobId": str_("job id") }), &["jobId"]),
        route: |args| {
            Ok(route(
                "POST",
                &format!("/api/jobs/{}/cancel", encode(&need(args, "jobId")?)),
            ))
        },
    },
    Tool {
        name: "cgw_retry_job",
        description: "从失败步骤重试(设计文档 §7.3):产生一个新 job,只跑剩余步骤,不重跑已经成功的。\
                      fromStart=true 才从头跑。",
        schema: || {
            obj(
                "",
                json!({
                    "jobId": str_("job id"),
                    "fromStep": num("显式指定从第几步开始(0 起)"),
                    "fromStart": bool_("从头执行"),
                }),
                &["jobId"],
            )
        },
        route: |args| {
            let job = need(args, "jobId")?;
            let mut body = Map::new();
            body.insert(
                "fromStart".to_string(),
                Value::from(args.get("fromStart").and_then(Value::as_bool).unwrap_or(false)),
            );
            if let Some(from) = args.get("fromStep") {
                if !from.is_null() {
                    body.insert("fromStep".to_string(), from.clone());
                }
            }
            Ok(route_body(
                "POST",
                &format!("/api/jobs/{}/retry", encode(&job)),
                Value::Object(body),
            ))
        },
    },
    Tool {
        name: "cgw_clear_jobs",
        description: "清空已完成的 job 历史记录(成功 / 失败 / 已取消)。正在运行或排队中的任务不会被影响。可选传入 projectId 只清空指定项目的记录。",
        schema: || {
            obj(
                "",
                json!({
                    "projectId": str_("可选:只清空该项目的已完成 job 历史"),
                }),
                &[],
            )
        },
        route: |args| {
            let mut query = String::new();
            if let Some(p) = args.get("projectId").and_then(Value::as_str) {
                if !p.trim().is_empty() {
                    query = format!("?projectId={}", encode(p.trim()));
                }
            }
            Ok(route("POST", &format!("/api/jobs/clear{query}")))
        },
    },

    // ---------------------------------------------------------- file tree
    Tool {
        name: "cgw_tree",
        description: "列项目文件树的一层(设计文档 §7.4):名称、相对路径、类型、大小、修改时间。\
                      side=remote 看客户机上的项目目录,side=host 看 daemon 的宿主托管目录。\
                      条目里的 path 就是文件操作任务要填的 path。",
        schema: || {
            obj(
                "",
                json!({
                    "projectId": str_("项目 id"),
                    "side": enum_("哪一侧", &["remote", "host"]),
                    "path": str_("相对项目根的路径;省略 = 根目录"),
                }),
                &["projectId"],
            )
        },
        route: |args| {
            let project = need(args, "projectId")?;
            let side = optional(args, "side").unwrap_or_else(|| "remote".to_string());
            let path = optional(args, "path").unwrap_or_default();
            Ok(route(
                "GET",
                &format!(
                    "/api/projects/{}/tree?side={}&path={}",
                    encode(&project),
                    encode(&side),
                    encode(&path)
                ),
            ))
        },
    },

    // ---------------------------------------------------------- workflow templates (§14.3)
    Tool {
        name: "cgw_list_workflows",
        description: "列出跨项目工作流模板(设计文档 §14.3)。它们是**模板**:\
                      apply 到一个项目之后就变成普通任务 + 一条批量工作流,\
                      之后普通用户能脱离你直接重跑。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/workflows")),
    },
    Tool {
        name: "cgw_read_workflow",
        description: "读一份工作流模板的原文 + 解析结果(改之前先看)。",
        schema: || obj("", json!({ "id": str_("模板 id(文件名,不含 .json)") }), &["id"]),
        route: |args| {
            Ok(route(
                "GET",
                &format!("/api/workflows/{}/raw", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_save_workflow",
        description: "保存一份工作流模板(新建或覆盖)。**校验不通过会被拒**:\
                      步骤引用的任务必须存在、步骤 id 唯一、\
                      `{{steps.<步骤 id>.outputs.<名字>}}` 只能指向前面已声明的输出。",
        schema: || {
            obj(
                "",
                json!({
                    "id": str_("模板 id(文件名,不含 .json)"),
                    "raw": str_("模板的 JSON 原文;结构见 cgw://schema/workflow"),
                }),
                &["id", "raw"],
            )
        },
        route: |args| {
            let id = need(args, "id")?;
            Ok(route_body(
                "PUT",
                &format!("/api/workflows/{}", encode(&id)),
                json!({ "raw": args.get("raw").cloned().unwrap_or(Value::Null) }),
            ))
        },
    },
    Tool {
        name: "cgw_delete_workflow",
        description: "删除一份工作流模板。已经 apply 到项目的**不受影响**\
                      (项目里那份是项目自己的了)。",
        schema: || obj("", json!({ "id": str_("模板 id") }), &["id"]),
        route: |args| {
            Ok(route(
                "DELETE",
                &format!("/api/workflows/{}", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_apply_workflow",
        description: "把工作流模板实例化到一个项目。**默认只预览**(回任务清单、\
                      batch id、冲突列表)——看清楚了再带 `apply: true` 来一次。\
                      同名任务 / batch 算冲突:要覆盖得显式给 `overwrite: true`,\
                      否则返回 409 workflowConflict。`run: true` 则 apply 完立刻跑一遍。",
        schema: || {
            obj(
                "",
                json!({
                    "projectId": str_("项目 id"),
                    "workflowId": str_("模板 id"),
                    "variables": {
                        "type": "object",
                        "description": "模板参数的值(只认模板声明过的名字)",
                        "additionalProperties": { "type": "string" },
                    },
                    "apply": bool_("true = 真的写进项目;省略/false = 只回冲突预览"),
                    "overwrite": bool_("true = 冲突时覆盖同名任务与 batch"),
                    "run": bool_("true = apply 完立刻执行那条批量工作流"),
                    "name": str_("batch 显示名;留空用模板名"),
                }),
                &["projectId", "workflowId"],
            )
        },
        route: |args| {
            let body = json!({
                "variables": args.get("variables").cloned().unwrap_or_else(|| json!({})),
                "apply": bool_arg(args, "apply"),
                "overwrite": bool_arg(args, "overwrite"),
                "run": bool_arg(args, "run"),
                "name": args.get("name").cloned().unwrap_or_else(|| json!("")),
            });
            Ok(route_body(
                "POST",
                &format!(
                    "/api/projects/{}/workflows/{}/apply",
                    encode(&need(args, "projectId")?),
                    encode(&need(args, "workflowId")?)
                ),
                body,
            ))
        },
    },

    // ---------------------------------------------------------- manifest
    Tool {
        name: "cgw_list_manifests",
        description: "列出所有构建类型 manifest,含每个类型的校验结果、变量声明、默认产物路径。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/manifests")),
    },
    Tool {
        name: "cgw_manifest_raw",
        description: "读一份 manifest 的原文(改之前先看)。",
        schema: || obj("", json!({ "id": str_("manifest id") }), &["id"]),
        route: |args| {
            Ok(route(
                "GET",
                &format!("/api/manifests/{}/raw", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_save_manifest",
        description: "保存一份 manifest(新建或覆盖)。**校验不通过会被拒**:结构非法、\
                      或 `{{占位符}}` 没在 variables 里声明,都返回 400。",
        schema: || {
            obj(
                "",
                json!({
                    "id": str_("manifest id(文件名,不含 .json)"),
                    "raw": str_("manifest 的 JSON 原文"),
                }),
                &["id", "raw"],
            )
        },
        route: |args| {
            let id = need(args, "id")?;
            Ok(route_body(
                "PUT",
                &format!("/api/manifests/{}", encode(&id)),
                json!({ "raw": args.get("raw").cloned().unwrap_or(Value::Null) }),
            ))
        },
    },
    Tool {
        name: "cgw_delete_manifest",
        description: "删除一份 manifest。",
        schema: || obj("", json!({ "id": str_("manifest id") }), &["id"]),
        route: |args| {
            Ok(route(
                "DELETE",
                &format!("/api/manifests/{}", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_reload_manifests",
        description: "**刷新事件**:从磁盘重新读一遍构建类型目录(`manifest/`),并告诉你相对上次\
                      新增/内容变了/删除了哪些。你在 daemon 那台机器上直接改过、或用别的工具写过\
                      那些 json 之后,调它一次;目录整个不见了会重新生成并把自带的模板复制回去。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route_body("POST", "/api/manifests/reload", json!({}))),
    },
    Tool {
        name: "cgw_manifest_status",
        description: "构建类型目录的现状:目录在哪、共几份、哪几份不合法、自带的模板缺哪些。\
                      排查\"为什么探测不到某个类型\"时先看它。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/manifests")),
    },
    Tool {
        name: "cgw_export_manifest",
        description: "导出一份构建类型的原文(给用户存成文件 / 贴到别处 / 做备份)。\
                      返回内容 + 建议的文件名;导入用 cgw_save_manifest(id + raw)。",
        schema: || obj("", json!({ "id": str_("manifest id(文件名,不含 .json)") }), &["id"]),
        route: |args| {
            Ok(route(
                "GET",
                &format!("/api/manifests/{}/raw", encode(&need(args, "id")?)),
            ))
        },
    },
    Tool {
        name: "cgw_detect_manifests",
        description: "按宿主机目录探测命中哪些构建类型(设计文档 §6.2:include.paths 必须**全部**匹配\
                      才命中)。命中多份时全都返回,由调用方自己选。",
        schema: || obj("", json!({ "sourcePath": str_("daemon 机器上的工程目录") }), &["sourcePath"]),
        route: |args| {
            Ok(route_body(
                "POST",
                "/api/manifests/detect",
                json!({ "sourcePath": need(args, "sourcePath")? }),
            ))
        },
    },

    // ---------------------------------------------------------- logs
    Tool {
        name: "cgw_log_days",
        description: "有哪些天的落盘日志可读(设计文档 §10),以及保留天数。",
        schema: || obj("无参数", json!({}), &[]),
        route: |_| Ok(route("GET", "/api/logs/days")),
    },
    Tool {
        name: "cgw_logs",
        description: "读某一天的落盘日志。可按来源(webui / cli / mcp / daemon)或日志域\
                      (客户机 id、job-xxx)过滤。",
        schema: || {
            obj(
                "",
                json!({
                    "day": str_("YYYY-MM-DD"),
                    "limit": num("最多返回多少条(取尾部)"),
                    "source": str_("只看某个来源"),
                    "scope": str_("只看某个日志域"),
                }),
                &["day"],
            )
        },
        route: |args| {
            let day = need(args, "day")?;
            let mut query: Vec<String> = Vec::new();
            if let Some(limit) = args.get("limit").and_then(Value::as_u64) {
                query.push(format!("limit={limit}"));
            }
            if let Some(source) = optional(args, "source") {
                query.push(format!("source={}", encode(&source)));
            }
            if let Some(scope) = optional(args, "scope") {
                query.push(format!("scope={}", encode(&scope)));
            }
            let suffix = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };
            Ok(route("GET", &format!("/api/logs/{}{suffix}", encode(&day))))
        },
    },
];

// ==================================================================== Tool table helpers

fn route(method: &'static str, path: &str) -> Route {
    Route {
        method,
        path: path.to_string(),
        body: None,
    }
}

fn route_body(method: &'static str, path: &str, body: Value) -> Route {
    Route {
        method,
        path: path.to_string(),
        body: Some(body),
    }
}

fn str_(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn num(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}

fn bool_(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

fn enum_(description: &str, values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values, "description": description })
}

fn obj(description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "description": description,
        "properties": properties,
        "required": required,
    })
}

/// Use the daemon-generated schema as the tool argument schema.
///
/// `cgw_save_client` takes exactly the `POST /api/clients` body, so no second
/// hand-written field list can drift.
fn daemon_schema(name: &str) -> Value {
    let value = jsonschema::named(name).unwrap_or_else(|| json!({ "type": "object" }));
    embed(value)
}

/// Task schema: `/api/schema/task` without its envelope, for `cgw_save_task.task`.
fn task_schema() -> Value {
    embed(jsonschema::named("task").unwrap_or_else(|| json!({ "type": "object" })))
}

/// Turn a standalone draft-07 schema into one that can be embedded in another.
///
/// Drops the document-level keys (`$schema` / `title` / `definitions`) and expands
/// `#/definitions/...` refs in place: under a new root they would point at the wrong
/// place, and the daemon's task / batch / client schemas all reference Location / Side.
fn embed(mut value: Value) -> Value {
    let definitions = value
        .get("definitions")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    inline_refs(&mut value, &definitions, 0);
    if let Some(object) = value.as_object_mut() {
        for key in ["$schema", "title", "definitions", "$id"] {
            object.remove(key);
        }
    }
    value
}

fn inline_refs(value: &mut Value, definitions: &Map<String, Value>, depth: usize) {
    // The depth cap only guards real cycles; schema trees are deep, so stay loose.
    if depth > 32 {
        return;
    }
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/definitions/"))
            {
                if let Some(definition) = definitions.get(name) {
                    let mut replacement = definition.clone();
                    inline_refs(&mut replacement, definitions, depth + 1);
                    if let Some(description) = object.get("description").cloned() {
                        if let Some(target) = replacement.as_object_mut() {
                            target.entry("description").or_insert(description);
                        }
                    }
                    *value = replacement;
                    return;
                }
            }
            for child in object.values_mut() {
                inline_refs(child, definitions, depth + 1);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                inline_refs(item, definitions, depth + 1);
            }
        }
        _ => return,
    }
    collapse_single_all_of(value);
}

/// Flatten `allOf: [X]` to `X` when it wraps a single entry (how schemars emits a
/// described `$ref`).
fn collapse_single_all_of(value: &mut Value) {
    let Some(object) = value.as_object() else {
        return;
    };
    let Some(Value::Array(items)) = object.get("allOf") else {
        return;
    };
    if items.len() != 1 || object.len() > 2 {
        return;
    }
    let mut inner = items[0].clone();
    if let Some(description) = object.get("description").cloned() {
        if let Some(target) = inner.as_object_mut() {
            target.entry("description").or_insert(description);
        }
    }
    *value = inner;
}

/// Flat `cgw_save_task` arguments (`{projectId, kind, manifest, ...}`) -> the `task` object.
///
/// Only top-level fields declared by the task schema move, so `projectId` stays out.
fn task_from_args(args: &Value) -> Value {
    let properties = task_schema()
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut task = Map::new();
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            if properties.contains_key(key) && !value.is_null() {
                task.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(task)
}

fn need(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(format!("缺少参数 {key}")),
    }
}

fn optional(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

/// Read a boolean argument: missing and `null` are `false`, and `"true"`-style strings
/// work too, since some clients send booleans as strings.
fn bool_arg(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ),
        _ => false,
    }
}

/// Percent-encode a path segment or query value (everything outside RFC 3986 unreserved).
fn encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn to_pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

// ==================================================================== Result trimming

/// Big responses blow up the model's context, so list endpoints keep key fields only.
///
/// This is not a second API: the same data trimmed, with the daemon's own field names.
fn summarize(tool: &str, result: &Value) -> Value {
    let keep = |items: &Value, fields: &[&str]| -> Vec<Value> {
        items
            .as_array()
            .map(|array| {
                array
                    .iter()
                    .map(|item| {
                        let mut object = Map::new();
                        for field in fields {
                            object.insert(
                                field.to_string(),
                                item.get(field).cloned().unwrap_or(Value::Null),
                            );
                        }
                        Value::Object(object)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };

    let projects = result.get("projects").cloned().unwrap_or(Value::Null);
    match tool {
        "cgw_list_tasks" => json!({
            "projects": keep(&projects, &["id", "name", "tasks", "batches"]),
        }),
        "cgw_list_batches" => json!({
            "projects": keep(&projects, &["id", "name", "batches"]),
        }),
        "cgw_list_projects" => {
            let raw = projects.as_array().cloned().unwrap_or_default();
            let trimmed = raw
                .iter()
                .map(|item| {
                    let mut object = Map::new();
                    for field in [
                        "id",
                        "name",
                        "clientId",
                        "sourcePath",
                        "remotePath",
                        "lastSyncOk",
                        "lastBuildOk",
                        "lastArtifact",
                    ] {
                        object.insert(
                            field.to_string(),
                            item.get(field).cloned().unwrap_or(Value::Null),
                        );
                    }
                    let count = |field: &str| {
                        item.get(field)
                            .map(|v| {
                                if let Some(a) = v.as_array() {
                                    a.len()
                                } else if let Some(o) = v.as_object() {
                                    o.len()
                                } else {
                                    0
                                }
                            })
                            .unwrap_or(0)
                    };
                    object.insert("taskCount".to_string(), Value::from(count("tasks")));
                    object.insert("batchCount".to_string(), Value::from(count("batches")));
                    Value::Object(object)
                })
                .collect::<Vec<_>>();
            json!({
                "projects": trimmed,
                "running": result.get("running").cloned().unwrap_or(Value::Null),
            })
        }
        "cgw_list_clients" => json!({
            "clients": keep(
                result.get("clients").unwrap_or(&Value::Null),
                &[
                    "id", "name", "host", "port", "auth", "tags", "baseDir", "baseDirResolved",
                    "hostLock",
                ],
            ),
        }),
        "cgw_list_manifests" => json!({
            "manifests": keep(
                result.get("manifests").unwrap_or(&Value::Null),
                &[
                    "id", "displayName", "platform", "valid", "errors", "include", "variables",
                    "build",
                ],
            ),
        }),
        _ => result.clone(),
    }
}

// ==================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Listen;

    fn schema_of(name: &str) -> Value {
        (tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no such tool: {name}"))
            .schema)()
    }

    /// Unique names, all `cgw_`-prefixed; a duplicate makes `tools/call` pick the first.
    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for tool in tools() {
            assert!(tool.name.starts_with("cgw_"), "bad name: {}", tool.name);
            assert!(seen.insert(tool.name), "duplicate tool name: {}", tool.name);
            assert!(!tool.description.trim().is_empty(), "{} has no description", tool.name);
        }
        assert_eq!(seen.len(), 43, "tool count changed; update the docs");
    }

    /// Every `required` field must really be declared in `properties`, or clients get a
    /// schema they can never satisfy.
    #[test]
    fn required_fields_are_declared() {
        for tool in tools() {
            let schema = (tool.schema)();
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{} schema has no properties", tool.name));
            for field in schema
                .get("required")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let field = field.as_str().unwrap_or_default();
                assert!(
                    properties.contains_key(field),
                    "{} requires undeclared field {field}",
                    tool.name
                );
            }
        }
    }

    /// Feed schema-generated minimal arguments to every route, so a missing `need()`
    /// blows up here.
    #[test]
    fn every_tool_can_build_a_route() {
        for tool in tools() {
            let args = sample_args(&(tool.schema)());
            let route = (tool.route)(&args)
                .unwrap_or_else(|err| panic!("{} cannot build a request: {err}", tool.name));
            assert!(
                route.path.starts_with("/api/"),
                "{} hit a non-API path: {}",
                tool.name,
                route.path
            );
            assert!(
                matches!(route.method, "GET" | "POST" | "PUT" | "DELETE"),
                "{} has a bad method: {}",
                tool.name,
                route.method
            );
        }
    }

    /// No field name in a schema may be empty, and the top level must be `type: object`.
    #[test]
    fn schemas_are_object_shaped() {
        for tool in tools() {
            let schema = (tool.schema)();
            assert_eq!(
                schema.get("type").and_then(Value::as_str),
                Some("object"),
                "{} inputSchema is not an object",
                tool.name
            );
            assert!(schema.get("properties").is_some(), "{} has no properties", tool.name);
        }
    }

    /// Tool argument schemas *are* the daemon schemas: no second field list to compare
    /// any more, which is what moving MCP into the daemon bought.
    #[test]
    fn tool_schemas_are_the_daemon_schemas() {
        let client = schema_of("cgw_save_client");
        // Both sides run through `embed` so `$ref`s expand the same way, because a
        // client cannot resolve them otherwise; everything else must match exactly.
        let reference = embed(jsonschema::named("client").unwrap());
        for field in reference
            .get("properties")
            .and_then(Value::as_object)
            .unwrap()
            .keys()
        {
            assert!(
                client["properties"].get(field).is_some(),
                "cgw_save_client is missing field {field}"
            );
            assert_eq!(
                client["properties"][field]["type"],
                reference["properties"][field]["type"],
                "cgw_save_client.{field} type differs from the daemon's"
            );
        }
        assert_eq!(client["required"], reference["required"]);

        let project = schema_of("cgw_create_project");
        let reference = embed(jsonschema::named("project").unwrap());
        assert_eq!(project["properties"], reference["properties"]);

        let task = &schema_of("cgw_save_task")["properties"]["task"];
        let reference = embed(jsonschema::named("task").unwrap());
        assert_eq!(task["properties"], reference["properties"]);
        assert_eq!(task["oneOf"], reference["oneOf"]);
    }

    /// Embedded sub-schemas must be self-contained: `$ref`s fully expanded, none dangling.
    #[test]
    fn embedded_schemas_have_no_dangling_refs() {
        fn walk(value: &Value) -> usize {
            match value {
                Value::Object(object) => {
                    let mut count = usize::from(object.contains_key("$ref"));
                    for child in object.values() {
                        count += walk(child);
                    }
                    count
                }
                Value::Array(items) => items.iter().map(walk).sum(),
                _ => 0,
            }
        }
        for name in jsonschema::NAMES {
            let embedded = embed(jsonschema::named(name).unwrap());
            assert_eq!(walk(&embedded), 0, "{name} still has unexpanded $ref");
            assert!(
                embedded.get("definitions").is_none(),
                "{name} still carries definitions"
            );
        }
        // Location fields must really be present, proving the expansion is not a shell.
        let task = task_schema();
        assert!(task["properties"]["to"]["properties"]["side"].is_object());
    }

    /// The categorized shape must flatten to the daemon's patch without losing the old
    /// flat fields.
    #[test]
    fn categorized_config_flattens_to_the_http_patch() {
        let flat = categorized(&json!({
            "port": { "webui": 65251, "mcp": 3994 },
            "listen": { "mcp": "wan" },
            "user": { "name": "alice", "password": "s3cret" },
            "logRetainDays": 9,
            "roots": { "add": ["F:\\CODEX"], "remove": ["D:\\old"] },
            "mcp": { "enabled": true },
        }));
        assert_eq!(flat["port"], 65251);
        assert_eq!(flat["mcp"]["port"], 3994, "the two ports must land on port / mcp.port");
        assert_eq!(flat["listen"], Value::Null, "mcp alone must not touch webui listen");
        assert_eq!(flat["mcp"]["listen"], "wan");
        assert_eq!(flat["username"], "alice");
        assert_eq!(flat["password"], "s3cret");
        assert_eq!(flat["logRetainDays"], 9);
        assert_eq!(flat["rootsAdd"][0], "F:\\CODEX");
        assert_eq!(flat["rootsRemove"][0], "D:\\old");
        // `mcp.enabled` and `mcp.port` come from different category fields and must
        // merge rather than replace each other.
        assert_eq!(flat["mcp"]["enabled"], true);
        assert!(flat.get("user").is_none() && flat.get("roots").is_none());

        // `user.clear` wipes the credentials
        let cleared = categorized(&json!({ "user": { "clear": true } }));
        assert_eq!(cleared["username"], "");
        assert_eq!(cleared["password"], "");

        // Legacy flat fields pass through unchanged
        let legacy = categorized(&json!({ "allowedRoots": ["C:\\work"], "port": 1234 }));
        assert_eq!(legacy["port"], 1234);
        assert_eq!(legacy["allowedRoots"][0], "C:\\work");
    }

    #[test]
    fn resources_cover_every_daemon_schema() {
        let resources = resources_json();
        // 5 schemas + 2 prose resources (guide / cli)
        assert_eq!(resources.len(), jsonschema::NAMES.len() + 2);
        for name in jsonschema::NAMES {
            assert!(
                resources
                    .iter()
                    .any(|item| item["uri"] == format!("{RESOURCE_PREFIX}{name}")),
                "missing resource {name}"
            );
        }
        for uri in [GUIDE_URI, CLI_URI] {
            let item = resources
                .iter()
                .find(|item| item["uri"] == uri)
                .unwrap_or_else(|| panic!("missing prose resource {uri}"));
            assert!(!item["description"].as_str().unwrap_or("").is_empty());
        }
    }

    /// `cgw://guide` must really be a playbook, not a placeholder.
    #[test]
    fn guide_covers_the_three_flows_and_points_at_cli() {
        for needle in [
            "第一次把一个项目跑起来",
            "日常重跑",
            "失败了怎么办",
            "cgw_run_batch",
            "projectBusy",
            "ephemeral",
            "cgw://cli",
            "cgw://schema/task",
            // Input/output contract (design doc §7.6.3): the agent must know that a
            // declared output has to be produced for real.
            "artifactMissing",
            "{{inputs.",
        ] {
            assert!(GUIDE.contains(needle), "guide does not contain {needle}");
        }
    }

    /// `cgw://cli` *is* `cgw help --json`: same table, not a second copy.
    #[test]
    fn cli_resource_is_the_same_table_as_the_cli() {
        let value: Value = serde_json::from_str(&cli_text()).expect("cgw://cli must be valid JSON");
        let commands = value["commands"].as_array().expect("commands");
        assert_eq!(
            commands.len(),
            crate::help::HELP.len(),
            "cgw://cli and CLI command counts differ (is there a second table?)"
        );
        assert_eq!(value["exitCodes"].as_object().map(|m| m.len()), Some(8));
        assert!(value["whenToUseCli"].as_str().is_some_and(|text| {
            text.contains("shell") && text.contains("exitCodes") && text.contains("cgw")
        }));
        assert!(value["selfDescribing"].is_string());
        // Entry by entry: whatever the CLI table has must be in the resource too.
        for entry in crate::help::HELP {
            assert!(
                commands
                    .iter()
                    .any(|item| item["name"] == entry.name && item["usage"] == entry.usage_en),
                "cgw://cli is missing {} / {}",
                entry.name,
                entry.usage_en
            );
        }
    }

    /// Prompts are the click-and-send teaching layer: listable, gettable, and clear about
    /// missing arguments.
    #[test]
    fn prompts_are_listed_and_gettable() {
        let listed = prompts_json();
        assert_eq!(listed.len(), PROMPTS.len());
        assert!(listed.len() >= 3, "need at least the three templates");
        for item in &listed {
            assert!(item["name"].is_string());
            assert!(item["title"].is_string());
            assert!(!item["description"].as_str().unwrap_or("").is_empty());
            assert!(item["arguments"].is_array());
        }

        for prompt in PROMPTS {
            // Fill every argument, so required ones can produce a body.
            let args = prompt
                .args
                .iter()
                .map(|(name, _, _)| ((*name).to_string(), json!("demo")))
                .collect::<serde_json::Map<_, _>>();
            let result = get_prompt(&json!({ "name": prompt.name, "arguments": args }))
                .unwrap_or_else(|err| panic!("{} is not gettable: {}", prompt.name, err));
            let text = result["messages"][0]["content"]["text"]
                .as_str()
                .unwrap_or_default();
            assert!(text.len() > 80, "{} body is too short: {text}", prompt.name);
        }

        // A missing required argument -> say which one
        let missing = get_prompt(&json!({ "name": "diagnose-job", "arguments": {} })).unwrap_err();
        assert!(missing["message"].as_str().unwrap_or("").contains("jobId"));
        // An unknown name -> list the available ones
        let unknown = get_prompt(&json!({ "name": "nope" })).unwrap_err();
        assert!(unknown["message"].as_str().unwrap_or("").contains("first-build"));
    }

    /// Instructions are the first layer of the double safety net: both resources must
    /// be named.
    #[test]
    fn instructions_point_at_both_resources() {
        let init = initialize_result(&json!({ "protocolVersion": "2025-06-18" }));
        let text = init["instructions"].as_str().unwrap_or_default();
        assert!(text.contains("cgw://guide"), "instructions does not point at guide");
        assert!(text.contains("cgw://cli"), "instructions does not point at cli");
        assert!(init["capabilities"]["prompts"].is_object(), "prompts capability not declared");
    }

    #[test]
    fn negotiates_protocol_version() {
        let known = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(known["protocolVersion"], "2024-11-05");
        let unknown = initialize_result(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(unknown["protocolVersion"], "2025-06-18");
        assert_eq!(unknown["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn failure_folds_the_daemon_error() {
        let raw = "HTTP 409 {\"error\":{\"code\":\"projectBusy\",\"message\":\"项目正忙\",\
                   \"jobId\":\"job-1\",\"kind\":\"build\",\"runningSeconds\":12,\
                   \"retryAfterSeconds\":10}}";
        let failure = Failure::parse(raw);
        assert_eq!(failure.status, 409);
        assert_eq!(failure.code, "projectBusy");
        let text = failure.describe();
        assert!(text.contains("job-1"), "{text}");
        assert!(text.contains("10 秒后再试"), "{text}");
        assert!(text.contains("项目正忙"), "{text}");
        let structured = failure.to_json();
        assert_eq!(structured["httpStatus"], 409);
        assert_eq!(structured["jobId"], "job-1");
    }

    /// Flat arguments must pick out the task fields, but never carry `projectId` in.
    #[test]
    fn flat_task_args_drop_routing_fields() {
        let task = task_from_args(&json!({
            "projectId": "p1",
            "kind": "copy",
            "from": { "side": "remote", "path": "a" },
            "to": { "side": "remote", "path": "b" },
            "nonsense": 1,
        }));
        assert_eq!(task["kind"], "copy");
        assert!(task.get("projectId").is_none());
        assert!(task.get("nonsense").is_none());
        assert_eq!(task["from"]["path"], "a");
    }

    #[test]
    fn encodes_path_segments() {
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
        assert_eq!(encode("job-1"), "job-1");
        assert_eq!(encode("中文"), "%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn decodes_basic_auth() {
        // "test:secret"
        assert_eq!(base64_decode("dGVzdDpzZWNyZXQ=").unwrap(), b"test:secret");
        assert!(base64_decode("!!!").is_err());
    }

    #[test]
    fn mcp_wan_rate_limits_failed_auth_attempts() {
        let dir = std::env::temp_dir().join(format!("cgw-mcp-auth-test-{}", crate::clients::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.json");
        let mut config = crate::config::Config::default();
        config.auth.username = "alice".to_string();
        config.auth.password_hash = crate::auth::hash_password("secret123").unwrap();
        config.auth.login_interval_seconds = 60;

        let web = Arc::new(crate::server::WebState::new(
            config_path,
            config,
            crate::i18n::Lang::ZhHans,
            12345,
            "http://127.0.0.1:12345".to_string(),
            None,
        ));
        let mcp_state = McpState {
            web,
            port: 12345,
            failed_attempts: Mutex::new(HashMap::new()),
        };

        let remote_addr: SocketAddr = "192.168.1.50:54321".parse().unwrap();
        let loopback_addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();

        // 1. Loopback always passes
        let empty_headers = HeaderMap::new();
        assert!(matches!(check_auth(&mcp_state, &empty_headers, loopback_addr), AuthResult::Ok));

        // 2. Non-loopback without header gets challenge Unauthorized (not recorded as brute-force)
        assert!(matches!(check_auth(&mcp_state, &empty_headers, remote_addr), AuthResult::Unauthorized));

        // 3. Submitting invalid credentials gets Unauthorized and records the failure
        let mut wrong_headers = HeaderMap::new();
        wrong_headers.insert(header::AUTHORIZATION, "Basic d3Jvbmc6d3Jvbmc=".parse().unwrap());
        assert!(matches!(check_auth(&mcp_state, &wrong_headers, remote_addr), AuthResult::Unauthorized));

        // 4. Second immediate attempt from same remote IP gets RateLimited!
        match check_auth(&mcp_state, &wrong_headers, remote_addr) {
            AuthResult::RateLimited(wait) => {
                assert!(wait > 0 && wait <= 60);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }

        // 5. Another remote IP is not blocked
        let other_remote: SocketAddr = "192.168.1.51:54321".parse().unwrap();
        assert!(matches!(check_auth(&mcp_state, &empty_headers, other_remote), AuthResult::Unauthorized));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_and_reports_disabled() {
        let runtime = McpRuntime::new();
        let disabled = Mcp {
            enabled: false,
            listen: Listen::Lan,
            port: None,
        };
        let status = runtime.status(&disabled);
        assert!(!status.running);
        assert!(status.url.is_empty());
        // When idle, report the requested port, not 0.
        assert_eq!(status.port, DEFAULT_PORT);
    }

    /// **The MCP port is independently defined**: whatever is configured is used, with no
    /// derivation from any other port.
    ///
    /// It once looked like "main port + 1", which was really Windows handing out
    /// consecutive ephemeral ports (`bind(:0)` right after the daemon). This pins the
    /// verbatim behaviour so nobody writes `daemon_port + 1` later.
    #[test]
    fn explicit_port_is_used_verbatim() {
        // First take a currently free port, then let bind use it.
        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let wanted = probe.local_addr().unwrap().port();
        drop(probe);

        let config = Mcp {
            enabled: true,
            listen: Listen::Lan,
            port: Some(wanted),
        };
        let (listener, note) = bind(&config).unwrap();
        assert_eq!(listener.local_addr().unwrap().port(), wanted);
        assert!(note.is_none(), "an explicit port must not have a fallback note: {note:?}");

        // Draw the line against derivation: the default port is an independent constant.
        assert_ne!(DEFAULT_PORT, 0);
        assert_eq!(config.requested_port(), Some(wanted));
    }

    /// Auto mode tries the default port first: when free it must be taken with no note.
    #[test]
    fn auto_bind_prefers_the_default_port() {
        let config = Mcp {
            enabled: true,
            listen: Listen::Lan,
            port: None,
        };
        let (listener, note) = bind(&config).unwrap();
        let port = listener.local_addr().unwrap().port();
        // 3994 may be taken by something else on this machine, so accept either it or a note.
        if port == DEFAULT_PORT {
            assert!(note.is_none());
        } else {
            assert!(note.is_some(), "a fallback without a note leaves the port change unexplained");
        }
    }

    /// Build arguments that fill every required field from the schema.
    fn sample_args(schema: &Value) -> Value {
        let mut args = Map::new();
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for field in required {
                let Some(field) = field.as_str() else { continue };
                let spec = schema
                    .get("properties")
                    .and_then(|value| value.get(field))
                    .cloned()
                    .unwrap_or(Value::Null);
                args.insert(field.to_string(), sample(&spec));
            }
        }
        Value::Object(args)
    }

    fn sample(spec: &Value) -> Value {
        match spec.get("type").and_then(Value::as_str) {
            Some("number") | Some("integer") => json!(1),
            Some("boolean") => json!(true),
            Some("array") => json!([]),
            Some("object") => json!({}),
            Some("string") => {
                if let Some(values) = spec.get("enum").and_then(Value::as_array) {
                    values.first().cloned().unwrap_or_else(|| json!("x"))
                } else {
                    json!("x")
                }
            }
            _ => json!("x"),
        }
    }
}
