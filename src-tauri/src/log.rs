//! In-process shared state + log bus.
//!
//! Logs have two outlets: they are appended to a ring buffer that a CLI window
//! fetches in one go when it opens (`cli::get_logs`), and pushed through
//! [`LogSink`] to already-open windows. The frontend dedupes by `seq`, so
//! "fetch snapshot + subscribe" neither duplicates nor drops lines.
//!
//! **This module does not know Tauri**: pushing is the shell-side `LogSink`
//! implementation's job, so the core (library crate) needs no webview and the
//! headless daemon runs the same code.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::i18n::Lang;

pub const EVENT_LOG: &str = "cgw://log";

/// Main window label, which is also the name of the main log scope.
pub const MAIN_SCOPE: &str = "main";

/// Max entries kept per log scope.
const MAX_ENTRIES: usize = 4000;

/// Log levels; also the frontend CSS class names.
pub const INFO: &str = "info";
pub const WARN: &str = "warn";
pub const ERROR: &str = "error";
pub const INPUT: &str = "input";
pub const OUT: &str = "out";

/// Source marker used when no `X-Client` is present.
pub const SOURCE_DAEMON: &str = "daemon";

/// Allowed source markers (design doc §10 calls it an **open enum**, leaving room
/// for `mcp`).
///
/// Used only to reduce a request header to a safe short tag; unknown values fall
/// back to `daemon`, so no arbitrary string lands in the logs as a label.
pub fn normalize_source(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 24 {
        return SOURCE_DAEMON.to_string();
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return SOURCE_DAEMON.to_string();
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "webui" => "webui".to_string(),
        "cli" => "cli".to_string(),
        "mcp" => "mcp".to_string(),
        "daemon" => SOURCE_DAEMON.to_string(),
        // open enum: new sources need no change here
        other => other.to_string(),
    }
}

#[derive(Clone, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub level: &'static str,
    pub text: String,
    pub ts_ms: u64,
    /// Source marker (design doc §10): `webui` / `cli` / `mcp` from the
    /// `X-Client` request header, or `daemon` for the daemon's own lines.
    pub source: String,
}

#[derive(Clone, Serialize)]
pub struct Status {
    pub version: String,
    pub lang: String,
    pub port: Option<u16>,
    pub url: Option<String>,
    pub config_path: Option<String>,
    pub fatal: bool,
    /// Log scope of this window (equals the window label).
    pub scope: String,
    /// Instance windows only: the matching client.
    pub instance: Option<serde_json::Value>,
}

/// Log push outlet. The shell (windowed Tauri app) implements it to send lines to
/// a window; the headless daemon does not, so lines just stay in the buffer for
/// HTTP reads.
pub trait LogSink: Send + Sync {
    fn emit(&self, scope: &str, entry: &LogEntry);
}

/// One log scope (one per window).
#[derive(Default)]
struct Scope {
    logs: Mutex<Vec<LogEntry>>,
    seq: AtomicU64,
}

pub struct Core {
    main: Arc<Scope>,
    /// Log scopes of instance windows, keyed by window label `instance-<client-id>`.
    instances: Mutex<HashMap<String, Arc<Scope>>>,
    /// Log scope -> source marker (design doc §10).
    ///
    /// Bound to the **scope** rather than passed down: build logs are written deep
    /// inside `build.rs` / `fileops.rs` / `sync.rs`, and threading `source` through
    /// would change many signatures; stored on the scope, every line those modules
    /// write via `push_scoped` automatically carries the right source.
    sources: Mutex<HashMap<String, String>>,
    sink: OnceLock<Arc<dyn LogSink>>,
    /// File outlet (design doc §10). Installed by headless and windowed builds
    /// alike; skipped when no directory is available.
    file: OnceLock<Arc<crate::logfile::LogFile>>,
    lang: Mutex<Lang>,
    lang_note: Mutex<String>,
    config_path: Mutex<Option<PathBuf>>,
    port: Mutex<Option<u16>>,
    url: Mutex<Option<String>>,
    fatal: AtomicBool,
}

static CORE: OnceLock<Core> = OnceLock::new();

pub fn core() -> &'static Core {
    CORE.get_or_init(|| Core {
        main: Arc::new(Scope {
            logs: Mutex::new(Vec::new()),
            seq: AtomicU64::new(1),
        }),
        instances: Mutex::new(HashMap::new()),
        sources: Mutex::new(HashMap::new()),
        sink: OnceLock::new(),
        file: OnceLock::new(),
        lang: Mutex::new(Lang::from_system()),
        lang_note: Mutex::new(String::new()),
        config_path: Mutex::new(None),
        port: Mutex::new(None),
        url: Mutex::new(None),
        fatal: AtomicBool::new(false),
    })
}

/// Label of an instance window.
pub fn instance_label(client_id: &str) -> String {
    format!("instance-{client_id}")
}

impl Core {
    /// Attach the log outlet. The windowed shell calls this once at startup;
    /// headless daemon does not.
    pub fn attach(&self, sink: Arc<dyn LogSink>) {
        let _ = self.sink.set(sink);
    }

    /// Log outlet (present only once the shell attached it).
    pub fn sink(&self) -> Option<Arc<dyn LogSink>> {
        self.sink.get().cloned()
    }

    /// Attach the file outlet. `app::bootstrap` calls this once the config directory
    /// is known.
    pub fn attach_file(&self, file: Arc<crate::logfile::LogFile>) {
        let _ = self.file.set(file);
    }

    /// File outlet (present only once attached).
    pub fn file(&self) -> Option<Arc<crate::logfile::LogFile>> {
        self.file.get().cloned()
    }

    pub fn lang(&self) -> Lang {
        *crate::lock::lock(&self.lang)
    }

    pub fn set_lang(&self, lang: Lang, note: String) {
        *crate::lock::lock(&self.lang) = lang;
        *crate::lock::lock(&self.lang_note) = note;
    }

    /// Where the language came from, e.g. `auto (zh-CN)` / `en-US (config)`.
    pub fn lang_note(&self) -> String {
        crate::lock::lock(&self.lang_note).clone()
    }

    pub fn set_config_path(&self, path: PathBuf) {
        *crate::lock::lock(&self.config_path) = Some(path);
    }

    pub fn config_path(&self) -> Option<PathBuf> {
        crate::lock::lock(&self.config_path).clone()
    }

    pub fn set_webui(&self, port: u16, url: String) {
        *crate::lock::lock(&self.port) = Some(port);
        *crate::lock::lock(&self.url) = Some(url);
    }

    pub fn port(&self) -> Option<u16> {
        *crate::lock::lock(&self.port)
    }

    pub fn url(&self) -> Option<String> {
        crate::lock::lock(&self.url).clone()
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal.load(Ordering::Relaxed)
    }

    /// Get a log scope, creating it if needed.
    fn scope(&self, name: &str) -> Arc<Scope> {
        if name == MAIN_SCOPE {
            return self.main.clone();
        }
        let mut map = crate::lock::lock(&self.instances);
        map.entry(name.to_string())
            .or_insert_with(|| Arc::new(Scope::default()))
            .clone()
    }

    pub fn push(&self, level: &'static str, text: impl Into<String>) {
        self.push_scoped(MAIN_SCOPE, level, text);
    }

    /// Write one line into a scope; the scope name is the window label
    /// (`main` / `instance-<id>`).
    ///
    /// The source marker comes from the scope binding (see [`Core::bind_source`]),
    /// defaulting to `daemon`.
    pub fn push_scoped(&self, scope: &str, level: &'static str, text: impl Into<String>) {
        let source = self.source_of(scope);
        self.push_sourced(scope, level, &source, text);
    }

    /// Bind a log scope to a source. Lines in that scope then carry this marker.
    pub fn bind_source(&self, scope: &str, source: &str) {
        crate::lock::lock(&self.sources)
            .insert(scope.to_string(), normalize_source(source));
    }

    /// Bind several scopes at once (a build job writes both the instance page and
    /// the job page).
    pub fn bind_sources(&self, scopes: &[String], source: &str) {
        let source = normalize_source(source);
        let mut map = crate::lock::lock(&self.sources);
        for scope in scopes {
            map.insert(scope.clone(), source.clone());
        }
    }

    fn source_of(&self, scope: &str) -> String {
        crate::lock::lock(&self.sources)
            .get(scope)
            .cloned()
            .unwrap_or_else(|| SOURCE_DAEMON.to_string())
    }

    /// Same as above, but carrying the source marker (design doc §10 `X-Client`).
    pub fn push_sourced(
        &self,
        scope: &str,
        level: &'static str,
        source: &str,
        text: impl Into<String>,
    ) {
        let data = self.scope(scope);
        let entry = LogEntry {
            seq: data.seq.fetch_add(1, Ordering::Relaxed),
            level,
            text: text.into(),
            ts_ms: now_ms(),
            source: normalize_source(source),
        };

        // In debug builds also print to stdout, so `cargo run` shows it directly
        #[cfg(debug_assertions)]
        {
            if level == INPUT {
                println!("[{scope}] > {}", entry.text);
            } else {
                println!("[{scope}] {}", entry.text);
            }
        }

        if let Some(sink) = self.sink.get() {
            sink.emit(scope, &entry);
        }
        if let Some(file) = self.file.get() {
            file.append(scope, &entry);
        }

        let mut logs = crate::lock::lock(&data.logs);
        logs.push(entry);
        if logs.len() > MAX_ENTRIES {
            let excess = logs.len() - MAX_ENTRIES;
            logs.drain(0..excess);
        }
    }

    /// Startup fatal error: log it + set fatal, but **do not exit the process**, so
    /// the user can read the cause in the CLI window (closing the window ends it).
    pub fn fatal(&self, text: impl Into<String>) {
        self.fatal.store(true, Ordering::Relaxed);
        self.push(ERROR, text);
        let t = crate::i18n::text(self.lang());
        self.push(INFO, t.fatal_hint);
    }

    pub fn snapshot(&self, scope: &str) -> Vec<LogEntry> {
        crate::lock::lock(&self.scope(scope).logs).clone()
    }

    pub fn clear(&self, scope: &str) {
        crate::lock::lock(&self.scope(scope).logs).clear();
    }

    pub fn status(&self, scope: &str) -> Status {
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            lang: self.lang().code().to_string(),
            port: self.port(),
            url: self.url(),
            config_path: self.config_path().map(|p| p.display().to_string()),
            fatal: self.is_fatal(),
            scope: scope.to_string(),
            instance: crate::state::client_for_scope(scope).map(|c| c.to_json()),
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Convenience entry point (main log scope).
pub fn log(level: &'static str, text: impl Into<String>) {
    core().push(level, text);
}

/// Convenience entry point (given window / log scope).
pub fn log_scoped(scope: &str, level: &'static str, text: impl Into<String>) {
    core().push_scoped(scope, level, text);
}

/// Convenience entry point (log scope + source marker, design doc §10 `X-Client`).
pub fn log_sourced(
    scope: &str,
    level: &'static str,
    source: &str,
    text: impl Into<String>,
) {
    core().push_sourced(scope, level, source, text);
}

/// Bind one log scope to a source (called once when a build job starts).
pub fn bind_source(scope: &str, source: &str) {
    core().bind_source(scope, source);
}

/// Bind several scopes at once.
pub fn bind_sources(scopes: &[String], source: &str) {
    core().bind_sources(scopes, source);
}

/// Write to several log scopes at once: build output goes to both the instance page
/// and the job page (design doc §10.1).
pub fn log_multi(scopes: &[String], level: &'static str, text: impl Into<String>) {
    let text = text.into();
    for scope in scopes {
        core().push_scoped(scope, level, text.clone());
    }
}

/// Write to several log scopes at once, carrying the source marker.
pub fn log_multi_sourced(
    scopes: &[String],
    level: &'static str,
    source: &str,
    text: impl Into<String>,
) {
    let text = text.into();
    for scope in scopes {
        core().push_sourced(scope, level, source, text.clone());
    }
}
