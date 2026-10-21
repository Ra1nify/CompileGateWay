//! Boot flow: config -> language -> listen mode -> port -> HTTP server.
//!
//! Shell-agnostic: the Tauri app and the headless daemon both call it; only
//! [`BootOptions`] differs (open a browser, native dialog available).

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use crate::config::{self, Listen};
use crate::dialog::SharedDialog;
use crate::i18n::{self, Lang};
use crate::log::{self, core, INFO, OUT, WARN};
use crate::logfile;
use crate::manifest;
use crate::net;
use crate::server::{self, WebState};
use crate::state;
use crate::workflow;

pub const APP_NAME: &str = "CompileGateway";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const LABEL_WIDTH: usize = 16;

pub struct BootOptions {
    /// Open the WebUI in the system browser after boot (desktop shell yes, daemon no).
    pub open_browser: bool,
    /// Native file picker. Pass `None` in headless environments.
    pub dialog: Option<SharedDialog>,
}

pub struct Booted {
    pub url: String,
    pub port: u16,
    pub web: Option<Arc<WebState>>,
    /// Startup failed (port in use, etc.); the shell keeps the window or exits.
    pub fatal: bool,
}

pub fn out(text: impl Into<String>) {
    log::log(OUT, text);
}

pub fn info(text: impl Into<String>) {
    log::log(INFO, text);
}

pub fn warn(text: impl Into<String>) {
    log::log(WARN, text);
}

/// Aligned `label: value` line.
fn kv(key: &str, value: &str) -> String {
    format!("{}: {value}", i18n::label(key, LABEL_WIDTH))
}

/// Boot the app. Logs go to `log::core()`; the shell attaches the log sink first.
pub fn bootstrap(options: BootOptions) -> Booted {
    let state = core();

    // ---- 1. Read (or create) the config file -------------------------------------------
    let loaded = match config::load() {
        Ok(loaded) => loaded,
        Err(err) => {
            let lang = state.lang();
            state.fatal(describe_load_error(lang, err));
            return failed("", 0);
        }
    };
    state.set_config_path(loaded.path.clone());

    // First run: seed the bundled build-type templates into manifest/; existing dirs are left alone
    let prepared = manifest::seed_templates(&loaded.path);
    // Same for the cross-project workflow templates under workflows/ (design doc §14.3)
    let workflows = workflow::seed_templates(&loaded.path);

    // ---- 1.5 Log to disk (design doc §10)----------------------------------------
    // Attached before language resolution so boot-time logs land on disk too.
    let log_dir = logfile::dir_for(&loaded.path);
    let retain = loaded.config.log_retain_days;
    let log_file = std::sync::Arc::new(logfile::LogFile::new(log_dir.clone(), retain));
    log::core().attach_file(log_file.clone());

    // ---- 2. Resolve the language --------------------------------------------------------
    let (lang, lang_note) = Lang::resolve(&loaded.config.language);
    state.set_lang(lang, lang_note.clone());
    // Publish it process-wide for the deep layers that cannot see a `Lang` (see `i18n::current`).
    i18n::set_current(lang);
    let t = i18n::text(lang);

    out(format!("{APP_NAME} {VERSION}"));
    out(kv(t.label_config, &loaded.path.display().to_string()));
    out(kv(t.label_language, &lang_note));
    out(kv(t.label_listen, state::listen_text(&t, loaded.config.listen)));
    out(kv(t.label_manifests, &prepared.dir.display().to_string()));
    out(kv(t.label_workflows, &workflows.dir.display().to_string()));
    if let Some(home) = crate::paths::cgw_home() {
        // Tells the user that this file is how `cgw` finds us from any directory
        out(kv(t.label_cgw_home, &home.display().to_string()));
    }
    out(kv(
        t.label_logs,
        &format!(
            "{} ({} {})",
            log_dir.display(),
            retain,
            t.unit_days
        ),
    ));

    // The build-type directory is **yours**: we only speak up on the first run or
    // after a migration, and never touch what is already inside.
    if prepared.created {
        info(format!(
            "{}: {} ({} {})",
            t.manifests_seeded,
            prepared.dir.display(),
            prepared.seeded.len(),
            t.unit_files
        ));
    }
    if prepared.migrated {
        info(format!(
            "{}: {} -> {}",
            t.manifests_migrated,
            manifest::legacy_dir(&loaded.path).display(),
            prepared.dir.display()
        ));
    }

    if loaded.created {
        info(format!("{}: {}", t.config_created, loaded.path.display()));
    }
    if !i18n::is_known_language(&loaded.config.language) {
        warn(format!(
            "{}: {:?}",
            t.config_unknown_language, loaded.config.language
        ));
    }

    // ---- 3. Pick the port --------------------------------------------------------
    // Port set in config -> must be free, otherwise fail (no retry, no fallback)
    // No port in config   -> bind :0 and let the system pick a free one
    let (listener, port, from_config) = match bind_listener(&loaded.config) {
        Ok(value) => value,
        Err(BindError::InUse { port }) => {
            state.fatal(i18n::err_port_in_use(lang, port));
            return failed("", port);
        }
        Err(BindError::Other { addr, err }) => {
            state.fatal(i18n::err_bind_failed(lang, &addr, &err.to_string()));
            return failed("", 0);
        }
    };

    let port_note = if from_config {
        t.port_source_config
    } else {
        t.port_source_auto
    };
    out(kv(t.label_port, &format!("{port} ({port_note})")));

    // lan is reachable from this machine only, so localhost works; wan needs the host IP other devices can use
    let url = match loaded.config.listen {
        Listen::Lan => format!("http://localhost:{port}"),
        Listen::Wan => format!("http://{}:{port}", net::local_ipv4()),
    };
    out(kv(t.label_webui, &url));
    out(kv(t.label_auth, &state::auth_text(&t, &loaded.config)));
    state.set_webui(port, url.clone());

    if !from_config {
        info(t.port_auto_note);
    }

    // ---- 4. Start the WebUI HTTP server ----------------------------------------
    let web = Arc::new(WebState::new(
        loaded.path.clone(),
        loaded.config.clone(),
        lang,
        port,
        url.clone(),
        options.dialog,
    ));
    if let Err(err) = server::spawn(listener, web.clone()) {
        state.fatal(format!("HTTP server thread: {err}"));
        return failed(&url, port);
    }
    state::set_web(web.clone());
    server::pump_queues(&web);
    info(t.webui_started);

    // ---- 4.5 Built-in MCP adapter (design doc §12 Phase 6)--------------------------
    // It runs on a **separate port**: the WebUI port is for humans, the MCP port is
    // for AI agents (often on another machine). Port, on/off and listen mode live in
    // config.json and apply immediately, no restart needed.
    let mcp_status = match web.mcp.apply(&loaded.config.mcp, &web, port) {
        Ok(status) => {
            if status.running {
                out(kv(
                    t.label_mcp,
                    &i18n::mcp_status_line(lang, &status.url, crate::mcp::tools().len()),
                ));
                if let Some(note) = &status.note {
                    info(note.clone());
                }
                if web.mcp_wan_without_auth() {
                    warn(t.warn_mcp_wan_no_auth);
                }
            } else {
                info(t.mcp_disabled);
            }
            status
        }
        Err(err) => {
            // A failed MCP must not hold back the WebUI: an occupied port only warns, the daemon keeps serving
            warn(format!("{}:{err}", t.mcp_failed));
            crate::mcp::Status {
                enabled: loaded.config.mcp.enabled,
                running: false,
                port: loaded
                    .config
                    .mcp
                    .requested_port()
                    .unwrap_or(crate::mcp::DEFAULT_PORT),
                listen: loaded.config.mcp.listen,
                url: String::new(),
                error: Some(err),
                note: None,
            }
        }
    };

    // Record the port actually bound this run so thin clients such as the CLI can find the daemon
    // (config.json may hold port 0 = auto-pick, which cannot be looked up).
    write_runtime_file(
        &loaded.path,
        &url,
        port,
        loaded.config.listen,
        &mcp_status,
    );

    // ---- 5. Persistent warning in wan mode -----------------------------------------
    if web.wan_without_auth() {
        warn(t.warn_wan_no_auth);
    }

    // ---- 6. Open the GUI in the system default browser -----------------------------------
    if options.open_browser {
        match opener::open(&url) {
            Ok(()) => info(t.browser_opened),
            Err(err) => {
                warn(t.browser_failed);
                out(format!("  {url}  ({err})"));
            }
        }
    }

    info(t.input_hint);

    Booted {
        url,
        port,
        web: Some(web),
        fatal: false,
    }
}

/// Runtime info of the daemon, used by thin clients (the CLI) to discover the address.
///
/// Stored next to config.json. A hard-killed process leaves the file behind, so clients
/// must be able to fall back to the port in config.json when it stops answering.
pub const RUNTIME_FILE: &str = "runtime.json";

fn write_runtime_file(
    config_path: &std::path::Path,
    url: &str,
    port: u16,
    listen: Listen,
    mcp: &crate::mcp::Status,
) {
    let Some(dir) = config_path.parent() else {
        return;
    };
    let payload = serde_json::json!({
        "url": url,
        "port": port,
        "listen": listen.code(),
        "pid": std::process::id(),
        "configPath": crate::paths::display(config_path),
        "startedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        // The MCP port may be auto-picked too; thin clients (and the MCP bridge) need it as well
        "mcpUrl": if mcp.running { mcp.url.clone() } else { String::new() },
        "mcpPort": if mcp.running { mcp.port } else { 0 },
        "mcpEnabled": mcp.enabled,
    });
    let Ok(text) = serde_json::to_string_pretty(&payload) else {
        return;
    };
    let text = text + "\n";

    // 1) Next to config.json (portable: copy the directory and it travels along)
    let _ = std::fs::write(dir.join(RUNTIME_FILE), &text);

    // 2) A second copy in the **per-user shared directory** (env var shim).
    //    Once `cgw` is on PATH, its current_exe() is the PATH location, not the daemon
    //    install dir -- the two meet through this file. A failed write is harmless.
    if let Some(home) = crate::paths::ensure_cgw_home() {
        if home != dir {
            let _ = std::fs::write(home.join(RUNTIME_FILE), &text);
        }
    }
}

fn failed(url: &str, port: u16) -> Booted {
    Booted {
        url: url.to_string(),
        port,
        web: None,
        fatal: true,
    }
}

pub fn describe_load_error(lang: Lang, err: config::LoadError) -> String {
    match &err {
        config::LoadError::NoConfigDir => i18n::err_no_config_dir(lang),
        config::LoadError::Read { path, err } => i18n::err_config_read(lang, path, &err.to_string()),
        config::LoadError::Parse { path, err } => i18n::err_config_parse(lang, path, err),
        config::LoadError::Write { path, err } => {
            i18n::err_config_write(lang, path, &err.to_string())
        }
    }
}

// ---------------------------------------------------------------- Port

pub enum BindError {
    InUse { port: u16 },
    Other { addr: String, err: std::io::Error },
}

/// Bind the address named by `listen`, returning (listener, port, port came from config).
/// Bind first, hand the listener to axum: avoids a race between "check free" and "listen".
pub fn bind_listener(config: &config::Config) -> Result<(TcpListener, u16, bool), BindError> {
    let ip = config.listen.bind_ip();
    match config.requested_port() {
        Some(port) => {
            let addr = SocketAddr::from((ip, port));
            match TcpListener::bind(addr) {
                Ok(listener) => Ok((listener, port, true)),
                Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
                    Err(BindError::InUse { port })
                }
                Err(err) => Err(BindError::Other {
                    addr: addr.to_string(),
                    err,
                }),
            }
        }
        None => {
            let addr = SocketAddr::from((ip, 0));
            let listener = TcpListener::bind(addr).map_err(|err| BindError::Other {
                addr: addr.to_string(),
                err,
            })?;
            let port = listener
                .local_addr()
                .map_err(|err| BindError::Other {
                    addr: addr.to_string(),
                    err,
                })?
                .port();
            Ok((listener, port, false))
        }
    }
}
