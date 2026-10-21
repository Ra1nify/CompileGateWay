//! Commands the CLI window exposes to the frontend.
//!
//! Logs and commands are scoped per **window**: `main` for the main window, one
//! `instance-<id>` per client. The window label is the log scope name, so the
//! frontend never needs to know who it is.
//!
//! This uses Tauri IPC instead of HTTP on purpose: a window must show logs before the
//! HTTP server starts (e.g. when the port is taken), and IPC ignores that ordering.
//! The standalone CLI binary as the daemon's thin HTTP client is a later design phase.

use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager, Theme, WebviewUrl, WebviewWindowBuilder, Window};

use compilegateway::i18n::{self, label};
use compilegateway::log::{self, core, instance_label, LogEntry, Status, ERROR, INFO, INPUT, OUT, WARN};

#[derive(Serialize)]
pub struct Outcome {
    /// none | clear | exit
    pub action: &'static str,
}

fn outcome(action: &'static str) -> Outcome {
    Outcome { action }
}

#[tauri::command]
pub fn get_logs(window: Window) -> Vec<LogEntry> {
    core().snapshot(window.label())
}

#[tauri::command]
pub fn get_status(window: Window) -> Status {
    core().status(window.label())
}

/// On the main window "exit" quits the app; on an instance window it closes only that window.
#[tauri::command]
pub fn quit(app: AppHandle, window: Window) {
    if window.label() == log::MAIN_SCOPE {
        app.exit(0);
    } else {
        let _ = window.close();
    }
}

#[tauri::command]
pub fn run_command(app: AppHandle, window: Window, input: String) -> Outcome {
    let scope = window.label().to_string();
    let raw = input.trim().to_string();
    if raw.is_empty() {
        return outcome("none");
    }

    log::log_scoped(&scope, INPUT, raw.clone());

    if scope == log::MAIN_SCOPE {
        run_main(&app, &scope, &raw)
    } else {
        run_instance(&scope, &raw)
    }
}

// ------------------------------------------------------------------ HTTP client

/// Turns the CLI into an HTTP client of the daemon (design doc §3).
///
/// `api <METHOD> <PATH> [JSON]` reaches the same endpoints the WebUI uses, so the
/// **CLI is fully equivalent to the WebUI** with no text UI per screen. Loopback
/// requests skip auth on the daemon side, so no session cookie is needed.
fn run_api(scope: &str, t: &i18n::Texts, state: &log::Core, rest: &str) {
    let Some(port) = state.port() else {
        scoped_log(scope, WARN, t.api_unavailable);
        return;
    };

    let mut parts = rest.splitn(3, char::is_whitespace);
    let method = parts.next().unwrap_or("").trim().to_ascii_uppercase();
    let path = parts.next().unwrap_or("").trim().to_string();
    let body = parts.next().map(str::trim).filter(|s| !s.is_empty());

    if method.is_empty() || path.is_empty() {
        scoped_log(scope, INFO, t.api_usage);
        return;
    }
    if !path.starts_with('/') {
        scoped_log(scope, WARN, t.api_usage);
        return;
    }

    match compilegateway::httpclient::request(port, &method, &path, body) {
        Ok(response) => {
            scoped_log(
                scope,
                if response.ok() { INFO } else { WARN },
                format!("{method} {path} -> HTTP {}", response.status),
            );
            if !response.body.trim().is_empty() {
                scoped_log(scope, OUT, compilegateway::httpclient::pretty(response.body.trim()));
            }
        }
        Err(message) => scoped_log(scope, ERROR, message),
    }
}

/// HTTP version of `status`: read `/api/status` and `/api/projects` directly.
fn http_status(state: &log::Core) -> Option<String> {
    let port = state.port()?;
    let status: serde_json::Value =
        serde_json::from_str(&compilegateway::httpclient::request(port, "GET", "/api/status", None).ok()?.body)
            .ok()?;
    let projects: serde_json::Value =
        serde_json::from_str(&compilegateway::httpclient::request(port, "GET", "/api/projects", None).ok()?.body)
            .ok()?;
    let lang = state.lang();
    let t = i18n::text(lang);
    let get = |key: &str| status.get(key).and_then(|v| v.as_str()).unwrap_or("-");

    let mut out = String::new();
    let mut push = |k: &str, v: &str| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&label(k, 16));
        out.push_str(": ");
        out.push_str(v);
    };
    push(t.label_webui, get("url"));
    push(t.label_port, &status["port"].to_string());
    push(t.label_listen, get("listen"));
    push(t.label_language, get("language"));
    push(t.label_config, get("configPath"));
    push(
        t.clients_count,
        &projects["projects"]
            .as_array()
            .map(|list| list.len())
            .unwrap_or(0)
            .to_string(),
    );
    out.push('\n');
    out.push_str(&label(t.running_jobs, 16));
    out.push_str(": ");
    out.push_str(
        &projects["running"]
            .as_object()
            .map(|map| map.len())
            .unwrap_or(0)
            .to_string(),
    );
    Some(out)
}

// ------------------------------------------------------------------ settings (window side)

/// `set ...` - **anything the settings page can change, the window can change too**.
/// Parsing (`compilegateway::setcmd`) and the `/api/config` endpoint are shared with
/// the WebUI and the standalone CLI, so all three stay semantically identical.
fn run_set(scope: &str, t: &i18n::Texts, state: &log::Core, rest: &str) {
    let Some(port) = state.port() else {
        scoped_log(scope, WARN, t.api_unavailable);
        return;
    };
    let json_out = rest.split_whitespace().any(|token| token == "--json");
    let tokens: Vec<String> = compilegateway::setcmd::tokenize(rest)
        .into_iter()
        .filter(|token| token != "--json")
        .collect();

    match compilegateway::setcmd::parse(&tokens) {
        Ok(compilegateway::setcmd::Intent::Overview) => {
            let Some(config) = get_json(port, "/api/config") else {
                scoped_log(scope, WARN, t.api_unavailable);
                return;
            };
            if json_out {
                scoped_log(scope, OUT, compilegateway::httpclient::pretty(&config.to_string()));
                return;
            }
            scoped_log(scope, OUT, set_overview(t, &config));
        }
        Ok(compilegateway::setcmd::Intent::RootsList) => {
            let Some(config) = get_json(port, "/api/config") else {
                scoped_log(scope, WARN, t.api_unavailable);
                return;
            };
            let mut out = String::from(t.set_roots_title);
            for (index, root) in config["hostRoots"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                out.push('\n');
                out.push_str("  ");
                out.push_str(&index.to_string());
                out.push_str(".  ");
                out.push_str(root.as_str().unwrap_or("-"));
                if index == 0 {
                    out.push_str(t.set_roots_default);
                }
            }
            out.push('\n');
            out.push_str(t.set_roots_hint);
            scoped_log(scope, OUT, out);
        }
        Ok(compilegateway::setcmd::Intent::Patch(patch)) => {
            let body = patch.to_string();
            match compilegateway::httpclient::request(port, "POST", "/api/config", Some(&body)) {
                Ok(response) if response.ok() => {
                    let parsed: serde_json::Value =
                        serde_json::from_str(&response.body).unwrap_or(serde_json::Value::Null);
                    if json_out {
                        scoped_log(scope, OUT, compilegateway::httpclient::pretty(&response.body));
                        return;
                    }
                    let restart = parsed["restartRequired"].as_bool() == Some(true);
                    scoped_log(
                        scope,
                        INFO,
                        if restart {
                            t.set_saved_restart
                        } else {
                            t.set_saved
                        },
                    );
                }
                Ok(response) => {
                    // daemon's structured error: print both the code and the human message
                    let parsed: serde_json::Value =
                        serde_json::from_str(&response.body).unwrap_or(serde_json::Value::Null);
                    let code = parsed["error"]["code"].as_str().unwrap_or("?");
                    let message = parsed["error"]["message"]
                        .as_str()
                        .unwrap_or(response.body.trim());
                    scoped_log(
                        scope,
                        WARN,
                        format!("{}: HTTP {} {code} - {message}", t.set_failed, response.status),
                    );
                }
                Err(message) => scoped_log(scope, ERROR, message),
            }
        }
        Err(err) => scoped_log(scope, WARN, err.message()),
    }
}

fn get_json(port: u16, path: &str) -> Option<serde_json::Value> {
    let response = compilegateway::httpclient::request(port, "GET", path, None).ok()?;
    if !response.ok() {
        return None;
    }
    serde_json::from_str(&response.body).ok()
}

/// Bare `set` / `set all`: list every settings page item together with how to change it.
fn set_overview(t: &i18n::Texts, config: &serde_json::Value) -> String {
    let mut out = String::from(t.set_overview_title);
    let mut push = |line: String| {
        out.push('\n');
        out.push_str(&line);
    };
    push(String::new());
    push(format!("  {}{}", label(t.set_group_ports, 16), t.set_usage_port));
    push(format!(
        "    {:<14}{}",
        "webui", config["port"]
    ));
    push(format!(
        "    {:<14}{}",
        "mcp", config["mcp"]["port"]
    ));
    push(String::new());
    push(format!("  {}{}", label(t.set_group_listen, 16), t.set_usage_listen));
    push(format!(
        "    {:<14}{}",
        "webui",
        config["listen"].as_str().unwrap_or("-")
    ));
    push(format!(
        "    {:<14}{}",
        "mcp",
        config["mcp"]["listen"].as_str().unwrap_or("-")
    ));
    push(String::new());
    push(format!("  {}{}", label(t.set_group_user, 16), t.set_usage_user));
    push(format!(
        "    {:<14}{}",
        "name",
        config["username"].as_str().unwrap_or("")
    ));
    push(format!(
        "    {:<14}{}",
        "password",
        if config["hasPassword"].as_bool() == Some(true) {
            t.set_password_set
        } else {
            t.set_password_none
        }
    ));
    push(String::new());
    push(format!("  {}{}", label(t.set_group_ui, 16), ""));
    push(format!(
        "    {:<14}{}   ({})",
        "language",
        config["language"].as_str().unwrap_or("-"),
        t.set_usage_language
    ));
    push(format!(
        "    {:<14}{}   ({})",
        "login-interval",
        config["loginIntervalSeconds"],
        t.set_usage_login_interval
    ));
    push(format!(
        "    {:<14}{}   ({})",
        "log-retain",
        config["logRetainDays"],
        t.set_usage_log_retain
    ));
    push(String::new());
    push(format!("  {}{}", label(t.set_group_roots, 16), t.set_usage_roots));
    for (index, root) in config["hostRoots"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        push(format!("    {index}. {}", root.as_str().unwrap_or("-")));
    }
    push(String::new());
    push(format!("  {}{}", label(t.set_group_mcp, 16), t.set_usage_mcp));
    push(format!(
        "    {:<14}{}",
        "enabled",
        if config["mcp"]["enabled"].as_bool() == Some(true) {
            "true"
        } else {
            "false"
        }
    ));
    push(format!(
        "    {:<14}{}",
        "endpoint",
        config["mcp"]["endpoint"].as_str().unwrap_or("-")
    ));
    push(String::new());
    push(format!("  {}", t.set_overview_hint));
    out
}

// ------------------------------------------------------------------ main window

fn run_main(app: &AppHandle, scope: &str, raw: &str) -> Outcome {
    let state = core();
    let lang = state.lang();
    let t = i18n::text(lang);
    let (command, rest) = split(raw);

    match command {
        // `help` / `help <topic>` / `help --<topic>`: the last two document one topic,
        // the same thing typing that topic bare shows (`help --set` == bare `set`).
        "help" | "?" => {
            let topic = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('-');
            if topic.is_empty() || topic == "help" {
                let mut out = String::from(t.help_header);
                for (name, desc) in main_commands(&t) {
                    out.push('\n');
                    out.push_str("  ");
                    out.push_str(&label(name, 10));
                    out.push_str(desc);
                }
                out.push('\n');
                out.push_str(t.help_topics_hint);
                scoped_log(scope, OUT, out);
            } else if let Some(line) = main_command_doc(&t, topic) {
                scoped_log(scope, OUT, line);
            } else {
                scoped_log(scope, WARN, t.help_no_topic.replace("{}", topic));
                scoped_log(scope, INFO, t.unknown_hint);
            }
        }
        "status" => {
            // Prefer HTTP for authoritative state (design doc §3); fall back to in-process
            // reads when the service is down (e.g. port taken) so logs still show.
            match http_status(state) {
                Some(text) => scoped_log(scope, OUT, text),
                None => {
                    let mut out = config_status(&t, state);
                    out.push('\n');
                    out.push_str(&label(t.clients_count, 16));
                    out.push_str(": ");
                    out.push_str(&client_count().to_string());
                    scoped_log(scope, OUT, out);
                    scoped_log(scope, WARN, t.api_unavailable);
                }
            }
        }
        "api" => run_api(scope, &t, state, rest),
        "set" => run_set(scope, &t, state, rest),
        "diag" => open_diag(app, scope, &t, rest),
        "webui" | "open" => match state.url() {
            Some(url) => match opener::open(&url) {
                Ok(()) => scoped_log(scope, INFO, t.browser_opened),
                Err(err) => {
                    scoped_log(scope, WARN, t.browser_failed);
                    scoped_log(scope, OUT, format!("  {url}  ({err})"));
                }
            },
            None => scoped_log(scope, WARN, t.webui_unavailable),
        },
        "clear" | "cls" => {
            state.clear(scope);
            return outcome("clear");
        }
        "exit" | "quit" => {
            scoped_log(scope, INFO, t.bye);
            return outcome("exit");
        }
        other => {
            // **All other commands forward to the standalone CLI's implementation**
            // (`compilegateway::client`), so the window and pwsh run one code path and one
            // set of output wording, which is what makes WebUI / window / standalone CLI
            // genuinely consistent instead of three lookalike implementations.
            let _ = other;
            if !forward_to_client(scope, state, raw) {
                scoped_log(scope, WARN, format!("unknown command: {other}"));
                scoped_log(scope, INFO, t.unknown_hint);
            }
        }
    }

    outcome("none")
}

/// `diag <name or id>`: open the instance window for a client (never created by default).
fn open_diag(app: &AppHandle, scope: &str, t: &i18n::Texts, target: &str) {
    let Some(web) = compilegateway::state::web() else {
        scoped_log(scope, WARN, t.webui_unavailable);
        return;
    };
    let clients = web.clients_snapshot();

    if clients.is_empty() {
        scoped_log(scope, WARN, t.diag_no_clients);
        return;
    }
    if target.is_empty() {
        scoped_log(scope, INFO, t.diag_pick);
        for client in &clients {
            scoped_log(scope, 
                OUT,
                format!("  {}  ({}:{})", client.name, client.host, client.port),
            );
        }
        return;
    }

    let needle = target.to_lowercase();
    let mut matches: Vec<_> = clients
        .iter()
        .filter(|c| c.id == target || c.name.to_lowercase() == needle)
        .cloned()
        .collect();
    if matches.is_empty() {
        matches = clients
            .into_iter()
            .filter(|c| c.name.to_lowercase().contains(&needle))
            .collect();
    }

    match matches.len() {
        0 => scoped_log(scope, WARN, t.diag_not_found.replace("{}", target)),
        1 => {
            let client = matches.into_iter().next().unwrap();
            let window_label = instance_label(&client.id);
            open_instance_window(app, &client, &window_label);
            scoped_log(scope, 
                INFO,
                format!("{}: {} [{}]", t.diag_opened, client.name, window_label),
            );
        }
        _ => {
            scoped_log(scope, WARN, t.diag_ambiguous.replace("{}", target));
            for client in matches {
                scoped_log(scope, 
                    OUT,
                    format!("  {}  ({}:{})", client.name, client.host, client.port),
                );
            }
        }
    }
}

fn open_instance_window(app: &AppHandle, client: &compilegateway::clients::Client, label: &str) {
    let app = app.clone();
    let client = client.clone();
    let label = label.to_string();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(existing) = app.get_webview_window(&label) {
            let _ = existing.set_focus();
            return;
        }
        let title = format!(
            "CompileGateway - {} ({}:{})",
            client.name, client.host, client.port
        );
        let built = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(760.0, 460.0)
            .min_inner_size(420.0, 260.0)
            .theme(Some(Theme::Dark))
            .build();
        if let Err(err) = built {
            log::log_scoped(
                log::MAIN_SCOPE,
                WARN,
                i18n::window_open_failed(i18n::current(), &err.to_string()),
            );
        }
    });
}

// ------------------------------------------------------------------ instance window

fn run_instance(scope: &str, raw: &str) -> Outcome {
    let state = core();
    let lang = state.lang();
    let t = i18n::text(lang);
    let (command, _rest) = split(raw);

    let Some(client) = compilegateway::state::client_for_scope(scope) else {
        scoped_log(scope, WARN, t.diag_vanished);
        return outcome("none");
    };

    match command {
        "help" | "?" => {
            let mut out = String::from(t.help_header);
            for (name, desc) in [
                ("help", t.cmd_help),
                ("status", t.cmd_instance_status),
                ("test", t.cmd_test),
                ("clear", t.cmd_clear),
                ("exit", t.cmd_close_window),
            ] {
                out.push('\n');
                out.push_str("  ");
                out.push_str(&label(name, 10));
                out.push_str(desc);
            }
            scoped_log(scope, OUT, out);
        }
        "status" => {
            let host_key = if client.host_key.is_empty() {
                t.host_key_none.to_string()
            } else {
                client.host_key.clone()
            };
            let mut out = String::new();
            let mut push = |k: &str, v: &str| {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&label(k, 16));
                out.push_str(": ");
                out.push_str(v);
            };
            push(t.lbl_name, &client.name);
            push(t.lbl_host, &client.host);
            push(t.lbl_port, &client.port.to_string());
            push(t.lbl_auth, &compilegateway::state::auth_label(&t, &client));
            push(t.lbl_tags, &client.tags.join(", "));
            push(t.lbl_base_dir, &client.base_dir);
            if !client.base_dir_resolved.is_empty() {
                push(t.project_remote_label, &client.base_dir_resolved);
            }
            push(t.host_key, &host_key);
            scoped_log(scope, OUT, out);
        }
        "test" => {
            scoped_log(scope, INFO, format!("{}...", t.testing));
            let result = tauri::async_runtime::block_on(compilegateway::ssh::probe(
                &client,
                Duration::from_secs(15),
            ));
            scoped_log(scope, 
                if result.ok { INFO } else { WARN },
                format!("{} - {}", result.message, result.output.trim()),
            );
        }
        "clear" | "cls" => {
            state.clear(scope);
            return outcome("clear");
        }
        "exit" | "quit" => {
            scoped_log(scope, INFO, t.bye);
            return outcome("exit");
        }
        other => {
            scoped_log(scope, WARN, format!("unknown command: {other}"));
            scoped_log(scope, INFO, t.unknown_hint);
        }
    }

    outcome("none")
}

// ------------------------------------------------------------------ shared

/// Commands the main window accepts (name + one line). **Shared by the help list and
/// `help --<topic>`** so adding a command can't silently skip its docs (or vice versa).
fn main_commands(t: &i18n::Texts) -> Vec<(&'static str, &'static str)> {
    vec![
        ("help", t.cmd_help),
        ("status", t.cmd_status),
        ("set", t.cmd_set),
        ("api", t.cmd_api),
        ("diag", t.cmd_diag),
        ("webui", t.cmd_webui),
        ("clear", t.cmd_clear),
        ("exit", t.cmd_exit),
    ]
}

/// Detailed docs for one topic (all of `help --api` / `help set` / `help --set` land here).
fn main_command_doc(t: &i18n::Texts, topic: &str) -> Option<String> {
    let mut out = String::new();
    let mut push = |line: &str| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    };
    match topic {
        "help" => {
            push(t.cmd_help);
            push("");
            push(t.doc_help);
        }
        "status" => {
            push(t.cmd_status);
            push("");
            push(t.doc_status);
        }
        "set" => {
            push(t.cmd_set);
            push("");
            push(t.set_help);
        }
        "api" => {
            push(t.cmd_api);
            push("");
            push(t.doc_api);
        }
        "diag" => {
            push(t.cmd_diag);
            push("");
            push(t.doc_diag);
        }
        "webui" | "open" => {
            push(t.cmd_webui);
            push("");
            push(t.doc_webui);
        }
        "clear" | "cls" => {
            push(t.cmd_clear);
            push("");
            push(t.doc_clear);
        }
        "exit" | "quit" => {
            push(t.cmd_exit);
            push("");
            push(t.doc_exit);
        }
        _ => return None,
    }
    Some(out)
}

fn scoped_log(scope: &str, level: &'static str, text: impl Into<String>) {
    log::log_scoped(scope, level, text);
}

fn split(raw: &str) -> (&str, &str) {
    match raw.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (raw, ""),
    }
}

fn client_count() -> usize {
    compilegateway::state::web()
        .map(|web| web.clients_snapshot().len())
        .unwrap_or(0)
}

fn config_status(t: &i18n::Texts, state: &log::Core) -> String {
    let listen = compilegateway::state::web()
        .map(|web| compilegateway::lock::read(&web.config).listen)
        .unwrap_or(compilegateway::config::Listen::Lan);
    let auth_line = compilegateway::state::web()
        .map(|web| compilegateway::state::auth_text(t, &compilegateway::lock::read(&web.config)))
        .unwrap_or_else(|| t.auth_none.to_string());

    let mut out = String::new();
    let mut push = |k: &str, v: &str| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&label(k, 16));
        out.push_str(": ");
        out.push_str(v);
    };
    push(
        t.label_config,
        &state
            .config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    push(t.label_language, &state.lang_note());
    push(t.label_listen, compilegateway::state::listen_text(t, listen));
    push(
        t.label_port,
        &state
            .port()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );
    push(t.label_webui, &state.url().unwrap_or_default());
    push(t.label_auth, &auth_line);
    out
}

/// Run one command line through the shared implementation, logging into this window.
///
/// Returns `false` when this is not a command the standalone CLI knows (the caller then
/// prints the unknown-command hint).
fn forward_to_client(scope: &str, state: &log::Core, raw: &str) -> bool {
    let mut tokens: Vec<String> = compilegateway::setcmd::tokenize(raw);
    if tokens.is_empty() {
        return true;
    }
    // Ask first whether the standalone CLI knows this command: if not, don't swallow it.
    if !compilegateway::client::is_known(&tokens) {
        return false;
    }
    // Force the **in-process** daemon: the window and daemon are one, so don't guess an address
    if let Some(port) = state.port() {
        if !tokens.iter().any(|token| token == "--url") {
            tokens.push("--url".to_string());
            tokens.push(format!("http://127.0.0.1:{port}"));
        }
    }
    let owned = scope.to_string();
    let sink_scope = owned.clone();
    // A guard restores the sink on every exit path, including a panic inside `run_line` (S14).
    let result = {
        let _sink = compilegateway::client::SinkGuard::install(Box::new(move |line: &str| {
            if !line.is_empty() {
                scoped_log(&sink_scope, OUT, line.to_string());
            }
        }));
        compilegateway::client::run_line(&tokens)
    };
    if let Err(error) = result {
        let message = error.message.clone();
        if !message.trim().is_empty() {
            scoped_log(&owned, WARN, message);
        }
    }
    true
}