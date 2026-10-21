//! `cgw` command implementations: the daemon's thin HTTP client (design doc §3, CLI layer).
//!
//! **It lives in the library, not the binary**, because one implementation feeds three
//! frontends: the standalone CLI (`compilegateway-cli`, PATH-installable), the **CLI
//! window** (the app itself, `src/cli.rs`, whose own command set is only the help /
//! status / diag window actions and which forwards everything else here), and the MCP
//! `cgw://cli` resource (the command table, see `crate::help`).
//!
//! Output goes through [`write_line`]: stdout for the standalone CLI, the log pane for the
//! window, so the two share the exact wording and not just the semantics.
//!
//! Address resolution order: `--url` / `$CGW_URL` → the daemon's `runtime.json` → the port
//! in `config.json`. (A port of 0 there means auto-picked, so only `runtime.json` knows.)
//!
//! Three things for scripts / AI agents:
//!   * **stable exit codes** (the `EXIT_*` group below) - judge "is the project busy" by
//!     exit code, never by grepping the wording;
//!   * **`--json` makes success and failure both structured**, with the daemon's `error`
//!     object as the error body;
//!   * **`--wait` follows an async job to the end**, so callers need not poll, and a
//!     failure exits with `EXIT_JOB_FAILED`.

use std::collections::HashMap;
use std::process::ExitCode;

use crate::app::{APP_NAME, RUNTIME_FILE, VERSION};
use crate::httpclient::{self, Response};
use crate::setcmd;
use serde_json::json;

// ------------------------------------------------------------------ exit codes
//
// A **stable contract** for scripts and AI agents. Changing this changes the interface.

/// Success.
pub const EXIT_OK: u8 = 0;
/// Bad argument / usage (wrong command, missing argument, malformed JSON).
pub const EXIT_USAGE: u8 = 1;
/// Cannot reach the daemon, or the daemon is not running.
pub const EXIT_TRANSPORT: u8 = 2;
/// The daemon returned an error (4xx / 5xx) that is neither busy nor not-found.
pub const EXIT_API: u8 = 3;
/// Per-project lock: a job is already running for this project (HTTP 409 `projectBusy`).
pub const EXIT_BUSY: u8 = 4;
/// The job `--wait` was following ended up failing.
pub const EXIT_JOB_FAILED: u8 = 5;
/// The requested thing does not exist (HTTP 404).
pub const EXIT_NOT_FOUND: u8 = 6;
/// Login required but missing, or wrong credentials (HTTP 401).
pub const EXIT_UNAUTHORIZED: u8 = 7;

/// CLI error: a stable exit code plus the daemon's structured error, when there is one.
#[derive(Debug)]
pub struct CliError {
    /// Stable exit code (the `EXIT_*` group above).
    pub code: u8,
    /// One human-readable line.
    pub message: String,
    /// Raw daemon `error` object, emitted as-is under `--json`.
    pub api: Option<serde_json::Value>,
}

impl CliError {
    fn new(code: u8, message: impl Into<String>) -> CliError {
        CliError {
            code,
            message: message.into(),
            api: None,
        }
    }

    fn usage(message: impl Into<String>) -> CliError {
        CliError::new(EXIT_USAGE, message)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> CliError {
        CliError::new(EXIT_USAGE, message)
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> CliError {
        CliError::new(EXIT_USAGE, message)
    }
}

// ------------------------------------------------------------------ output

use std::cell::RefCell;

/// A whole-line output sink: the window installs one, the standalone CLI uses `None`.
pub type LineSink = Box<dyn FnMut(&str)>;

thread_local! {
    /// Where output goes. `None` = plain stdout (the standalone CLI default); the window
    /// installs one that writes into the log pane.
    static SINK: RefCell<Option<LineSink>> = const { RefCell::new(None) };
}

/// Replace the output sink (used by the window).
pub fn set_sink(sink: Option<LineSink>) {
    SINK.with(|slot| *slot.borrow_mut() = sink);
}

/// Install a sink for the lifetime of this guard and restore the previous one on drop (S14).
///
/// `set_sink(..)` / `set_sink(None)` as a manual pair leaks the sink if the code in between
/// panics. A guard resets it on every exit path, including unwinding.
pub struct SinkGuard {
    previous: Option<LineSink>,
}

impl SinkGuard {
    /// Install `sink`, holding onto whatever was installed before.
    pub fn install(sink: LineSink) -> Self {
        let previous = SINK.with(|slot| slot.borrow_mut().take());
        set_sink(Some(sink));
        Self { previous }
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        // Put the previous sink back (usually `None`).
        set_sink(self.previous.take());
    }
}

/// Emit one line (with newline). Every command goes through here.
pub fn write_line(text: &str) {
    SINK.with(|slot| {
        let mut borrow = slot.borrow_mut();
        match borrow.as_mut() {
            Some(sink) => sink(text),
            None => println!("{text}"),
        }
    });
}

/// Emit text **without a newline** (stdout export, shell prompt).
pub fn write_raw(text: &str) {
    SINK.with(|slot| {
        let mut borrow = slot.borrow_mut();
        match borrow.as_mut() {
            // A whole-line sink (the window log) gets whole lines, so no partial line jams the buffer
            Some(sink) => {
                if !text.trim().is_empty() {
                    sink(text);
                }
            }
            None => {
                use std::io::Write;
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
        }
    });
}

/// Emit one line - every command implementation uses this macro.
macro_rules! say {
    () => { $crate::client::write_line("") };
    ($($arg:tt)*) => { $crate::client::write_line(&format!($($arg)*)) };
}

/// Emit without a newline (only stdout export and the shell prompt need this).
macro_rules! say_raw {
    ($($arg:tt)*) => { $crate::client::write_raw(&format!($($arg)*)) };
}

/// Standalone CLI entry point (all `src/bin/cli.rs` does is call this).
pub fn main_entry() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json = raw.iter().any(|token| token == "--json");

    match run(&raw) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            // Under `--json` errors are structured too (to stdout), so callers need not parse stderr prose
            if json {
                let payload = json!({
                    "ok": false,
                    "exitCode": error.code,
                    "error": error.api.unwrap_or_else(|| json!({
                        "code": "cliError",
                        "message": error.message,
                    })),
                });
                say!("{payload}");
            } else {
                say_raw!("{}\n", error.message);
            }
            ExitCode::from(error.code)
        }
    }
}

/// Entry point for the **window**: run one command line as the standalone CLI would.
///
/// The only difference is that output goes to the log pane (via [`set_sink`]) and stdin is
/// never touched (the window has no `-shell` use - the window *is* that shell).
pub fn run_line(raw: &[String]) -> Result<u8, CliError> {
    // stdin is a TTY in the window, but "bare prompt = interactive shell" means nothing there
    if raw.is_empty() {
        return Ok(EXIT_OK);
    }
    if raw[0] == "shell" || raw.iter().any(|token| token == "--shell" || token == "-shell") {
        return Ok(EXIT_OK);
    }
    run(raw)
}

#[cfg(test)]
mod forward_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// The window path: output must reach the sink rather than stdout, or the window shows nothing.
    /// `help` needs no daemon, so this case does not start a service.
    #[test]
    fn run_line_writes_into_the_sink() {
        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let target = captured.clone();
        set_sink(Some(Box::new(move |line: &str| {
            crate::lock::lock(&target).push(line.to_string());
        })));
        let code = run_line(&["help".to_string(), "--json".to_string()]).unwrap();
        set_sink(None);

        assert_eq!(code, EXIT_OK);
        let lines = crate::lock::lock(&captured).clone();
        assert_eq!(lines.len(), 1, "expected exactly one JSON line: {lines:?}");
        assert!(lines[0].starts_with('{'), "{}", lines[0]);
        assert!(lines[0].contains("\"commands\""));
    }

    /// The window asks whether the standalone CLI knows a command before forwarding it.
    #[test]
    fn is_known_follows_the_command_table() {
        let known = |line: &str| is_known(&crate::setcmd::tokenize(line));
        assert!(known("status"));
        assert!(known("set port --mcp 3994"));
        assert!(known("manifests import x.json"));
        assert!(known("manifests export xcode --save y.json"));
        assert!(known("clients"));
        assert!(known("jobs retry abc --wait"));
        assert!(!known("nope"));
        // A known top level with a wrong subcommand is **forwarded** so the CLI reports the
        // unknown subcommand itself, instead of the window saying a generic "unknown command".
        assert!(known("manifests nope"));
    }

    /// The window must never enter "bare prompt = interactive shell" (the window is already that shell).
    #[test]
    fn run_line_ignores_the_interactive_shell_form() {
        assert_eq!(run_line(&[]).unwrap(), EXIT_OK);
        assert_eq!(run_line(&["shell".to_string()]).unwrap(), EXIT_OK);
        assert_eq!(run_line(&["-shell".to_string()]).unwrap(), EXIT_OK);
    }
}

/// Whether **the standalone CLI knows this command** (the window decides forward vs. report unknown).
///
/// The criterion is the command table ([`crate::help`]), so the window and `cgw help` cannot disagree.
pub fn is_known(raw: &[String]) -> bool {
    let Some(command) = raw.iter().find(|token| !token.starts_with('-')) else {
        // Switches only (e.g. `--version`): leave it to the CLI itself
        return raw.iter().any(|token| token == "--version" || token == "-V");
    };
    crate::help::find_command(command).is_some()
        || crate::help::top_level_names().iter().any(|name| name == command)
}

/// No arguments at all (a bare `cgw`)?
///
/// The test for "typing `cgw` and pressing enter enters the interactive environment": any
/// positional or switch counts as work to do, so only a truly bare call opens the shell -
/// otherwise `cgw --url X` could trap someone in a session.
fn is_bare_invocation(args: &Args) -> bool {
    args.raw.is_empty() || (args.positional.is_empty() && args.flags.is_empty() && args.options.is_empty())
}

fn run(raw: &[String]) -> Result<u8, CliError> {
    use std::io::IsTerminal;
    let args = Args::parse(raw);
    if args.has("version") || args.has("V") {
        say!("{APP_NAME} {VERSION} (cli)");
        return Ok(EXIT_OK);
    }
    // `cgw -shell` / `--shell` / `shell`: a CLI-style interactive shell in the current terminal
    // (prompt + scrolling log, right in PowerShell). Checked before `positional.is_empty()`.
    if args.has("shell") || args.positional.first().map(String::as_str) == Some("shell") {
        return shell(&args);
    }

    // **A bare `cgw` plus enter enters the interactive environment** (like `adb shell`); in
    // pipes/CI stdin is absent and it would just hang, so there a bare `cgw` prints help.
    if is_bare_invocation(&args) && std::io::stdin().is_terminal() {
        return shell(&args);
    }

    // All four spellings `help` / `?` / `--help` / `-h` work. **Must precede** the
    // `positional.is_empty()` branch, or `--help --json` would print help instead of JSON.
    let wants_help = args.has("help")
        || args.has("h")
        || matches!(args.positional.first().map(String::as_str), Some("help" | "?"));
    if wants_help {
        // `cgw help [command] [subcommand]` and `cgw <command> -h` take the same drill-down path;
        // `--json` gives agents a machine-readable command surface, `--webui` prints the WebUI table.
        return print_help_for(&args);
    }

    if args.positional.is_empty() {
        // Switches only (e.g. `cgw --json`) are treated as help too
        if args.has("json") {
            say!("{}", crate::help::json());
            return Ok(EXIT_OK);
        }
        if args.has("webui") {
            say!("{}", crate::help::webui_table());
            return Ok(EXIT_OK);
        }
        say!("{}", crate::help::tree());
        return Ok(EXIT_OK);
    }

    // `env --script` needs no daemon address, so intercept it before address resolution
    if args.positional[0] == "env" && args.has("script") {
        say!("{}", crate::env::probe_script());
        return Ok(EXIT_OK);
    }


    let server = Server::resolve(&args)?;
    dispatch(&server, &args)
}

/// Dispatch one command to its implementation; every terminal-shell line lands here too,
/// so both command sets stay identical.
fn dispatch(server: &Server, args: &Args) -> Result<u8, CliError> {
    let command = args.positional[0].as_str();
    let rest = &args.positional[1..];
    let json = args.has("json");

    match command {
        "status" => cmd_status(&server, json),
        "doctor" => cmd_doctor(&server, json),
        "setup" => cmd_setup(&args, json),
        "set" => cmd_set(&server, &args, rest, json),
        "login" => cmd_login(&server, &args, rest, json),
        "logout" => cmd_logout(&server, json),
        "clients" => cmd_clients(&server, &args, rest, json),
        "basedir" => cmd_basedir(&server, &args, rest, json),
        "env" => cmd_env(&server, &args, rest, json),
        "projects" => cmd_projects(&server, &args, rest, json),
        "browse" => cmd_browse(&server, rest, json),
        "tree" => cmd_tree(&server, &args, rest, json),
        "cat" => cmd_cat(&server, &args, rest, json),
        "tasks" => cmd_tasks(&server, &args, rest, json),
        "task" => cmd_task(&server, rest, json),
        "batches" => cmd_batches(&server, rest, json),
        "batch" => cmd_batch(&server, &args, rest, json),
        "run" | "build" => cmd_build(&server, &args, rest, json),
        "push" => cmd_push(&server, &args, rest, json),
        "pull" => cmd_pull(&server, &args, rest, json),
        "export" => cmd_export(&server, &args, rest, json),
        "exports" => cmd_exports(&server, &args, rest, json),
        "jobs" => cmd_jobs(&server, &args, rest, json),
        "job" => cmd_job(&server, rest, json),
        "manifests" => cmd_manifests(&server, &args, rest, json),
        "workflows" => cmd_workflows(server, args, rest, json),
        "schema" => match rest.first().map(String::as_str) {
            Some(name) => cmd_schema(&server, rest, name),
            None => cmd_schema_list(&server),
        },
        "logs" => cmd_logs(&server, &args, rest, json),
        "mcp" => cmd_mcp(&server, &args, rest, json),
        "config" => cmd_config(&server, &args, rest, json),
        "api" => cmd_api(&server, &args, rest, json),
        other => Err(CliError::usage(format!(
            "未知命令:{other}(用 `cgw help` 看用法,`cgw help --json` 拿机器可读的命令面)"
        ))),
    }
}

// ------------------------------------------------------------------ arguments

/// Switches that take no value. Options listed here **never** swallow the next token.
///
/// Otherwise `--json help` parses as "option `json` has value `help`" and then produces baffling
/// "no daemon address" errors. Tokenization ambiguity is a top CLI bug class, so pin it with a list.
const BOOLEAN_FLAGS: &[&str] = &[
    "json",
    "wait",
    "clean",
    "cached",
    "script",
    "shell",
    "help",
    "h",
    "V",
    "version",
    "from-start",
    "follow",
    "no-verify",
    "force",
    // The queueing and workflow-template switch groups (design doc §7.6.6 / §14.3)
    "enqueue",
    "apply",
    "overwrite",
    "run",
];

#[derive(Default)]
struct Args {
    positional: Vec<String>,
    flags: Vec<String>,
    options: HashMap<String, String>,
    /// Raw token list. Every terminal-shell line is a fresh [`Args::parse`], so re-reading the
    /// process args would be wrong: take raw values from here.
    raw: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Args {
        let mut args = Args {
            raw: raw.to_vec(),
            ..Args::default()
        };
        let mut index = 0;
        while index < raw.len() {
            let token = &raw[index];
            if let Some(rest) = token.strip_prefix("--") {
                if let Some((key, value)) = rest.split_once('=') {
                    args.options.insert(key.to_string(), value.to_string());
                } else if !BOOLEAN_FLAGS.contains(&rest)
                    && index + 1 < raw.len()
                    && !raw[index + 1].starts_with('-')
                {
                    // Only options that truly need a value swallow the next token; boolean
                    // switches never do, or `--json help` would read as "json = help".
                    args.options
                        .insert(rest.to_string(), raw[index + 1].clone());
                    index += 1;
                } else {
                    args.flags.push(rest.to_string());
                }
            } else if let Some(short) = token.strip_prefix('-') {
                if short.len() == 1 {
                    args.flags.push(short.to_string());
                } else if short.len() >= 2
                    && short.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                {
                    // Word-shaped single dashes count as options too (`-shell` / `-wait` / `-json`);
                    // pure numbers (e.g. `-5`) stay positional so negative values are not eaten.
                    args.flags.push(short.to_string());
                } else {
                    args.positional.push(token.clone());
                }
            } else {
                args.positional.push(token.clone());
            }
            index += 1;
        }
        args
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }
    fn get(&self, name: &str) -> Option<&str> {
        self.options.get(name).map(String::as_str)
    }

    /// Fetch `--flag value` from the raw tokens (some options never reach `options`, so read on demand).
    fn flag_value(&self, name: &str) -> Option<String> {        self.raw
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
    }

    /// Fetch a JSON argument: `--body '<json>'` / `--file <path>` / `--file -` (stdin), or a
    /// positional argument that starts with `{`.
    ///
    /// Agents would rather pipe JSON in than fight shell quoting.
    fn json_argument(&self, from: usize) -> Result<serde_json::Value, CliError> {
        if let Some(candidate) = self.positional.get(from) {
            if candidate.trim_start().starts_with('{') {
                return serde_json::from_str(candidate)
                    .map_err(|err| CliError::usage(format!("参数不是合法 JSON:{err}")));
            }
        }

        let text = match (self.flag_value("--body"), self.flag_value("--file")) {
            (Some(body), _) => body,
            (None, Some(file)) if file == "-" => {
                use std::io::Read;
                let mut buffer = String::new();
                std::io::stdin()
                    .read_to_string(&mut buffer)
                    .map_err(|err| CliError::usage(format!("读标准输入失败:{err}")))?;
                buffer
            }
            (None, Some(file)) => std::fs::read_to_string(&file)
                .map_err(|err| CliError::usage(format!("读 {file} 失败:{err}")))?,
            (None, None) => {
                return Err(CliError::usage(
                    "需要一段 JSON:用 --body '<json>'、--file <路径>,或 --file - 从标准输入读",
                ))
            }
        };

        serde_json::from_str(text.trim())
            .map_err(|err| CliError::usage(format!("JSON 不合法:{err}")))
    }

    /// Timeout seconds for `--wait` (default 3600).
    fn wait_timeout(&self) -> u64 {
        self.get("timeout")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(3600)
    }
}

/// Read a "file or stdin" argument: `-` means read from stdin.
///
/// Exists for pipeline use like `ssh-keyscan host | cgw clients hostkey <id> --import -`.
fn read_text_argument(source: &str) -> Result<String, CliError> {
    if source == "-" {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|err| CliError::usage(format!("读标准输入失败:{err}")))?;
        return Ok(buffer);
    }
    std::fs::read_to_string(source).map_err(|err| CliError::usage(format!("读 {source} 失败:{err}")))
}

// ------------------------------------------------------------------ address and session

/// Daemon address plus session cookie.
///
/// Two session sources, in priority order: env credentials (logged in per call, leaving no
/// trace) then the `cgw-session.json` saved by `cgw login`. Loopback access needs no session.
struct Server {
    host: String,
    /// Session cookie. `Mutex` lets `call(&self)` log in once when it hits a 401; nearly every
    /// command takes `&Server`, so switching them all to `&mut` for login is not worth it.
    session: std::sync::Mutex<Option<String>>,
    /// Auto-login already attempted (stops retrying forever after a 401).
    login_tried: std::sync::atomic::AtomicBool,
}

/// Where `cgw login` stores the session cookie (same directory as `runtime.json`).
const SESSION_FILE: &str = "cgw-session.json";

/// Available JSON Schema names (design doc §2). The source of truth is the daemon's
/// `jsonschema::NAMES`; this copy only rejects misspelled names before the request.
const SCHEMA_NAMES: [&str; 6] = [
    "manifest",
    "task",
    "batch",
    "client",
    "project",
    "workflow",
];

impl Server {
    fn resolve(args: &Args) -> Result<Server, CliError> {
        let url = locate_url(args)
            .map(|(url, _)| url)
            .ok_or_else(|| {
                CliError::new(
                    EXIT_TRANSPORT,
                    format!(
                        "找不到 daemon 地址。三种办法:\n\
                         \x20 1. --url http://host:port 或设环境变量 CGW_URL;\n\
                         \x20 2. 确认 daemon 正在运行(它会在 {} 写一份 runtime.json);\n\
                         \x20 3. `cgw env` 看当前的解析链和怎么把 cgw 放进 PATH。",
                        crate::paths::cgw_home()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "~/.compilegateway".to_string())
                    ),
                )
            })?;

        let host = url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string();
        // An explicit `--session` cookie wins; otherwise read the locally stored one.
        // Env credentials are not handled here: that is the "log in on 401" path in `call`.
        let session = args
            .get("session")
            .map(str::to_string)
            .or_else(|| load_session(&host));
        Ok(Server {
            host,
            session: std::sync::Mutex::new(session),
            login_tried: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn session(&self) -> Option<String> {
        crate::lock::lock(&self.session).clone()
    }

    fn set_session(&self, cookie: Option<String>) {
        *crate::lock::lock(&self.session) = cookie;
    }

    fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<Response, CliError> {
        let request = |cookie: Option<&str>| {
            httpclient::request_full(
                &self.host,
                method,
                path,
                body,
                httpclient::DEFAULT_CLIENT,
                cookie,
            )
            .map_err(|err| CliError::new(EXIT_TRANSPORT, err))
        };

        let cookie = self.session();
        let response = request(cookie.as_deref())?;
        if response.status != 401 {
            return Ok(response);
        }

        // Only on a 401, log in once with env credentials; loopback access never gets here.
        if self
            .login_tried
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(response);
        }
        match self.login_with_env()? {
            Some(cookie) => {
                self.set_session(Some(cookie.clone()));
                request(Some(&cookie))
            }
            None => Ok(response),
        }
    }

    /// Trade `$CGW_USERNAME` / `$CGW_PASSWORD` for a session. `None` means neither variable is set.
    fn login_with_env(&self) -> Result<Option<String>, CliError> {
        let (Ok(username), Ok(password)) =
            (std::env::var("CGW_USERNAME"), std::env::var("CGW_PASSWORD"))
        else {
            return Ok(None);
        };
        let payload = json!({ "username": username, "password": password }).to_string();
        let response = httpclient::request_full(
            &self.host,
            "POST",
            "/api/login",
            Some(&payload),
            httpclient::DEFAULT_CLIENT,
            None,
        )
        .map_err(|err| CliError::new(EXIT_TRANSPORT, err))?;
        if !response.ok() {
            return Err(api_error(&response));
        }
        Ok(response
            .header("set-cookie")
            .and_then(|value| value.split(';').next())
            .map(str::to_string))
    }

    /// A JSON call: a non-2xx response raises the daemon's error (with the matching exit code).
    fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<serde_json::Value, CliError> {
        let response = self.call(method, path, body)?;
        let parsed: serde_json::Value = serde_json::from_str(response.body.trim()).map_err(|err| {
            CliError::new(
                EXIT_API,
                format!(
                    "daemon 返回的不是 JSON({err}):{}",
                    response.body.trim()
                ),
            )
        })?;
        if !response.ok() {
            return Err(api_error_with_body(&response, parsed));
        }
        Ok(parsed)
    }

    /// Get a jobId and, with `--wait`, follow it to the end.
    fn start_and_maybe_wait(
        &self,
        args: &Args,
        path: &str,
        body: Option<&str>,
        json: bool,
    ) -> Result<u8, CliError> {
        let started = self.json("POST", path, body)?;
        let job_id = started["jobId"].as_str().unwrap_or("").to_string();
        if job_id.is_empty() {
            return Err(CliError::new(EXIT_API, "daemon 没有返回 jobId"));
        }

        if !args.has("wait") {
            if json {
                say!("{}", serde_json::to_string_pretty(&started).unwrap_or_default());
            } else {
                say!("jobId={job_id}");
                say!("用 `cgw job {job_id}` 看状态;想让它自己等到结束就加 --wait。");
            }
            return Ok(EXIT_OK);
        }

        let timeout = args.wait_timeout();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
        let mut last = String::new();
        loop {
            if std::time::Instant::now() > deadline {
                return Err(CliError::new(
                    EXIT_JOB_FAILED,
                    format!("等 job {job_id} 超过 {timeout} 秒还没结束"),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(800));
            let job = self.json("GET", &format!("/api/jobs/{}", enc(&job_id)), None)?;
            let state = job["state"].as_str().unwrap_or("").to_string();
            if !json {
                let message = job["message"].as_str().unwrap_or("");
                if message != last {
                    eprintln!("[{state}] {message}");
                    last = message.to_string();
                }
            }
            match state.as_str() {
                "running" => continue,
                "ok" => {
                    if json {
                        say!("{}", serde_json::to_string_pretty(&job).unwrap_or_default());
                    } else {
                        say!("{}", job["message"].as_str().unwrap_or("完成"));
                        if let Some(retry) = job["retryFromStep"].as_u64() {
                            say!("注意:还有步骤没成功,从第 {} 步重试用 `cgw jobs retry {job_id}`", retry + 1);
                        }
                    }
                    return Ok(EXIT_OK);
                }
                "cancelled" => {
                    if json {
                        say!("{}", serde_json::to_string_pretty(&job).unwrap_or_default());
                    }
                    eprintln!("job {job_id} 被取消");
                    return Ok(EXIT_JOB_FAILED);
                }
                _ => {
                    if json {
                        say!("{}", serde_json::to_string_pretty(&job).unwrap_or_default());
                    } else {
                        eprintln!("{}", job["message"].as_str().unwrap_or("job 失败"));
                    }
                    return Ok(EXIT_JOB_FAILED);
                }
            }
        }
    }
}

/// Fold the daemon's error body into a [`CliError`] with an exit code.
fn api_error(response: &Response) -> CliError {
    let parsed = serde_json::from_str::<serde_json::Value>(response.body.trim()).ok();
    match parsed {
        Some(value) => api_error_with_body(response, value),
        None => CliError::new(
            code_for_status(response.status, ""),
            format!("HTTP {}:{}", response.status, response.body.trim()),
        ),
    }
}

fn api_error_with_body(response: &Response, body: serde_json::Value) -> CliError {
    let code = body["error"]["code"].as_str().unwrap_or("unknown").to_string();
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or("请求失败")
        .to_string();
    let mut error = CliError::new(code_for_status(response.status, &code), message);
    error.api = Some(json!({
        "code": code,
        "httpStatus": response.status,
        "message": error.message,
        "details": body["error"]["details"],
        "retryAfterSeconds": body["error"]["retryAfterSeconds"],
        "jobId": body["error"]["jobId"],
        "projectId": body["error"]["projectId"],
    }));
    error
}

/// HTTP status -> exit code.
///
/// **403 splits in two**: `authLocalOnly` maps to `EXIT_UNAUTHORIZED` like 401; any other 403 is
/// the caller's mistake (bad path) and must not read as "go log in".
fn code_for_status(status: u16, code: &str) -> u8 {
    match status {
        401 => EXIT_UNAUTHORIZED,
        403 if matches!(code, "authLocalOnly" | "unauthorized") => EXIT_UNAUTHORIZED,
        404 => EXIT_NOT_FOUND,
        409 => EXIT_BUSY,
        _ => EXIT_API,
    }
}

/// Where `cgw login` stores the session cookie.
///
/// Prefers `CGW_HOME` (the per-user shared dir) and deliberately **not** the exe's directory:
/// a PATH location may be read-only (Program Files) and would not survive a machine or upgrade.
fn session_path() -> Option<std::path::PathBuf> {
    if let Some(home) = crate::paths::ensure_cgw_home() {
        return Some(home.join(SESSION_FILE));
    }
    let dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))?;
    Some(dir.join(SESSION_FILE))
}

fn load_session(host: &str) -> Option<String> {
    let path = session_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value[host].as_str().map(str::to_string)
}

fn save_session(host: &str, cookie: Option<&str>) -> Result<(), CliError> {
    let Some(path) = session_path() else {
        return Ok(());
    };
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));
    match cookie {
        Some(cookie) => {
            value[host] = json!(cookie);
        }
        None => {
            if let Some(map) = value.as_object_mut() {
                map.remove(host);
            }
        }
    }
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(&path, text + "\n")
        .map_err(|err| CliError::new(EXIT_API, format!("写入会话文件失败:{err}")))
}

/// Where the address came from - `cgw setup` and errors must say so explicitly.
pub enum UrlSource {
    Flag,
    Env(String),
    RuntimeFile(std::path::PathBuf),
    ConfigFile(std::path::PathBuf),
}

/// Same order as [`discover_url`], but also reports where the address was found.
fn locate_url(args: &Args) -> Option<(String, UrlSource)> {
    if let Some(url) = args.get("url") {
        return Some((url.to_string(), UrlSource::Flag));
    }
    if let Some(raw) = std::env::var("CGW_URL").ok().filter(|v| !v.trim().is_empty()) {
        return Some((raw, UrlSource::Env("CGW_URL".to_string())));
    }
    for dir in exe_dirs() {
        let path = dir.join(RUNTIME_FILE);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(url) = value["url"].as_str() {
                    return Some((url.to_string(), UrlSource::RuntimeFile(path)));
                }
            }
        }
    }
    for dir in exe_dirs() {
        if let Some(url) = url_from_config(&dir) {
            return Some((url, UrlSource::ConfigFile(dir.join("config.json"))));
        }
    }
    None
}

fn url_from_config(dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("config.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let port = value["port"].as_u64().unwrap_or(0);
    if port == 0 {
        // Port 0 means auto-picked, unknowable from config.json - only runtime.json can help
        return None;
    }
    Some(format!("http://127.0.0.1:{port}"))
}

fn exe_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    // The per-user shared dir comes first: the daemon writes a pointer file there, so a `cgw`
    // on PATH can still find it (see `paths::cgw_home`).
    if let Some(home) = crate::paths::cgw_home() {
        dirs.push(home);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if !dirs.contains(&dir.to_path_buf()) {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if !dirs.contains(&cwd) {
            dirs.push(cwd);
        }
    }
    dirs
}

// ------------------------------------------------------------------ environment compatibility layer

/// `cgw setup` - make `cgw` work like `adb`: on PATH, runnable from any directory.
///
/// Reports the source of the daemon address (flag / env / runtime.json / config.json), where
/// `CGW_HOME` is (its pointer file lets the CLI find the daemon from any cwd), whether the `cgw`
/// directory is on PATH, and how to persist that (Windows can use `--apply`).
/// Windows writes the user PATH in the registry; Linux / macOS only **print** the `export` line,
/// because editing a user's shell rc is too invasive for a tool to do.
fn cmd_setup(args: &Args, json: bool) -> Result<u8, CliError> {
    let exe = std::env::current_exe().ok();
    let exe_dir = exe.as_ref().and_then(|path| path.parent()).map(|p| p.to_path_buf());
    let home = crate::paths::cgw_home();
    let located = locate_url(args);
    let on_path = exe_dir
        .as_ref()
        .map(|dir| path_contains(dir))
        .unwrap_or(false);

    if args.has("apply") {
        let Some(dir) = exe_dir.as_ref() else {
            return fail("找不到自己(读不到当前可执行文件路径)".to_string());
        };
        let outcome = apply_to_path(dir)?;
        if json {
            return print_json(&serde_json::json!({
                "ok": true, "dir": dir.display().to_string(), "result": outcome,
            }));
        }
        say!("{outcome}");
        if !cfg!(windows) {
            say!();
            say!("(Linux / macOS 上不代改 shell 配置 —— 把上面那行加进 ~/.bashrc 或 ~/.zshrc 即可。)");
        }
        return Ok(EXIT_OK);
    }

    if json {
        return print_json(&serde_json::json!({
            "exe": exe.as_ref().map(|p| crate::paths::display(p)),
            "exeDir": exe_dir.as_ref().map(|p| crate::paths::display(p)),
            "cgwHome": home.as_ref().map(|p| crate::paths::display(p)),
            "onPath": on_path,
            "daemon": located.as_ref().map(|(url, _)| url.clone()),
            "daemonSource": located.as_ref().map(|(_, source)| describe_source(source)),
            "env": {
                "CGW_URL": std::env::var("CGW_URL").ok(),
                "CGW_USERNAME": std::env::var("CGW_USERNAME").ok().map(|_| "(已设置)"),
                "CGW_PASSWORD": std::env::var("CGW_PASSWORD").ok().map(|_| "(已设置)"),
                "CGW_HOME": std::env::var("CGW_HOME").ok(),
            },
        }));
    }

    say!("cgw 环境");
    say!("  可执行文件   {}", exe.as_ref().map(|p| crate::paths::display(p)).unwrap_or_else(|| "-".into()));
    say!("  共享目录     {}{}", home.as_ref().map(|p| crate::paths::display(p)).unwrap_or_else(|| "-".into()), if home.as_ref().map(|p| p.is_dir()).unwrap_or(false) { "(存在)" } else { "(还没有 —— daemon 起来时会建)" });
    say!("  PATH 里      {}", if on_path { "有" } else { "没有" });
    match &located {
        Some((url, source)) => say!("  daemon       {url}(来自 {})", describe_source(source)),
        None => say!("  daemon       没找到 —— 启动 daemon,或设 CGW_URL / 用 --url 指定"),
    }
    say!();
    say!("用法(和 adb 一个手感):");
    say!("  cgw status                  一次性执行");
    say!("  cgw                         直接回车 = 进交互环境,exit 退出");
    say!("  cgw help <命令>             逐层看用法;cgw help --json 给 agent");
    say!();
    if on_path {
        say!("PATH 已经包含 cgw 所在目录,在任意目录直接敲 `cgw` 就行。");
    } else if cfg!(windows) {
        say!("把 cgw 加进 PATH:`cgw setup --apply`(写当前用户的 Path,不需要管理员)");
    } else {
        let dir = exe_dir.as_ref().map(|p| crate::paths::display(p)).unwrap_or_else(|| "/path/to/cgw".into());
        say!("把这一行加进 ~/.bashrc 或 ~/.zshrc:");
        say!("  export PATH=\"$PATH:{dir}\"");
    }
    say!();
    say!("可选的环境变量(不想每次带 --url 就设上):");
    say!("  CGW_URL=http://127.0.0.1:65251     daemon 地址");
    say!("  CGW_USERNAME / CGW_PASSWORD        需要登录的 daemon(撞到 401 时用一次)");
    say!("  CGW_HOME=<目录>                    换掉共享目录(默认 ~/.compilegateway)");
    Ok(EXIT_OK)
}

fn describe_source(source: &UrlSource) -> String {
    match source {
        UrlSource::Flag => "--url".to_string(),
        UrlSource::Env(name) => format!("环境变量 {name}"),
        UrlSource::RuntimeFile(path) => crate::paths::display(path),
        UrlSource::ConfigFile(path) => format!("{} (port)", crate::paths::display(path)),
    }
}

/// Is the directory on this process's PATH? Accepts both `;` and `:` separators for portability.
fn path_contains(dir: &std::path::Path) -> bool {
    let target = crate::paths::display(dir).trim_end_matches(['/', '\\']).to_ascii_lowercase();
    std::env::var("PATH")
        .unwrap_or_default()
        .split([';', ':'])
        .map(|entry| entry.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase())
        .any(|entry| entry == target)
}

/// Persist `dir` into the user PATH. Windows writes the registry; other platforms return the
/// line to add by hand.
fn apply_to_path(dir: &std::path::Path) -> Result<String, CliError> {
    let text = crate::paths::display(dir);
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = hkcu
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
            .map_err(|err| CliError::new(EXIT_API, format!("读用户环境变量失败:{err}")))?;

        // **Read the raw value and write it back raw**: the user Path may be REG_EXPAND_SZ
        // (containing %USERPROFILE%), and set_value would store REG_SZ and freeze those variables.
        let (current, vtype) = match env.get_raw_value("Path") {
            Ok(raw) => (
                decode_utf16(&raw.bytes),
                raw.vtype,
            ),
            Err(_) => (String::new(), winreg::enums::RegType::REG_EXPAND_SZ),
        };

        if current
            .split(';')
            .map(|entry| entry.trim().trim_end_matches(['/', '\\']).to_ascii_lowercase())
            .any(|entry| entry == text.to_ascii_lowercase())
        {
            return Ok(format!("{text} 已经在用户 Path 里了,没动。"));
        }

        let updated = if current.trim().trim_end_matches(';').is_empty() {
            text.clone()
        } else {
            format!("{};{}", current.trim().trim_end_matches(';'), text)
        };
        env.set_raw_value(
            "Path",
            &winreg::RegValue {
                bytes: encode_utf16(&updated),
                vtype,
            },
        )
        .map_err(|err| CliError::new(EXIT_API, format!("写用户 Path 失败:{err}")))?;
        return Ok(format!(
            "已把 {text} 加到当前用户的 Path。**新开的**终端里 `cgw` 就能直接用了\
             (已经开着的窗口要重开一次才会读到新 PATH)。"
        ));
    }
    #[cfg(not(windows))]
    {
        Ok(format!(
            "Linux / macOS 上不代改 shell 配置。把这一行加进 ~/.bashrc 或 ~/.zshrc:\n  export PATH=\"$PATH:{text}\""
        ))
    }
}

#[cfg(windows)]
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

#[cfg(windows)]
fn encode_utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0u16))
        .flat_map(u16::to_le_bytes)
        .collect()
}

// ------------------------------------------------------------------ cgw set (categorized settings)

/// `cgw set <category> [--key value ...]` - **anything the settings page can change, with the
/// same categories**.
///
/// Why a separate `set` instead of only `config set`: settings page entries are **categorized**
/// (port / listen / credentials / language / rate limit / logs / host roots / MCP), while
/// `config set --port X` is a flat pile of switches. Both exist; `set` is for reading the UI back:
///
/// ```text
/// cgw set port --webui 65251      cgw set port --mcp 3994
/// cgw set listen --webui lan      cgw set listen --mcp lan
/// cgw set user --name alice --password secret
/// cgw set language zh-Hans        cgw set login-interval 60
/// cgw set log-retain 7            cgw set roots add F:\CODEX
/// cgw set mcp off
/// ```
///
/// The protocol only gained `rootsAdd` / `rootsRemove`, so editing the allowlist needs no
/// read-whole-file-then-write-back cycle that two clients could race.
fn cmd_set(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let category = rest.first().map(String::as_str);
    let mut patch = serde_json::Map::new();
    let dry_run = args.has("dry-run");

    match category {
        Some("port") => {
            let mut touched = false;
            if let Some(value) = args.get("webui") {
                patch.insert("port".into(), json!(parse_port(value, "--webui")?));
                touched = true;
            } else if args.has("webui") {
                return fail("--webui 后面要给端口号,例如:cgw set port --webui 65251".to_string());
            }
            if let Some(value) = args.get("mcp") {
                patch.insert("mcp".into(), json!({ "port": parse_port(value, "--mcp")? }));
                touched = true;
            } else if args.has("mcp") {
                return fail("--mcp 后面要给端口号,例如:cgw set port --mcp 3994".to_string());
            }
            if !touched {
                return fail(
                    "要改哪个端口?`cgw set port --webui 65251` 或 `cgw set port --mcp 3994`"
                        .to_string(),
                );
            }
        }
        Some("listen") => {
            let mut touched = false;
            if let Some(value) = args.get("webui") {
                patch.insert("listen".into(), json!(parse_listen(value)?));
                touched = true;
            }
            if let Some(value) = args.get("mcp") {
                patch.insert("mcp".into(), json!({ "listen": parse_listen(value)? }));
                touched = true;
            }
            if !touched {
                return fail(
                    "要改哪个?`cgw set listen --webui lan|wan` 或 `cgw set listen --mcp lan|wan`"
                        .to_string(),
                );
            }
        }
        Some("user") => {
            if args.has("clear") {
                patch.insert("username".into(), json!(""));
                patch.insert("password".into(), json!(""));
            } else {
                if let Some(name) = args.get("name") {
                    patch.insert("username".into(), json!(name));
                }
                if let Some(password) = args.get("password") {
                    patch.insert("password".into(), json!(password));
                }
                if patch.is_empty() {
                    return fail(
                        "`cgw set user --name <用户名> [--password <密码>]`,或 `--clear` 清掉"
                            .to_string(),
                    );
                }
            }
        }
        Some("language") => {
            let value = rest
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| "用法:cgw set language auto|zh-Hans|en-US".to_string())?;
            patch.insert("language".into(), json!(value));
        }
        Some("login-interval") => {
            let value = rest
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| "用法:cgw set login-interval <秒>(0 = 不限流)".to_string())?;
            let seconds: u64 = value.parse().map_err(|_| format!("要是非负整数:{value}"))?;
            patch.insert("loginIntervalSeconds".into(), json!(seconds));
        }
        Some("log-retain") => {
            let value = rest
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| "用法:cgw set log-retain <天数>(默认 7,上限 3650)".to_string())?;
            let days: u64 = value.parse().map_err(|_| format!("要是非负整数:{value}"))?;
            patch.insert("logRetainDays".into(), json!(days));
        }
        Some("roots") => match rest.get(1).map(String::as_str) {
            None | Some("list") => {
                let body = server.json("GET", "/api/config", None)?;
                if json {
                    return print_json(&body["hostRoots"]);
                }
                let roots = body["hostRoots"].as_array().cloned().unwrap_or_default();
                say!("当前允许的宿主目录(按顺序):");
                for (index, root) in roots.iter().enumerate() {
                    say!(
                        "  {index}.  {}{}",
                        root.as_str().unwrap_or("-"),
                        if index == 0 { "   (用户主目录,默认就有)" } else { "" }
                    );
                }
                say!();
                say!("加一个:`cgw set roots add <绝对路径>`;去掉:`cgw set roots rm <绝对路径>`");
                return Ok(EXIT_OK);
            }
            Some(action @ ("add" | "rm" | "remove" | "set")) => {
                let paths: Vec<String> = rest[2..]
                    .iter()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect();
                if paths.is_empty() {
                    return fail(format!("`cgw set roots {action}` 后面要给至少一个绝对路径"));
                }
                let key = match action {
                    "add" => "rootsAdd",
                    "set" => "allowedRoots",
                    _ => "rootsRemove",
                };
                patch.insert(key.into(), json!(paths));
            }
            Some(other) => {
                return fail(format!(
                    "未知的 roots 动作:{other}(支持 list / add / rm / set)"
                ))
            }
        },
        Some("mcp") => {
            let value = rest
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| "用法:cgw set mcp on|off".to_string())?;
            patch.insert("mcp".into(), json!({ "enabled": parse_bool(value)? }));
        }
        // Host-wide lock (design doc §7.6.6, end): parsing **reuses** `setcmd`, the very function
        // the CLI window calls, so on/off/true/false handling and errors exist only once.
        Some("host-lock") => {
            let intent = setcmd::parse(rest).map_err(|err| CliError::usage(err.message()))?;
            match intent {
                setcmd::Intent::Patch(value) => {
                    if let Some(map) = value.as_object() {
                        for (key, item) in map {
                            patch.insert(key.clone(), item.clone());
                        }
                    }
                }
                _ => return fail("用法:`cgw set host-lock on|off`".to_string()),
            }
        }
        Some("all") | None => {
            let body = server.json("GET", "/api/config", None)?;
            if json {
                return print_json(&body);
            }
            say!("配置页上能改的都在这里(括号里是对应的命令):");
            say!();
            say!("  端口");
            say!("    WebUI           {}        (cgw set port --webui N)", body["port"]);
            say!("    MCP             {}        (cgw set port --mcp N)", body["mcp"]["port"]);
            say!("  监听范围");
            say!(
                "    WebUI           {}        (cgw set listen --webui lan|wan)",
                body["listen"].as_str().unwrap_or("-")
            );
            say!(
                "    MCP             {}        (cgw set listen --mcp lan|wan)",
                body["mcp"]["listen"].as_str().unwrap_or("-")
            );
            say!("  访问凭据(只有本机可改)");
            say!(
                "    用户名          {}        (cgw set user --name U)",
                body["username"].as_str().unwrap_or("(未设置)")
            );
            say!(
                "    密码            {}        (cgw set user --password P / --clear)",
                if body["hasPassword"].as_bool() == Some(true) { "已设置" } else { "未设置" }
            );
            say!("  界面");
            say!(
                "    语言            {}        (cgw set language auto|zh-Hans|en-US)",
                body["language"].as_str().unwrap_or("-")
            );
            say!(
                "    登录限流        {} 秒     (cgw set login-interval N)",
                body["loginIntervalSeconds"]
            );
            say!(
                "    日志保留        {} 天     (cgw set log-retain N)",
                body["logRetainDays"]
            );
            say!("  并发");
            say!(
                "    主机级互斥      {}        (cgw set host-lock on|off;同一台客户机一次只跑一个 job)",
                if body["hostLock"].as_bool() == Some(true) { "开" } else { "关" }
            );
            say!("  宿主目录白名单");
            for (index, root) in body["hostRoots"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                say!(
                    "    {index}. {}{}",
                    root.as_str().unwrap_or("-"),
                    if index == 0 { "   (用户主目录)" } else { "" }
                );
            }
            say!("                    (cgw set roots add|rm|list <路径>)");
            say!("  MCP");
            say!(
                "    开关            {}        (cgw set mcp on|off)",
                if body["mcp"]["enabled"].as_bool() == Some(true) { "开" } else { "关" }
            );
            say!("    端点            {}", body["mcp"]["endpoint"].as_str().unwrap_or("-"));
            say!();
            say!("每一项都能在配置页上点到;`cgw set all --json` 给程序看。");
            return Ok(EXIT_OK);
        }
        Some(other) => {
            return fail(format!(
                "未知分类:{other}。支持 port / listen / user / language / login-interval / log-retain / roots / mcp / host-lock / all"
            ))
        }
    }

    if patch.is_empty() {
        return fail("没有要改的东西".to_string());
    }
    let patch = serde_json::Value::Object(patch);
    if dry_run {
        if json {
            return print_json(&patch);
        }
        say!("(dry-run)会发送这个 patch:");
        say!("{}", serde_json::to_string_pretty(&patch).unwrap_or_default());
        return Ok(EXIT_OK);
    }

    let result = server.json("POST", "/api/config", Some(&patch.to_string()))?;
    if json {
        return print_json(&result);
    }
    if result["restartRequired"].as_bool() == Some(true) {
        say!("已保存。端口 / 监听模式的改动要重启程序才生效(MCP 与宿主目录那几项立即生效)。");
    } else {
        say!("已保存(立即生效)。");
    }
    Ok(EXIT_OK)
}

fn parse_port(value: &str, flag: &str) -> Result<u16, String> {
    value
        .parse()
        .map_err(|_| format!("{flag} 必须是 0..65535 的整数:{value}"))
}

fn parse_listen(value: &str) -> Result<String, String> {
    if value != "lan" && value != "wan" {
        return Err(format!("只能是 lan 或 wan:{value}"));
    }
    Ok(value.to_string())
}

fn cmd_status(server: &Server, json: bool) -> Result<u8, CliError> {
    let status = server.json("GET", "/api/status", None)?;
    if json {
        return print_json(&status);
    }
    let projects = server.json("GET", "/api/projects", None).unwrap_or_default();
    let running = projects["running"].as_object().map(|m| m.len()).unwrap_or(0);

    say!("{APP_NAME} {}", status["version"].as_str().unwrap_or("?"));
    kv("url", status["url"].as_str().unwrap_or("-"));
    kv("listen", status["listen"].as_str().unwrap_or("-"));
    kv("language", status["language"].as_str().unwrap_or("-"));
    kv("config", status["configPath"].as_str().unwrap_or("-"));
    kv("clients", &count(&server.json("GET", "/api/clients", None)?["clients"]));
    kv("projects", &count(&projects["projects"]));
    kv("jobs running", &running.to_string());
    if status["warning"].is_string() {
        kv("warning", status["warning"].as_str().unwrap_or(""));
    }
    Ok(EXIT_OK)
}

fn cmd_clients(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        None | Some("list") => {
            let body = server.json("GET", "/api/clients", None)?;
            if json {
                return print_json(&body);
            }
            let list = body["clients"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                say!("(还没有客户机)");
            }
            for client in list {
                say!(
                    "{:<18} {:<16} {:<22} {}",
                    client["id"].as_str().unwrap_or("-"),
                    client["name"].as_str().unwrap_or("-"),
                    format!(
                        "{}:{}",
                        client["host"].as_str().unwrap_or("-"),
                        client["port"]
                    ),
                    client["auth"]["method"].as_str().unwrap_or("-"),
                );
            }
            Ok(EXIT_OK)
        }
        Some("test") => {
            let id = require(rest, 1, "clients test <id>")?;
            let body = server.json("POST", &format!("/api/clients/{}/test", enc(id)), None)?;
            if json {
                return print_json(&body);
            }
            say!(
                "{} {}",
                if body["ok"].as_bool().unwrap_or(false) {
                    "OK  "
                } else {
                    "FAIL"
                },
                body["message"].as_str().unwrap_or("")
            );
            let output = body["output"].as_str().unwrap_or("");
            if !output.trim().is_empty() {
                say!("{}", output.trim());
            }
            Ok(EXIT_OK)
        }
        Some("add") => {
            let usage = "usage: clients add --host <ip> [--name N] [--port 22] [--user U] [--password P | --key <路径> [--key-passphrase P]] [--tag T] [--base-dir DIR] [--host-lock]";
            let host = args.get("host").ok_or(usage)?;
            let method = if args.get("key").is_some() { "key" } else { "password" };
            let mut auth = serde_json::json!({
                "method": method,
                "username": args.get("user").unwrap_or(""),
                "keyPath": args.get("key").unwrap_or(""),
            });
            if let Some(password) = args.get("password") {
                auth["password"] = serde_json::Value::String(password.to_string());
            }
            // Key passphrase: `ssh-keygen` encrypts private keys by default, so this gate is common
            if let Some(passphrase) = args.get("key-passphrase") {
                auth["passphrase"] = serde_json::Value::String(passphrase.to_string());
            }
            let host_lock = match args.get("host-lock") {
                Some(val) => match val.to_ascii_lowercase().as_str() {
                    "false" | "off" | "0" | "no" => false,
                    _ => true,
                },
                None => args.has("host-lock"),
            };
            let payload = serde_json::json!({
                "id": "",
                "name": args.get("name").unwrap_or(host),
                "host": host,
                "port": args.get("port").and_then(|p| p.parse::<u16>().ok()).unwrap_or(22),
                "auth": auth,
                "tags": args.get("tag").map(|t| vec![t.to_string()]).unwrap_or_default(),
                "baseDir": args.get("base-dir").unwrap_or("~/CompileGateway"),
                "hostLock": host_lock,
            });
            let body = server.json("POST", "/api/clients", Some(&payload.to_string()))?;
            if json {
                return print_json(&body);
            }
            say!(
                "added {} ({})",
                body["client"]["id"].as_str().unwrap_or("-"),
                body["client"]["name"].as_str().unwrap_or("-")
            );
            Ok(EXIT_OK)
        }
        Some("edit") => {
            let usage = "usage: clients edit <id> [--name N] [--host IP] [--port P] [--method password|key] [--user U] [--password P] [--key 路径] [--key-passphrase P] [--tag T]... [--base-dir 路径] [--host-lock | --no-host-lock]";
            let id = require(rest, 1, usage)?;
            // Read current values first: the daemon's save endpoint replaces the whole record, so
            // omitted fields must be carried back or the existing name/password would vanish.
            let current = server.json("GET", "/api/clients", None)?;
            let existing = current["clients"]
                .as_array()
                .and_then(|list| list.iter().find(|item| item["id"] == id))
                .cloned()
                .ok_or_else(|| CliError::new(EXIT_NOT_FOUND, format!("找不到客户机 {id}")))?;

            let mut auth = json!({
                "method": args
                    .get("method")
                    .unwrap_or(existing["auth"]["method"].as_str().unwrap_or("password")),
                "username": args
                    .get("user")
                    .unwrap_or(existing["auth"]["username"].as_str().unwrap_or("")),
                "keyPath": args
                    .get("key")
                    .unwrap_or(existing["auth"]["keyPath"].as_str().unwrap_or("")),
            });
            // Omitting password / passphrase means "leave unchanged" (the daemon agrees)
            if let Some(password) = args.get("password") {
                auth["password"] = json!(password);
            }
            if let Some(passphrase) = args.get("key-passphrase") {
                auth["passphrase"] = json!(passphrase);
            }

            let tags: Vec<String> = match args.get("tag") {
                Some(_) => args
                    .raw
                    .windows(2)
                    .filter(|pair| pair[0] == "--tag")
                    .map(|pair| pair[1].clone())
                    .collect(),
                None => existing["tags"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            };

            let host_lock = if args.has("no-host-lock") {
                Some(false)
            } else if let Some(val) = args.get("host-lock") {
                match val.to_ascii_lowercase().as_str() {
                    "false" | "off" | "0" | "no" => Some(false),
                    _ => Some(true),
                }
            } else if args.has("host-lock") {
                Some(true)
            } else {
                existing["hostLock"].as_bool()
            };

            let payload = json!({
                "id": id,
                "name": args.get("name").unwrap_or(existing["name"].as_str().unwrap_or("")),
                "host": args.get("host").unwrap_or(existing["host"].as_str().unwrap_or("")),
                "port": args
                    .get("port")
                    .and_then(|p| p.parse::<u16>().ok())
                    .or_else(|| existing["port"].as_u64().map(|p| p as u16))
                    .unwrap_or(22),
                "auth": auth,
                "tags": tags,
                "baseDir": args
                    .get("base-dir")
                    .unwrap_or(existing["baseDir"].as_str().unwrap_or("~/CompileGateway")),
                "hostLock": host_lock.unwrap_or(false),
            });
            let body = server.json("POST", "/api/clients", Some(&payload.to_string()))?;
            if json {
                return print_json(&body);
            }
            say!(
                "已更新 {} ({})",
                body["client"]["id"].as_str().unwrap_or("-"),
                body["client"]["name"].as_str().unwrap_or("-")
            );
            Ok(EXIT_OK)
        }
        Some("hostkey") => {
            let id = require(
                rest,
                1,
                "clients hostkey <id> [--trust <SHA256:...> | --forget | --import <文件|-> [--force] | --export]",
            )?;

            // Pin this machine's key from a known_hosts file (offline pre-trust); the CLI reads it
            // because the file is on the **user's** machine, so this works with a remote daemon.
            if let Some(source) = args.flag_value("--import") {
                let text = read_text_argument(&source)?;
                // An empty file looks like "neither raw nor path given" to the API, producing a
                // roundabout error, so intercept it here with plainer wording (a dry pipe is common).
                if text.trim().is_empty() {
                    return Err(CliError::usage(format!(
                        "{source} 里没有内容 —— 检查一下文件路径,或者管道那头是不是没输出"
                    )));
                }
                let payload = json!({ "raw": text, "force": args.has("force") }).to_string();
                let body = server.json(
                    "POST",
                    &format!("/api/clients/{}/hostkey/import", enc(id)),
                    Some(&payload),
                )?;
                if json {
                    return print_json(&body);
                }
                if body["alreadyTrusted"].as_bool() == Some(true) {
                    say!(
                        "记录里本来就是这把钥匙,没动:{}",
                        body["fingerprint"].as_str().unwrap_or("")
                    );
                } else {
                    say!(
                        "已从第 {} 行导入 {} {}",
                        body["line"],
                        body["keyType"].as_str().unwrap_or(""),
                        body["fingerprint"].as_str().unwrap_or("")
                    );
                }
                for note in body["notes"].as_array().cloned().unwrap_or_default() {
                    say!("提示:{}", note.as_str().unwrap_or(""));
                }
                for problem in body["problems"].as_array().cloned().unwrap_or_default() {
                    eprintln!("跳过:{}", problem.as_str().unwrap_or(""));
                }
                return Ok(EXIT_OK);
            }

            // Export one known_hosts line (appendable straight to ~/.ssh/known_hosts)
            if args.has("export") {
                let body = server.json(
                    "GET",
                    &format!("/api/clients/{}/known-hosts", enc(id)),
                    None,
                )?;
                if json {
                    return print_json(&body);
                }
                say!("{}", body["line"].as_str().unwrap_or(""));
                if let Some(warning) = body["warning"].as_str() {
                    eprintln!("注意:{warning}");
                }
                return Ok(EXIT_OK);
            }

            let trust = args.flag_value("--trust");

            // No argument = just show the currently recorded fingerprint
            if trust.is_none() && !args.has("forget") {
                let found = server.json("GET", "/api/clients", None)?;
                let client = found["clients"]
                    .as_array()
                    .and_then(|list| list.iter().find(|item| item["id"] == id))
                    .cloned()
                    .ok_or_else(|| CliError::new(EXIT_NOT_FOUND, format!("找不到客户机 {id}")))?;
                let key = client["hostKey"].as_str().unwrap_or("");
                if json {
                    return print_json(&json!({
                        "id": id,
                        "hostKey": key,
                        "hasKeyLine": client["hasKeyLine"],
                    }));
                }
                if key.is_empty() {
                    say!("(还没记录过指纹,首次连接时自动信任)");
                } else {
                    kv("hostKey", key);
                    // A fingerprint alone cannot yield a standard known_hosts line; say how to fix that
                    if client["hasKeyLine"].as_bool() != Some(true) {
                        say!("(只有指纹,没有公钥原文;`--export` 拿不到标准格式,测一次连接就有了)");
                    }
                }
                return Ok(EXIT_OK);
            }

            let trusting = trust.is_some();
            let payload = json!({ "fingerprint": trust.unwrap_or_default() }).to_string();
            let body = server.json(
                "POST",
                &format!("/api/clients/{}/hostkey", enc(id)),
                Some(&payload),
            )?;
            if json {
                return print_json(&body);
            }
            if trusting {
                say!(
                    "已信任新的主机指纹:{}",
                    body["client"]["hostKey"].as_str().unwrap_or("")
                );
            } else {
                say!("已清除记录的主机指纹;下次连接会重新按首次信任处理。");
            }
            Ok(EXIT_OK)
        }
        Some("rm") | Some("delete") => {
            let id = require(rest, 1, "clients rm <id>")?;
            server.json("DELETE", &format!("/api/clients/{}", enc(id)), None)?;
            say!("已删除 {id}");
            Ok(EXIT_OK)
        }
        Some(other) => fail(format!("未知子命令:clients {other}")),
    }
}

fn cmd_projects(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        None | Some("list") => {
            let body = server.json("GET", "/api/projects", None)?;
            if json {
                return print_json(&body);
            }
            let list = body["projects"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                say!("(还没有项目)");
            }
            for project in list {
                say!(
                    "{:<18} {:<16} {:<44} tasks={}",
                    project["id"].as_str().unwrap_or("-"),
                    project["name"].as_str().unwrap_or("-"),
                    project["remotePath"].as_str().unwrap_or("-"),
                    project["tasks"].as_array().map(|t| t.len()).unwrap_or(0),
                );
            }
            Ok(EXIT_OK)
        }
        Some("add") => {
            let client = args
                .get("client")
                .ok_or("用法:projects add --client <clientId> --path <宿主机目录> [--name <名字>]")?;
            let path = args
                .get("path")
                .ok_or("用法:projects add --client <clientId> --path <宿主机目录> [--name <名字>]")?;
            let mut payload = serde_json::json!({ "clientId": client, "sourcePath": path });
            if let Some(name) = args.get("name") {
                payload["name"] = serde_json::Value::String(name.to_string());
            }
            let body = server.json("POST", "/api/projects", Some(&payload.to_string()))?;
            if json {
                return print_json(&body);
            }
            say!(
                "已登记 {} -> {}",
                body["project"]["id"].as_str().unwrap_or("-"),
                body["project"]["remotePath"].as_str().unwrap_or("-")
            );
            if let Some(note) = body["message"].as_str() {
                if !note.is_empty() {
                    say!("{note}");
                }
            }
            Ok(EXIT_OK)
        }
        Some("rm") | Some("delete") => {
            let id = require(rest, 1, "projects rm <id>")?;
            server.json("DELETE", &format!("/api/projects/{}", enc(id)), None)?;
            say!("已移除登记 {id}(没有删除任何文件)");
            Ok(EXIT_OK)
        }
        Some(other) => fail(format!("未知子命令:projects {other}")),
    }
}

/// `cgw tasks [list] <project>` / `tasks save` / `tasks rm` / `tasks preview` -
/// full CRUD for project tasks (everything the WebUI can do).
fn cmd_tasks(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        Some("save") => {
            let project = require(rest, 1, "tasks save <projectId> --body <JSON> | --file <路径> | -")?;
            let payload = args.json_argument(2)?;
            let body = server.json(
                "POST",
                &format!("/api/projects/{}/tasks", enc(project)),
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body["project"]["tasks"]);
            }
            let task = body["project"]["tasks"]
                .as_array()
                .and_then(|list| {
                    let id = payload["id"].as_str().unwrap_or("");
                    if id.is_empty() {
                        list.last().cloned()
                    } else {
                        list.iter().find(|item| item["id"] == id).cloned()
                    }
                })
                .unwrap_or_default();
            say!("已保存任务 {}", task["id"].as_str().unwrap_or("-"));
            Ok(EXIT_OK)
        }
        Some("rm") => {
            let project = require(rest, 1, "tasks rm <projectId> <taskId>")?;
            let task_id = require(rest, 2, "tasks rm <projectId> <taskId>")?;
            let body = server.json(
                "DELETE",
                &format!(
                    "/api/projects/{}/tasks/{}",
                    enc(project),
                    enc(task_id)
                ),
                None,
            )?;
            if json {
                return print_json(&body);
            }
            say!("已删除任务 {task_id}");
            Ok(EXIT_OK)
        }
        Some("preview") => {
            let project = require(rest, 1, "tasks preview <projectId> <taskId>")?;
            let task_id = require(rest, 2, "tasks preview <projectId> <taskId>")?;
            let body = server.json(
                "POST",
                &format!(
                    "/api/projects/{}/tasks/{}/preview",
                    enc(project),
                    enc(task_id)
                ),
                None,
            )?;
            if json {
                return print_json(&body);
            }
            kv("kind", body["kind"].as_str().unwrap_or("-"));
            if let Some(command) = body["command"].as_str() {
                if !command.is_empty() {
                    kv("command", command);
                }
            }
            let artifacts = body["artifacts"].as_array().cloned().unwrap_or_default();
            if !artifacts.is_empty() {
                kv(
                    "artifacts",
                    &artifacts
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            for key in ["from", "to", "target", "sources", "outputName", "clean", "overwrite"] {
                if !body[key].is_null() {
                    kv(key, &body[key].to_string());
                }
            }
            Ok(EXIT_OK)
        }
        _ => {
            let project = require(rest, 0, "tasks [list] <projectId>")?;
            let found = project_of(server, project)?;
            if json {
                return print_json(&found["tasks"]);
            }
            let tasks = found["tasks"].as_array().cloned().unwrap_or_default();
            if tasks.is_empty() {
                say!("(这个项目还没有任务)");
            }
            say!("{:<18} {:<10} {:<26} {}", "id", "kind", "name", "detail");
            for task in tasks {
                say!(
                    "{:<18} {:<10} {:<26} {}",
                    task["id"].as_str().unwrap_or("-"),
                    task["kind"].as_str().unwrap_or("-"),
                    task["name"].as_str().unwrap_or("-"),
                    task_detail(&task),
                );
            }
            let _ = args;
            Ok(EXIT_OK)
        }
    }
}

/// Show only the fields relevant to each task kind.
fn task_detail(task: &serde_json::Value) -> String {
    match task["kind"].as_str().unwrap_or("-") {
        "build" => {
            let variables = task["variables"]
                .as_object()
                .map(|map| {
                    map.iter()
                        .take(2)
                        .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("")))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let artifacts = task["artifacts"].as_array().cloned().unwrap_or_default();
            format!(
                "{} · {}{}{}",
                task["manifest"].as_str().unwrap_or("-"),
                task["signingMode"].as_str().unwrap_or("-"),
                if variables.is_empty() {
                    String::new()
                } else {
                    format!(" · {variables}")
                },
                if artifacts.is_empty() {
                    String::new()
                } else {
                    format!(" · {} 个产物", artifacts.len())
                },
            )
        }
        "copy" | "move" => format!(
            "{} -> {}{}",
            location_text(&task["from"]),
            location_text(&task["to"]),
            if task["overwrite"].as_bool() == Some(true) {
                " · 覆盖"
            } else {
                ""
            }
        ),
        "delete" => location_text(&task["target"]),
        "pull" => format!(
            "{} 个来源 -> {}",
            task["sources"].as_array().map(Vec::len).unwrap_or(0),
            location_text(&task["to"])
        ),
        "export" => format!(
            "{} 个来源 -> {}",
            task["sources"].as_array().map(Vec::len).unwrap_or(0),
            task["outputName"].as_str().unwrap_or("export.zip")
        ),
        "push" => format!("clean={}", task["clean"].as_bool().unwrap_or(false)),
        _ => String::new(),
    }
}

fn location_text(location: &serde_json::Value) -> String {
    format!(
        "{}:{}",
        location["side"].as_str().unwrap_or("?"),
        location["path"].as_str().unwrap_or("-")
    )
}


/// `cgw batches <project>` - list batch workflows.
fn cmd_batches(server: &Server, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "batches <projectId>")?;
    let found = project_of(server, project)?;
    if json {
        return print_json(&found["batches"]);
    }
    let batches = found["batches"].as_array().cloned().unwrap_or_default();
    if batches.is_empty() {
        say!("(这个项目还没有批量工作流)");
    }
    for batch in batches {
        let steps = batch["steps"]
            .as_array()
            .map(|list| {
                list.iter()
                    .map(|step| step["taskId"].as_str().unwrap_or("-").to_string())
                    .collect::<Vec<_>>()
                    .join(" -> ")
            })
            .unwrap_or_default();
        say!(
            "{:<16} {:<24} {steps}",
            batch["id"].as_str().unwrap_or("-"),
            batch["name"].as_str().unwrap_or("-"),
        );
    }
    Ok(EXIT_OK)
}

/// `cgw batch run <project> <batchId> [--from N] [--wait]` / `batch rm` / `batch save`
fn cmd_batch(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        Some("run") => {
            let project = require(
                rest,
                1,
                "batch run <projectId> <batchId> [--from N] [--wait] [--enqueue]",
            )?;
            let batch = require(
                rest,
                2,
                "batch run <projectId> <batchId> [--from N] [--wait] [--enqueue]",
            )?;
            let from = args.flag_value("--from").and_then(|value| value.parse::<usize>().ok());
            let payload = match from {
                Some(from) => json!({ "fromStep": from }),
                None => json!({}),
            };
            // `--enqueue` = run queued (design doc §7.6.6)
            let suffix = if args.has("enqueue") { "enqueue" } else { "run" };
            server.start_and_maybe_wait(
                args,
                &format!(
                    "/api/projects/{}/batches/{}/{}",
                    enc(project),
                    enc(batch),
                    suffix
                ),
                Some(&payload.to_string()),
                json,
            )
        }
        Some("rm") => {
            let project = require(rest, 1, "batch rm <projectId> <batchId>")?;
            let batch = require(rest, 2, "batch rm <projectId> <batchId>")?;
            let body = server.json(
                "DELETE",
                &format!(
                    "/api/projects/{}/batches/{}",
                    enc(project),
                    enc(batch)
                ),
                None,
            )?;
            if json {
                return print_json(&body);
            }
            say!("已删除 {batch}");
            Ok(EXIT_OK)
        }
        Some("save") => {
            // Two forms: `batch save <project> <name> <taskId...>` (quick) or
            // `batch save <project> --body/--file <Batch JSON>` (when step-level variables are needed)
            let project = require(rest, 1, "batch save <projectId> <name> <taskId...> | --body <JSON>")?;
            let payload = match rest.get(2) {
                Some(name) if !name.trim_start().starts_with('{') => {
                    let task_ids = &rest[3..];
                    if task_ids.is_empty() {
                        return Err(CliError::usage("至少要给一个 taskId,或者用 --body 给完整 JSON"));
                    }
                    json!({
                        "name": name,
                        "steps": task_ids.iter().map(|id| json!({ "taskId": id })).collect::<Vec<_>>(),
                    })
                }
                _ => args.json_argument(2)?,
            };
            let body = server.json(
                "POST",
                &format!("/api/projects/{}/batches", enc(project)),
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body["project"]["batches"]);
            }
            let batch = body["project"]["batches"]
                .as_array()
                .and_then(|list| {
                    let id = payload["id"].as_str().unwrap_or("");
                    if id.is_empty() {
                        list.last().cloned()
                    } else {
                        list.iter().find(|item| item["id"] == id).cloned()
                    }
                })
                .unwrap_or_default();
            say!(
                "已保存批量工作流 {} ({} 步)",
                batch["name"].as_str().unwrap_or("-"),
                batch["steps"].as_array().map(Vec::len).unwrap_or(0)
            );
            Ok(EXIT_OK)
        }
        _ => Err(CliError::usage(
            "用法:batch run|save|rm ...(见 `cgw help`)",
        )),
    }
}

/// `cgw run <projectId> <taskId> [--wait] [--enqueue]` - run a task.
///
/// `--enqueue` = run queued (design doc §7.6.6): while the project is busy, queue at the tail
/// instead of erroring.
fn cmd_build(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "run <projectId> <taskId> [--wait] [--enqueue]")?;
    let task = require(rest, 1, "run <projectId> <taskId> [--wait] [--enqueue]")?;
    let suffix = if args.has("enqueue") { "enqueue" } else { "run" };
    server.start_and_maybe_wait(
        args,
        &format!(
            "/api/projects/{}/tasks/{}/{}",
            enc(project),
            enc(task),
            suffix
        ),
        None,
        json,
    )
}

/// `cgw task <projectId> <taskId>` - show a task's full JSON definition.
fn cmd_task(server: &Server, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "task <projectId> <taskId>")?;
    let task_id = require(rest, 1, "task <projectId> <taskId>")?;
    let found = project_of(server, project)?;
    let task = found["tasks"]
        .as_array()
        .and_then(|list| list.iter().find(|t| t["id"] == task_id))
        .cloned()
        .ok_or_else(|| format!("找不到任务 {task_id}"))?;
    if json {
        return print_json(&task);
    }
    say!("{}", httpclient::pretty(&task.to_string()));
    Ok(EXIT_OK)
}

/// `cgw env <clientId> [--cached]` - target machine environment probe (design doc §8).
///
/// `cgw env --script` only prints the script to be run and connects to nothing: the probe runs a
/// dozen commands on someone else's machine, so let people read it first.
fn cmd_env(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    if args.has("script") {
        say!("{}", crate::env::probe_script());
        return Ok(EXIT_OK);
    }

    let id = require(rest, 0, "env <clientId> [--cached] | env --script")?;
    let body = if args.has("cached") {
        server.json("GET", &format!("/api/clients/{}/env", enc(id)), None)?
    } else {
        server.json("POST", &format!("/api/clients/{}/env", enc(id)), None)?
    };

    if json {
        return print_json(&body["report"]);
    }
    let report = &body["report"];
    if report.is_null() {
        say!("(还没有检测结果,去掉 --cached 跑一次)");
        return Ok(EXIT_OK);
    }

    kv("client", report["clientName"].as_str().unwrap_or(id));
    kv("os", report["os"].as_str().unwrap_or("-"));
    kv("message", report["message"].as_str().unwrap_or(""));
    if let Some(disk) = report["disk"].as_object() {
        let total = disk.get("totalBytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let free = disk.get("availableBytes").and_then(|v| v.as_u64()).unwrap_or(0);
        kv(
            "disk",
            &format!(
                "{} 可用 / {} 总计 ({})",
                human(free),
                human(total),
                disk.get("mount").and_then(|v| v.as_str()).unwrap_or("-")
            ),
        );
    }

    // Print grouped by platform (design doc §8 requires platform grouping)
    for group in report["groups"].as_array().cloned().unwrap_or_default() {
        let platform = group["platform"].as_str().unwrap_or("-");
        say!();
        say!("[{}]{}", platform, if group["available"] == true { "" } else { "  (全部缺失)" });
        for item in group["items"].as_array().cloned().unwrap_or_default() {
            let found = item["found"].as_bool() == Some(true);
            let version = item["version"].as_str().unwrap_or("");
            say!(
                "  {:<16} {:<8} {}",
                item["label"].as_str().unwrap_or("-"),
                if found { version } else { "缺失" },
                if found { "" } else { item["command"].as_str().unwrap_or("") }
            );
        }
    }

    let xcodes = report["xcodes"].as_array().cloned().unwrap_or_default();
    if !xcodes.is_empty() {
        say!();
        say!("[Xcode 实例]");
        for instance in xcodes {
            say!(
                "  {}  {}{}",
                instance["version"].as_str().unwrap_or("-"),
                instance["path"].as_str().unwrap_or("-"),
                if instance["build"].as_str().unwrap_or("").is_empty() {
                    String::new()
                } else {
                    format!(" (Build {})", instance["build"].as_str().unwrap_or(""))
                }
            );
        }
        let license = report["licenseOk"].as_bool() == Some(true);
        say!(
            "  许可: {}",
            if license { "已接受" } else { "未接受(构建前要先跑 xcodebuild -license accept)" }
        );
    }
    Ok(EXIT_OK)
}

/// `cgw tree <project> [path] [--side remote|host]` - project file tree (design doc §7.4).
fn cmd_tree(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "tree <projectId> [path] [--side remote|host]")?;
    let path = rest.get(1).map(String::as_str).unwrap_or("");
    let side = args.get("side").unwrap_or("remote");
    if side != "remote" && side != "host" {
        return fail(format!("--side 只能是 remote 或 host:{side}"));
    }

    let body = server.json(
        "GET",
        &format!(
            "/api/projects/{}/tree?side={}&path={}",
            enc(project),
            enc(side),
            enc(path)
        ),
        None,
    )?;
    if json {
        return print_json(&body["tree"]);
    }

    let listing = &body["tree"];
    kv("side", listing["side"].as_str().unwrap_or("-"));
    kv("root", listing["root"].as_str().unwrap_or("-"));
    kv(
        "path",
        if listing["path"].as_str().unwrap_or("").is_empty() {
            "."
        } else {
            listing["path"].as_str().unwrap_or(".")
        },
    );
    if let Some(parent) = listing["parent"].as_str() {
        kv("parent", if parent.is_empty() { "." } else { parent });
    }
    say!();

    let entries = listing["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        say!("(空目录)");
    }
    for entry in entries {
        let kind = entry["kind"].as_str().unwrap_or("?");
        say!(
            "{} {:<28} {:>10}  {}",
            match kind {
                "dir" => "d",
                "file" => "-",
                "symlink" => "l",
                _ => "?",
            },
            entry["name"].as_str().unwrap_or("-"),
            if kind == "file" {
                human(entry["size"].as_u64().unwrap_or(0))
            } else {
                String::new()
            },
            entry["path"].as_str().unwrap_or(""),
        );
    }
    if listing["truncated"].as_bool() == Some(true) {
        say!();
        say!("(已达 2000 个条目上限。若需定位深层文件，请指定子目录: cgw tree <project> <subdir>，或运行 cgw export 打包下载)");
    }
    Ok(EXIT_OK)
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `cgw logs [days|<YYYY-MM-DD>] [--source S] [--scope X] [--limit N]` -
/// on-disk log history (design doc §10).
fn cmd_logs(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let target = rest.first().map(String::as_str).unwrap_or("days");

    // `cgw logs client <clientId> [--since N]` - the live log shown in an instance tab (the WebUI
    // log page reads this same endpoint, scoped per client).
    if target == "client" {
        let id = require(rest, 1, "logs client <clientId> [--since N]")?;
        let since = args
            .get("since")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let body = server.json(
            "GET",
            &format!("/api/clients/{}/logs?since={since}", enc(id)),
            None,
        )?;
        if json {
            return print_json(&body);
        }
        for entry in body["entries"].as_array().cloned().unwrap_or_default() {
            say!(
                "{} [{:<5}] {:<8} {}",
                short_time(entry["ts_ms"].as_u64().unwrap_or(0)),
                entry["level"].as_str().unwrap_or("out"),
                entry["source"].as_str().unwrap_or("-"),
                entry["text"].as_str().unwrap_or("")
            );
        }
        return Ok(EXIT_OK);
    }

    if target == "days" {
        let body = server.json("GET", "/api/logs/days", None)?;
        if json {
            return print_json(&body);
        }
        kv("dir", body["dir"].as_str().unwrap_or("-"));
        kv(
            "retain",
            &format!("{} 天", body["retainDays"].as_u64().unwrap_or(0)),
        );
        if body["failed"].as_bool() == Some(true) {
            say!("(注意:日志写盘失败过,检查目录权限)");
        }
        say!();
        let days = body["days"].as_array().cloned().unwrap_or_default();
        if days.is_empty() {
            say!("(还没有落盘的日志)");
        }
        for day in days {
            say!("  {}", day.as_str().unwrap_or("-"));
        }
        return Ok(EXIT_OK);
    }

    let mut path = format!("/api/logs/{}", enc(target));
    let mut query: Vec<String> = Vec::new();
    if let Some(source) = args.get("source") {
        query.push(format!("source={}", enc(source)));
    }
    if let Some(scope) = args.get("scope") {
        query.push(format!("scope={}", enc(scope)));
    }
    if let Some(limit) = args.get("limit") {
        query.push(format!("limit={}", enc(limit)));
    }
    if !query.is_empty() {
        path = format!("{path}?{}", query.join("&"));
    }

    let body = server.json("GET", &path, None)?;
    if json {
        return print_json(&body);
    }
    say!(
        "{}: {} 条{}(返回 {} 条)",
        target,
        body["total"].as_u64().unwrap_or(0),
        if query.is_empty() { "" } else { "(已过滤)" },
        body["returned"].as_u64().unwrap_or(0)
    );
    // One log entry is written to both the instance scope and the job scope (design doc §10.1), so
    // the default view shows two lines; the scope column exposes this and --scope narrows it.
    for entry in body["entries"].as_array().cloned().unwrap_or_default() {
        say!(
            "{} [{:<5}] {:<8} {:<16} {}",
            short_time(entry["tsMs"].as_u64().unwrap_or(0)),
            entry["level"].as_str().unwrap_or("out"),
            entry["source"].as_str().unwrap_or("-"),
            entry["scope"].as_str().unwrap_or("-"),
            entry["text"].as_str().unwrap_or("")
        );
    }
    Ok(EXIT_OK)
}

/// HH:MM:SS only - the date is already in the header.
fn short_time(ts_ms: u64) -> String {
    let ms = ts_ms % 86_400_000;
    let seconds = ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// `cgw browse [absolute path]` - list subfolders on the daemon's machine (design doc §6.1).
fn cmd_browse(server: &Server, rest: &[String], json: bool) -> Result<u8, CliError> {
    let path = rest.first().map(String::as_str).unwrap_or("");
    let target = if path.is_empty() {
        "/api/browse".to_string()
    } else {
        format!("/api/browse?path={}", enc(path))
    };
    let body = server.json("GET", &target, None)?;
    if json {
        return print_json(&body);
    }
    kv("root", body["root"].as_str().unwrap_or("-"));
    kv("path", body["path"].as_str().unwrap_or("-"));
    if let Some(parent) = body["parent"].as_str() {
        kv("parent", parent);
    }
    say!();
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        say!("(没有子文件夹)");
    }
    for entry in entries {
        say!("  {}", entry["path"].as_str().unwrap_or("-"));
    }
    Ok(EXIT_OK)
}

fn project_of(server: &Server, project: &str) -> Result<serde_json::Value, CliError> {
    let body = server.json("GET", "/api/projects", None)?;
    body["projects"]
        .as_array()
        .and_then(|list| list.iter().find(|p| p["id"] == project))
        .cloned()
        .ok_or_else(|| CliError::new(EXIT_NOT_FOUND, format!("找不到项目 {project}")))
}



fn cmd_push(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    let project = require(rest, 0, "push <projectId> [--clean] [--keep-crlf] [--wait]")?;
    // Normalizes shebang scripts from CRLF to LF (Windows workspace -> Mac/Linux).
    // `--keep-crlf` is the escape hatch for people who want the bytes pushed untouched.
    let payload = serde_json::json!({
        "clean": args.has("clean"),
        "keepCrlf": args.has("keep-crlf"),
    });
    server.start_and_maybe_wait(
        args,
        &format!("/api/projects/{}/push", enc(project)),
        Some(&payload.to_string()),
        json,
    )
}

fn cmd_pull(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    let project = require(rest, 0, "pull <projectId> [--wait]")?;
    server.start_and_maybe_wait(
        args,
        &format!("/api/projects/{}/pull", enc(project)),
        None,
        json,
    )
}

fn cmd_jobs(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let _ = args;
    match rest.first().map(String::as_str) {
        Some("logs") => {
            let id = require(rest, 1, "jobs logs <jobId> [--since N]")?;
            let since = args
                .get("since")
                .and_then(|value| value.parse::<u64>().ok())
                .or_else(|| args.flag_value("--since").and_then(|v| v.parse().ok()))
                .unwrap_or(0);
            let body = server.json(
                "GET",
                &format!("/api/jobs/{}/logs?since={since}", enc(id)),
                None,
            )?;
            if json {
                return print_json(&body);
            }
            for entry in body["entries"].as_array().cloned().unwrap_or_default() {
                say!(
                    "[{}] {}",
                    entry["level"].as_str().unwrap_or("out"),
                    entry["text"].as_str().unwrap_or("")
                );
            }
            Ok(EXIT_OK)
        }
        Some("cancel") => {
            let id = require(rest, 1, "jobs cancel <jobId>")?;
            let body = server.json("POST", &format!("/api/jobs/{}/cancel", enc(id)), None)?;
            if json {
                return print_json(&body);
            }
            say!("已请求取消 {}", body["job"]["id"].as_str().unwrap_or(id));
            Ok(EXIT_OK)
        }
        Some("retry") => {
            let id = require(rest, 1, "jobs retry <jobId> [--from N] [--from-start]")?;
            let mut payload = json!({});
            if let Some(from) = args.flag_value("--from").and_then(|value| value.parse::<usize>().ok()) {
                payload["fromStep"] = json!(from);
            }
            if args.has("from-start") || args.flag_value("--from-start").is_some() {
                payload["fromStart"] = json!(true);
            }
            let body = server.json(
                "POST",
                &format!("/api/jobs/{}/retry", enc(id)),
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body);
            }
            say!(
                "新 jobId={}(从第 {} 步开始)",
                body["jobId"].as_str().unwrap_or("-"),
                body["retriedFrom"].as_u64().unwrap_or(0) + 1
            );
            Ok(EXIT_OK)
        }
        Some("clear") => {
            let project = args
                .get("project")
                .map(str::to_string)
                .or_else(|| args.flag_value("--project"))
                .or_else(|| rest.get(1).cloned());
            let url = match &project {
                Some(p) => format!("/api/jobs/clear?projectId={}", enc(p)),
                None => "/api/jobs/clear".to_string(),
            };
            let body = server.json("POST", &url, None)?;
            if json {
                return print_json(&body);
            }
            let pruned = body["pruned"].as_u64().unwrap_or(0);
            match &project {
                Some(p) => say!("已清空项目 {p} 的 {pruned} 条已完成执行记录"),
                None => say!("已清空 {pruned} 条已完成执行记录"),
            }
            Ok(EXIT_OK)
        }
        _ => {
            let body = server.json("GET", "/api/jobs", None)?;
            if json {
                return print_json(&body);
            }
            let list = body["jobs"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                say!("(还没有任务记录)");
            }
            for job in list {
                say!(
                    "{:<16} {:<7} {:<10} {:<6} {}",
                    job["id"].as_str().unwrap_or("-"),
                    job["kind"].as_str().unwrap_or("-"),
                    job["state"].as_str().unwrap_or("-"),
                    format!("{}s", job["elapsedSeconds"]),
                    job["message"].as_str().unwrap_or(""),
                );
            }
            Ok(EXIT_OK)
        }
    }
}

fn cmd_job(server: &Server, rest: &[String], json: bool) -> Result<u8, CliError> {
    let id = require(rest, 0, "job <jobId>")?;
    let body = server.json("GET", &format!("/api/jobs/{}", enc(id)), None)?;
    if json {
        return print_json(&body);
    }
    kv("id", body["id"].as_str().unwrap_or("-"));
    kv("kind", body["kind"].as_str().unwrap_or("-"));
    kv("state", body["state"].as_str().unwrap_or("-"));
    kv("title", body["title"].as_str().unwrap_or("-"));
    kv("elapsed", &format!("{}s", body["elapsedSeconds"]));
    kv("exitCode", &body["exitCode"].to_string());
    if body["artifactPath"].as_str().unwrap_or("").is_empty() {
        kv("artifact", "-");
    } else {
        kv("artifact", body["artifactPath"].as_str().unwrap_or("-"));
    }
    kv("message", body["message"].as_str().unwrap_or(""));

    // Batch workflow: list every step (design doc §7.3)
    let steps = body["steps"].as_array().cloned().unwrap_or_default();
    if steps.len() > 1 || body["kind"] == "batch" {
        say!();
        say!(
            "{:<4} {:<9} {:<10} {:<8} {}",
            "step", "taskId", "kind", "state", "detail"
        );
        for step in steps {
            let detail = if step["message"].as_str().unwrap_or("").is_empty() {
                step["outputs"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|item| item.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            } else {
                step["message"].as_str().unwrap_or("").to_string()
            };
            say!(
                "{:<4} {:<9} {:<10} {:<8} {}",
                step["index"].as_u64().unwrap_or(0) + 1,
                step["taskId"].as_str().unwrap_or("-"),
                step["kind"].as_str().unwrap_or("-"),
                step["state"].as_str().unwrap_or("-"),
                detail
            );
        }
    }

    if let Some(from) = body["retryFromStep"].as_u64() {
        say!();
        say!(
            "有步骤没成功:重试用 `cgw jobs retry {}`(从第 {} 步开始)",
            body["id"].as_str().unwrap_or(id),
            from + 1
        );
    }
    Ok(EXIT_OK)
}

/// `cgw schema <name>` - print one JSON Schema generated from the Rust structs (design doc §2).
///
/// Prints raw JSON by default (not only under `--json`): this output is for machines,
/// so an agent can save stdout as `.json` and hand it to a validator.
fn cmd_schema(server: &Server, rest: &[String], name: &str) -> Result<u8, CliError> {
    if rest.len() > 1 {
        return Err(CliError::usage(format!(
            "schema 一次只取一份(可用的:{})",
            SCHEMA_NAMES.join(" / ")
        )));
    }
    if !SCHEMA_NAMES.contains(&name) {
        return Err(CliError::usage(format!(
            "没有这份 schema:{name}(可用:{})",
            SCHEMA_NAMES.join(" / ")
        )));
    }
    let body = server.json("GET", &format!("/api/schema/{name}"), None)?;
    say!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
    Ok(EXIT_OK)
}

/// `cgw schema` (no name) - list the available names.
fn cmd_schema_list(server: &Server) -> Result<u8, CliError> {
    let body = server.json("GET", "/api/schema", None)?;
    for name in body["schemas"].as_array().cloned().unwrap_or_default() {
        say!("{}", name.as_str().unwrap_or("-"));
    }
    eprintln!("用 `cgw schema <名称>` 拿具体那一份。");
    Ok(EXIT_OK)
}

/// `cgw export <project> [--source remote|host] [--path P] [--name N] [--wait]`
/// - the Export shortcut of design doc §9 (runs one synthetic export task, not saved to the project).
fn cmd_export(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "export <projectId> [--source remote|host] [--path P] [--name N]")?;
    let mut payload = serde_json::Map::new();
    for (flag, key) in [("--source", "source"), ("--path", "path"), ("--name", "name")] {
        if let Some(value) = args.flag_value(flag) {
            payload.insert(key.to_string(), json!(value));
        }
    }
    server.start_and_maybe_wait(
        args,
        &format!("/api/projects/{}/export", enc(project)),
        Some(&serde_json::Value::Object(payload).to_string()),
        json,
    )
}

/// `cgw exports [list] <project>` / `cgw exports <project> <name> --save <path>`
/// - list / download the archives in `exports/`.
fn cmd_exports(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "exports <projectId> [名称 --save <路径>]")?;
    let name = rest.get(1).map(String::as_str);

    let Some(name) = name else {
        let body = server.json(
            "GET",
            &format!("/api/projects/{}/exports", enc(project)),
            None,
        )?;
        if json {
            return print_json(&body);
        }
        let items = body["exports"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            say!("(还没有导出过压缩包;`cgw export {project}` 可以现在导一个)");
        }
        for item in items {
            say!(
                "{:<44} {:>10}  {}",
                item["name"].as_str().unwrap_or("-"),
                human(item["bytes"].as_u64().unwrap_or(0)),
                crate::logfile::stamp_utc(item["modifiedAt"].as_u64().unwrap_or(0)),
            );
        }
        return Ok(EXIT_OK);
    };

    // Download: the only CLI path that takes **bytes** instead of JSON (so request_full, not json).
    let response = server.call(
        "GET",
        &format!("/api/projects/{}/exports/{}", enc(project), enc(name)),
        None,
    )?;
    if !response.ok() {
        return Err(api_error(&response));
    }

    let target = match args.flag_value("--save") {
        Some(dir) => {
            // A directory gets the archive under its own name; a file name is used as given
            let path = std::path::PathBuf::from(&dir);
            if path.is_dir() || dir.ends_with('/') || dir.ends_with('\\') {
                path.join(name)
            } else {
                path
            }
        }
        None => std::path::PathBuf::from(name),
    };
    std::fs::write(&target, &response.body_bytes)
        .map_err(|err| CliError::new(EXIT_USAGE, format!("写入 {} 失败:{err}", target.display())))?;
    say!(
        "已保存 {} 字节到 {}",
        response.body_bytes.len(),
        target.display()
    );
    Ok(EXIT_OK)
}

/// `cgw cat <project> <path> [--side remote|host]` - controlled peek at file contents
/// (the "controlled read API" of design doc §7.4). Binaries give metadata only; big files truncate with a note.
fn cmd_cat(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let project = require(rest, 0, "cat <projectId> <路径> [--side remote|host]")?;
    let path = require(rest, 1, "cat <projectId> <路径> [--side remote|host]")?;
    if path.trim().is_empty() {
        // An empty path folds into a 403 on the API side (not a usage error), so catch it here first
        return Err(CliError::usage("cat 需要一个文件路径(相对项目根)"));
    }
    let side = args.get("side").unwrap_or("remote");
    if side != "remote" && side != "host" {
        return fail(format!("--side 只能是 remote 或 host:{side}"));
    }

    let body = server.json(
        "GET",
        &format!(
            "/api/projects/{}/file?side={}&path={}",
            enc(project),
            enc(side),
            enc(path)
        ),
        None,
    )?;
    if json {
        return print_json(&body);
    }
    if body["binary"].as_bool() == Some(true) {
        say!(
            "(二进制文件,{} 字节,不显示内容;如需下载请在项目中创建 export 任务打包)",
            body["size"].as_u64().unwrap_or(0)
        );
        return Ok(EXIT_OK);
    }
    // Content goes to stdout as-is, so `cgw cat p x > local-file` works too
    say_raw!("{}", body["text"].as_str().unwrap_or(""));
    if !body["text"].as_str().unwrap_or("").ends_with('\n') {
        say!();
    }
    if body["truncated"].as_bool() == Some(true) {
        eprintln!(
            "(只显示了前 {} 字节,文件共 {} 字节;如需完整下载请使用 export 任务)",
            body["bytes"].as_u64().unwrap_or(0),
            body["size"].as_u64().unwrap_or(0)
        );
    }
    Ok(EXIT_OK)
}

/// `cgw workflows [list|show|save|rm|apply|refresh]` - cross-project workflow templates (design doc §14.3).
///
/// apply **previews only** by default (task list, batch id, conflicts); pass `--apply` to really write -
/// the "conflict preview" of §14.3 item 2: see what gets added and what it collides with.
fn cmd_workflows(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        None | Some("list") => {
            let body = server.json("GET", "/api/workflows", None)?;
            if json {
                return print_json(&body);
            }
            let list = body["workflows"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                say!("还没有工作流模板(目录:{})", body["dir"].as_str().unwrap_or("-"));
                say!("自带的那两份可以用 `cgw workflows refresh` 补回来。");
                return Ok(EXIT_OK);
            }
            say!("工作流模板  {}", body["dir"].as_str().unwrap_or("-"));
            for item in &list {
                say!(
                    "  {:<20} {:<28} {} 个任务 / {} 步{}",
                    item["id"].as_str().unwrap_or("-"),
                    item["name"].as_str().unwrap_or("-"),
                    item["taskCount"],
                    item["stepCount"],
                    if item["valid"].as_bool() == Some(true) { "" } else { "   [不合法]" }
                );
                if item["valid"].as_bool() != Some(true) {
                    say!(
                        "      {}",
                        item["errors"]
                            .as_array()
                            .map(|errors| errors
                                .iter()
                                .filter_map(|e| e.as_str())
                                .collect::<Vec<_>>()
                                .join("; "))
                            .unwrap_or_default()
                    );
                }
            }
            say!();
            say!("看一份:`cgw workflows show <id>`;用起来:`cgw workflows apply <project> <id> --set k=v --apply`");
            Ok(EXIT_OK)
        }
        Some("show") => {
            let id = require(rest, 1, "workflows show <id>")?;
            let body = server.json("GET", &format!("/api/workflows/{}/raw", enc(id)), None)?;
            if json {
                return print_json(&body);
            }
            say!("{}", body["raw"].as_str().unwrap_or("-"));
            if body["valid"].as_bool() != Some(true) {
                say!();
                say!(
                    "这份模板**不合法**:{}",
                    body["errors"]
                        .as_array()
                        .map(|errors| errors
                            .iter()
                            .filter_map(|e| e.as_str())
                            .collect::<Vec<_>>()
                            .join("; "))
                        .unwrap_or_default()
                );
            }
            Ok(EXIT_OK)
        }
        Some("save") => {
            let id = require(rest, 1, "workflows save <id> --body '<json>' | --file <路径>")?;
            let payload = args.json_argument(2)?;
            let body = server.json(
                "PUT",
                &format!("/api/workflows/{}", enc(id)),
                Some(&serde_json::json!({ "raw": payload.to_string() }).to_string()),
            )?;
            if json {
                return print_json(&body);
            }
            let saved = &body["workflow"];
            say!(
                "已保存工作流模板 {} ({}:{})",
                id,
                saved["name"].as_str().unwrap_or("-"),
                saved["stepCount"]
            );
            Ok(EXIT_OK)
        }
        Some("rm") | Some("delete") => {
            let id = require(rest, 1, "workflows rm <id>")?;
            let body = server.json("DELETE", &format!("/api/workflows/{}", enc(id)), None)?;
            if json {
                return print_json(&body);
            }
            say!("已删除工作流模板 {id}(已经 apply 到项目的那些不受影响)");
            Ok(EXIT_OK)
        }
        Some("refresh") => {
            let body = server.json("POST", "/api/workflows/reset", Some("{}"))?;
            if json {
                return print_json(&body);
            }
            let restored = body["restored"].as_array().cloned().unwrap_or_default();
            if restored.is_empty() {
                say!("自带的模板一份都不缺(已有的文件一律不碰)");
            } else {
                say!(
                    "补齐了:{}",
                    restored
                        .iter()
                        .filter_map(|id| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(EXIT_OK)
        }
        Some("apply") => {
            let usage = "workflows apply <projectId> <模板 id> [--set k=v ...] [--apply] [--overwrite] [--run] [--name N]";
            let project = require(rest, 1, usage)?;
            let id = require(rest, 2, usage)?;
            let body = serde_json::json!({
                "variables": workflow_variables(args)?,
                "apply": args.has("apply"),
                "overwrite": args.has("overwrite"),
                "run": args.has("run"),
                "name": args.get("name").unwrap_or(""),
            });
            let result = server.json(
                "POST",
                &format!(
                    "/api/projects/{}/workflows/{}/apply",
                    enc(project),
                    enc(id)
                ),
                Some(&body.to_string()),
            )?;
            if json {
                return print_json(&result);
            }
            let preview = &result["preview"];
            say!(
                "模板 {} -> 项目 {}",
                id,
                project
            );
            say!(
                "  批量工作流  {} ({})",
                preview["batchId"].as_str().unwrap_or("-"),
                preview["batchName"].as_str().unwrap_or("-")
            );
            for task in preview["tasks"].as_array().cloned().unwrap_or_default() {
                say!(
                    "  任务        {} [{}] {}",
                    task["id"].as_str().unwrap_or("-"),
                    task["kind"].as_str().unwrap_or("-"),
                    task["name"].as_str().unwrap_or("-")
                );
            }
            let conflicts = result["conflicts"].as_array().cloned().unwrap_or_default();
            if !conflicts.is_empty() {
                say!();
                say!(
                    "  冲突        {}(要覆盖得加 --overwrite)",
                    conflicts
                        .iter()
                        .filter_map(|item| item.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                );
            }
            if result["applied"].as_bool() == Some(true) {
                say!();
                say!("已写入项目。");
                if let Some(job) = result["jobId"].as_str() {
                    say!("已开始执行:job {job}(用 `cgw job {job}` 看进度)");
                } else {
                    say!("执行:`cgw batch run {project} {}`", preview["batchId"].as_str().unwrap_or("-"));
                }
            } else {
                say!();
                say!("以上是**预览**,什么都没写。确认无误后加 --apply 再来一次。");
            }
            Ok(EXIT_OK)
        }
        Some(other) => fail(format!(
            "未知的 workflows 子命令:{other}(支持 list / show / save / rm / apply / refresh)"
        )),
    }
}

/// Collect template variables from `--set k=v` (repeatable).
fn workflow_variables(args: &Args) -> Result<serde_json::Map<String, serde_json::Value>, CliError> {
    let mut map = serde_json::Map::new();
    let raw = args.raw.as_slice();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == "--set" || raw[index] == "--var" {
            let pair = raw.get(index + 1).ok_or_else(|| {
                CliError::usage("--set 要写成 key=value,例如 --set manifest=xcode".to_string())
            })?;
            let (key, value) = pair.split_once('=').ok_or_else(|| {
                CliError::usage(format!("--set 要写成 key=value,这里是:{pair}"))
            })?;
            map.insert(key.trim().to_string(), json!(value));
            index += 1;
        }
        index += 1;
    }
    Ok(map)
}

fn cmd_manifests(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    // Also accept the switch-style `cgw manifests --status` (habit, or a forgotten subcommand):
    // treat `--xxx` as the matching subcommand.
    let flag_alias = rest.first().map(String::as_str).unwrap_or("");
    let rest: Vec<String> = if flag_alias.is_empty() {
        for (flag, name) in [
            ("status", "status"),
            ("import", "import"),
            ("export", "export"),
            ("delete", "delete"),
            ("refresh", "refresh"),
        ] {
            if args.has(flag) || args.get(flag).is_some() {
                let mut shifted = vec![name.to_string()];
                // `--import x.json` / `--export x.json`: carry the value along too
                if let Some(value) = args.get(flag) {
                    shifted.push(value.to_string());
                }
                shifted.extend(rest.iter().cloned());
                return cmd_manifests(server, args, &shifted, json);
            }
        }
        rest.to_vec()
    } else {
        rest.to_vec()
    };
    let rest: &[String] = &rest;

    match rest.first().map(String::as_str) {
        // ---- status: dir location, count, invalid ones, missing shipped templates ------------
        Some("status") => {
            let list = server.json("GET", "/api/manifests", None)?;
            let templates = server.json("GET", "/api/manifests/templates", None)?;
            let manifests = list["manifests"].as_array().cloned().unwrap_or_default();
            let invalid: Vec<&str> = manifests
                .iter()
                .filter(|item| item["valid"].as_bool() != Some(true))
                .filter_map(|item| item["id"].as_str())
                .collect();
            let shipped: Vec<&str> = manifests
                .iter()
                .filter(|item| item["template"].as_bool() == Some(true))
                .filter_map(|item| item["id"].as_str())
                .collect();
            if json {
                return print_json(&serde_json::json!({
                    "dir": list["dir"],
                    "total": manifests.len(),
                    "invalid": invalid,
                    "templates": shipped,
                    "missingTemplates": templates["missing"],
                    "manifests": manifests,
                }));
            }
            say!("构建类型目录  {}", list["dir"].as_str().unwrap_or("-"));
            say!(
                "共 {} 份,其中自带模板 {} 份{}",
                manifests.len(),
                shipped.len(),
                if invalid.is_empty() {
                    String::new()
                } else {
                    format!(",**{} 份不合法**({})", invalid.len(), invalid.join(", "))
                }
            );
            let missing = templates["missing"].as_array().cloned().unwrap_or_default();
            if missing.is_empty() {
                say!("自带的模板一份不缺。");
            } else {
                say!("缺 {} 份自带模板:", missing.len());
                for item in missing {
                    say!(
                        "  {:<26} {}",
                        item["id"].as_str().unwrap_or("-"),
                        item["displayName"].as_str().unwrap_or("-")
                    );
                }
                say!("补齐:`cgw manifests reset`");
            }
            Ok(EXIT_OK)
        }
        // ---- export: write a local file (no --save prints to stdout) ------------------
        Some("export") => {
            let id = require(rest, 1, "manifests export <id> [--save <路径>]")?;
            let body = server.json("GET", &format!("/api/manifests/{}/raw", enc(id)), None)?;
            let raw = body["raw"].as_str().unwrap_or_default().to_string();
            match args.get("save").or_else(|| args.get("file")) {
                Some(target) => {
                    let target = std::path::PathBuf::from(target);
                    if let Some(dir) = target.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                        std::fs::create_dir_all(dir)
                            .map_err(|err| format!("建目录失败 {}:{err}", dir.display()))?;
                    }
                    std::fs::write(&target, raw.as_bytes())
                        .map_err(|err| format!("写文件失败 {}:{err}", target.display()))?;
                    if json {
                        return print_json(&serde_json::json!({
                            "ok": true, "id": id, "saved": crate::paths::display(&target),
                        }));
                    }
                    say!("已导出 {id} -> {}", crate::paths::display(&target));
                }
                None => {
                    if json {
                        return print_json(&body);
                    }
                    say_raw!("{raw}");
                    if !raw.ends_with('\n') {
                        say!();
                    }
                }
            }
            Ok(EXIT_OK)
        }
        // ---- import: the file name is the id (unless --id is given) -----------------------------
        Some("import") => {
            let file = require(rest, 1, "manifests import <文件.json> [--id <名称>] [--force]")?;
            let path = std::path::PathBuf::from(file);
            let raw = std::fs::read_to_string(&path)
                .map_err(|err| format!("读不到 {}:{err}", path.display()))?;
            let id = match args.get("id") {
                Some(id) => id.to_string(),
                None => path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .filter(|stem| !stem.is_empty())
                    .ok_or_else(|| format!("从文件名推不出 id,用 --id 指定:{}", path.display()))?,
            };
            // Same rule as `manifests save`: an existing id needs --force so scripts cannot clobber it
            if !args.has("force") {
                let list = server.json("GET", "/api/manifests", None)?;
                if list["manifests"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["id"] == id))
                {
                    return Err(CliError::new(
                        EXIT_USAGE,
                        format!("{id} 已经存在。确认要覆盖就加 --force(原内容不再保留)。"),
                    ));
                }
            }
            let payload = serde_json::json!({ "raw": raw });
            let body = server.json(
                "PUT",
                &format!("/api/manifests/{}", enc(&id)),
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body);
            }
            say!("已导入 {} -> {id}", crate::paths::display(&path));
            if body["manifest"]["valid"].as_bool() != Some(true) {
                say!("注意:这份 manifest 校验没过 —— `cgw manifests show {id}` 看问题。");
            }
            Ok(EXIT_OK)
        }
        None | Some("list") => {
            let body = server.json("GET", "/api/manifests", None)?;
            if json {
                return print_json(&body);
            }
            for manifest in body["manifests"].as_array().cloned().unwrap_or_default() {
                say!(
                    "{:<26} {:<12} steps={:<3} vars={:<3} {}{}",
                    manifest["id"].as_str().unwrap_or("-"),
                    manifest["platform"].as_str().unwrap_or("-"),
                    manifest["build"]["stepCount"],
                    manifest["variables"].as_array().map(|v| v.len()).unwrap_or(0),
                    manifest["displayName"].as_str().unwrap_or(""),
                    if manifest["valid"].as_bool().unwrap_or(true) {
                        String::new()
                    } else {
                        "  [INVALID]".to_string()
                    },
                );
            }
            Ok(EXIT_OK)
        }
        Some("show") => {
            let id = require(rest, 1, "manifests show <id>")?;
            let body = server.json("GET", &format!("/api/manifests/{}/raw", enc(id)), None)?;
            if json {
                return print_json(&body);
            }
            say!("{}", body["raw"].as_str().unwrap_or(""));
            Ok(EXIT_OK)
        }
        Some("schema") => cmd_schema(server, rest, "manifest"),
        Some("presets") | Some("templates") => {
            let body = server.json("GET", "/api/manifests/templates", None)?;
            if json {
                return print_json(&body);
            }
            let missing = body["missing"].as_array().cloned().unwrap_or_default();
            if missing.is_empty() {
                say!("自带的模板一份不缺。");
            }
            for preset in missing {
                say!(
                    "  {:<26} {}",
                    preset["id"].as_str().unwrap_or("-"),
                    preset["displayName"].as_str().unwrap_or("-")
                );
            }
            Ok(EXIT_OK)
        }
        Some("save") => {
            let id = require(rest, 1, "manifests save <id> --file <路径> | - [--force]")?;

            // Design doc §6.4: a name clash must be confirmed first. The CLI cannot prompt, so `--force`
            // says "I know it overwrites, go on" - blocked by default, so scripts cannot silently clobber one.
            if !args.has("force") {
                let list = server.json("GET", "/api/manifests", None)?;
                if list["manifests"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["id"] == id))
                {
                    return Err(CliError::new(
                        EXIT_USAGE,
                        format!(
                            "{id} 已经存在。确认要覆盖就加 --force(原内容不再保留)。"
                        ),
                    ));
                }
            }

            // `--file -` reads stdin - handy for an agent piping a manifest straight in
            let raw = match args.flag_value("--file").as_deref() {
                Some("-") => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|err| format!("读标准输入失败:{err}"))?;
                    buffer
                }
                Some(file) => std::fs::read_to_string(file)
                    .map_err(|err| format!("读不到 {file}:{err}"))?,
                None => {
                    return Err(CliError::usage(
                        "用法:manifests save <id> --file <路径>(或 --file - 从标准输入读)",
                    ))
                }
            };
            let payload = serde_json::json!({ "raw": raw });
            let body = server.json(
                "PUT",
                &format!("/api/manifests/{}", enc(id)),
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body);
            }
            say!("已保存 {id}(valid={})", body["manifest"]["valid"]);
            Ok(EXIT_OK)
        }
        Some("rm") | Some("delete") => {
            let id = require(rest, 1, "manifests rm <id>")?;            server.json("DELETE", &format!("/api/manifests/{}", enc(id)), None)?;
            say!("已删除 {id}");
            Ok(EXIT_OK)
        }
        Some("reset") => {
            let ids: Vec<String> = rest[1..].to_vec();
            let payload = serde_json::json!({ "ids": ids });
            let body = server.json("POST", "/api/manifests/reset", Some(&payload.to_string()))?;
            if json {
                return print_json(&body);
            }
            let restored: Vec<&str> = body["restored"]
                .as_array()
                .map(|list| list.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            if restored.is_empty() {
                // Design doc §6.4: reset writes back **only** presets missing on disk, never comparing content.
                // When it writes nothing, say why, or the command looks broken.
                say!("没有需要补齐的模板。");
                say!("(补齐只写回**不存在**的模板文件,不碰你已经改过的那份;想还原原版,先 `cgw manifests rm <id>` 再补齐。)");
            } else {
                say!("已恢复:{}", restored.join(", "));
            }
            Ok(EXIT_OK)
        }
        // refresh: the json files in the manifest dir are **yours**; you may add/edit/delete them outside,
        // invisible to the daemon - this command re-reads and reports what changed.
        Some("refresh") | Some("reload") => {
            let body = server.json("POST", "/api/manifests/reload", Some("{}"))?;
            if json {
                return print_json(&body);
            }
            say!(
                "构建类型已刷新({}):{} 份,{}",
                body["dir"].as_str().unwrap_or("-"),
                body["total"].as_u64().unwrap_or(0),
                body["summary"].as_str().unwrap_or("没有变化")
            );
            if body["created"].as_bool() == Some(true) {
                say!(
                    "目录不存在,已重新生成并复制了 {} 份自带模板。",
                    body["seeded"].as_array().map(|v| v.len()).unwrap_or(0)
                );
            }
            let invalid = body["invalid"].as_u64().unwrap_or(0);
            if invalid > 0 {
                say!("有 {invalid} 份不合法(`cgw manifests list` 里标着 [INVALID])。");
            }
            Ok(EXIT_OK)
        }
        Some("detect") => {
            let dir = require(rest, 1, "manifests detect <宿主机目录>")?;
            let payload = serde_json::json!({ "sourcePath": dir });
            let body = server.json(
                "POST",
                "/api/manifests/detect",
                Some(&payload.to_string()),
            )?;
            if json {
                return print_json(&body);
            }
            let matches = body["matches"].as_array().cloned().unwrap_or_default();
            if matches.is_empty() {
                say!("没有命中任何构建类型");
            }
            for hit in matches {
                say!(
                    "{:<26} {}",
                    hit["id"].as_str().unwrap_or("-"),
                    hit["displayName"].as_str().unwrap_or("")
                );
            }
            Ok(EXIT_OK)
        }
        Some(other) => fail(format!("未知子命令:manifests {other}")),
    }
}

fn cmd_config(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    match rest.first().map(String::as_str) {
        None | Some("show") => {
            let body = server.json("GET", "/api/config", None)?;
            if json {
                return print_json(&body);
            }
            kv("port", &body["port"].to_string());
            kv("listen", body["listen"].as_str().unwrap_or("-"));
            kv("language", body["language"].as_str().unwrap_or("-"));
            kv("username", body["username"].as_str().unwrap_or(""));
            kv("password", if body["hasPassword"].as_bool() == Some(true) { "已设置" } else { "未设置" });
            kv(
                "login interval",
                &format!("{} 秒", body["loginIntervalSeconds"].as_u64().unwrap_or(60)),
            );
            kv("config", body["configPath"].as_str().unwrap_or("-"));
            // Allowed host roots: shared by browse / project registration / manifest detection
            let roots: Vec<&str> = body["hostRoots"]
                .as_array()
                .map(|list| list.iter().filter_map(|value| value.as_str()).collect())
                .unwrap_or_default();
            kv("host roots", &roots.join("; "));
            let mcp = &body["mcp"];
            kv(
                "mcp",
                &if mcp["running"].as_bool() == Some(true) {
                    format!("开 · {}", mcp["endpoint"].as_str().unwrap_or("-"))
                } else if mcp["enabled"].as_bool() == Some(true) {
                    "开(没在监听 —— 端口可能起来了但没绑上)".to_string()
                } else {
                    "关(配置页里能开)".to_string()
                },
            );
            Ok(EXIT_OK)
        }
        Some("set") => {
            // Only explicitly given fields enter the patch - omitted ones keep their value.
            let mut patch = serde_json::Map::new();
            if let Some(port) = args.get("port") {
                let port: u16 = port
                    .parse()
                    .map_err(|_| format!("--port 必须是 0..65535 的整数:{port}"))?;
                patch.insert("port".to_string(), json!(port));
            }
            if let Some(listen) = args.get("listen") {
                if listen != "lan" && listen != "wan" {
                    return fail(format!("--listen 只能是 lan 或 wan:{listen}"));
                }
                patch.insert("listen".to_string(), json!(listen));
            }
            if let Some(language) = args.get("language") {
                patch.insert("language".to_string(), json!(language));
            }
            if let Some(seconds) = args.get("login-interval") {
                let seconds: u64 = seconds
                    .parse()
                    .map_err(|_| format!("--login-interval 必须是非负整数:{seconds}"))?;
                patch.insert("loginIntervalSeconds".to_string(), json!(seconds));
            }
            if let Some(days) = args.get("log-retain-days") {
                let days: u64 = days
                    .parse()
                    .map_err(|_| format!("--log-retain-days 必须是非负整数:{days}"))?;
                patch.insert("logRetainDays".to_string(), json!(days));
            }
            if let Some(username) = args.get("username") {
                patch.insert("username".to_string(), json!(username));
            }
            if let Some(password) = args.get("password") {
                patch.insert("password".to_string(), json!(password));
            }
            // Host root allowlist: shared by browse / project registration / manifest detection (design doc §6.1).
            // Separated by `;` - Windows paths contain `:` but no `;`.
            if let Some(raw) = args.get("allowed-roots") {
                let roots: Vec<String> = raw
                    .split(';')
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect();
                patch.insert("allowedRoots".to_string(), json!(roots));
            }
            // ---- MCP adapter (takes effect immediately, no restart) -------------------------------
            let mut mcp = serde_json::Map::new();
            if let Some(enabled) = args.get("mcp-enabled") {
                mcp.insert("enabled".to_string(), json!(parse_bool(enabled)?));
            }
            if let Some(raw) = args.get("mcp-listen") {
                if raw != "lan" && raw != "wan" {
                    return fail(format!("--mcp-listen 只能是 lan 或 wan:{raw}"));
                }
                mcp.insert("listen".to_string(), json!(raw));
            }
            if let Some(raw) = args.get("mcp-port") {
                let port: u16 = raw
                    .parse()
                    .map_err(|_| format!("--mcp-port 必须是 0..65535 的整数:{raw}"))?;
                mcp.insert("port".to_string(), json!(port));
            }
            if !mcp.is_empty() {
                patch.insert("mcp".to_string(), serde_json::Value::Object(mcp));
            }
            if patch.is_empty() {
                return fail("config set 至少要给一个参数(--port/--listen/--language/--login-interval/--username/--password/--mcp-enabled/--mcp-port/--mcp-listen)".to_string());
            }

            let body = serde_json::Value::Object(patch).to_string();
            let result = server.json("POST", "/api/config", Some(&body))?;
            if json {
                return print_json(&result);
            }
            if result["restartRequired"].as_bool() == Some(true) {
                say!("已保存。端口/监听模式的改动需要重启程序才生效(MCP 那几项不用)。");
            } else {
                say!("已保存。");
            }
            Ok(EXIT_OK)
        }
        Some(other) => fail(format!("未知子命令:config {other}(只支持 show / set)")),
    }
}

/// `cgw mcp` - the MCP adapter built into the daemon (design doc §12 Phase 6).
///
/// MCP **runs inside the daemon process on another port**, so what appears here is its group of
/// `config.json` settings (enable / listen / port). Unlike the daemon's own port, changes to these
/// **take effect immediately**, with no restart - every write below prints the new state.
fn cmd_mcp(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    /// Patch the mcp section of `config.json`, then print the resulting state as-is.
    fn patch(
        server: &Server,
        patch: serde_json::Value,
        json: bool,
    ) -> Result<u8, CliError> {
        let body = json!({ "mcp": patch }).to_string();
        let result = server.json("POST", "/api/config", Some(&body))?;
        if json {
            return print_json(&result["config"]["mcp"]);
        }
        let mcp = &result["config"]["mcp"];
        if mcp["running"].as_bool() == Some(true) {
            say!(
                "MCP 已经按新配置起来了:{} 个工具,端点是 POST {}/mcp",
                mcp["tools"].as_u64().unwrap_or(0),
                mcp["url"].as_str().unwrap_or("-")
            );
        } else {
            say!("MCP 现在是关的(mcp.enabled = false)。");
        }
        Ok(EXIT_OK)
    }

    match rest.first().map(String::as_str) {
        None | Some("status") => {
            let body = server.json("GET", "/api/mcp", None)?;
            if json {
                return print_json(&body);
            }
            kv("enabled", if body["enabled"].as_bool() == Some(true) { "true" } else { "false" });
            kv("running", if body["running"].as_bool() == Some(true) { "true" } else { "false" });
            kv("listen", body["listen"].as_str().unwrap_or("-"));
            kv("port", &body["port"].to_string());
            let url = body["url"].as_str().unwrap_or("");
            kv("endpoint", if url.is_empty() { "-" } else { body["endpoint"].as_str().unwrap_or("-") });
            kv(
                "healthz",
                &if url.is_empty() { "-".to_string() } else { format!("{url}/healthz") },
            );
            kv("tools", &body["tools"].as_u64().unwrap_or(0).to_string());
            kv(
                "protocols",
                &body["protocolVersions"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .filter_map(|item| item.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default(),
            );
            if body["warning"].as_str() == Some("mcpWanWithoutAuth") {
                kv("warning", "监听在 wan 但没有设置访问凭据,建议设密码或改回 lan");
            }
            Ok(EXIT_OK)
        }
        Some("url") => {
            let body = server.json("GET", "/api/mcp", None)?;
            let endpoint = body["endpoint"].as_str().unwrap_or("");
            if json {
                return print_json(&body);
            }
            if endpoint.is_empty() {
                return fail(
                    "MCP 现在没在监听(mcp.enabled = false,或者端口没绑上)。用 `cgw mcp on` 打开。"
                        .to_string(),
                );
            }
            say!("{endpoint}");
            Ok(EXIT_OK)
        }
        Some("tools") => {
            let body = server.json("GET", "/api/mcp/tools", None)?;
            if json {
                return print_json(&body);
            }
            let tools = body["tools"].as_array().cloned().unwrap_or_default();
            if let Some(name) = rest.get(1) {
                // With a name: print that tool's full argument schema (handy when debugging tools)
                let Some(tool) = tools.iter().find(|tool| tool["name"] == json!(name)) else {
                    return fail(format!("没有这个工具:{name}"));
                };
                say!("{}", tool["name"].as_str().unwrap_or("-"));
                say!();
                say!("{}", tool["description"].as_str().unwrap_or(""));
                say!();
                say!(
                    "{}",
                    serde_json::to_string_pretty(&tool["inputSchema"]).unwrap_or_default()
                );
                return Ok(EXIT_OK);
            }
            say!(
                "{} 个工具(和 agent 通过 MCP 看到的是同一份):",
                body["count"].as_u64().unwrap_or(0)
            );
            for tool in &tools {
                let description = tool["description"].as_str().unwrap_or("");
                let short: String = description.chars().take(48).collect();
                say!(
                    "  {:<18} {}{}",
                    tool["name"].as_str().unwrap_or("-"),
                    short,
                    if description.chars().count() > 48 { "…" } else { "" }
                );
            }
            Ok(EXIT_OK)
        }
        Some("on") | Some("enable") => patch(server, json!({ "enabled": true }), json),
        Some("off") | Some("disable") => patch(server, json!({ "enabled": false }), json),
        Some("port") => {
            let raw = args
                .get("port")
                .or_else(|| rest.get(1).map(String::as_str))
                .ok_or_else(|| "用法:cgw mcp port <N>(0 = 自动挑)".to_string())?;
            let port: u16 = raw
                .parse()
                .map_err(|_| format!("端口必须是 0..65535 的整数:{raw}"))?;
            patch(server, json!({ "port": port }), json)
        }
        Some("listen") => {
            let raw = args
                .get("listen")
                .or_else(|| rest.get(1).map(String::as_str))
                .ok_or_else(|| "用法:cgw mcp listen lan|wan".to_string())?;
            if raw != "lan" && raw != "wan" {
                return fail(format!("只能是 lan 或 wan:{raw}"));
            }
            patch(server, json!({ "listen": raw }), json)
        }
        Some(other) => fail(format!(
            "未知子命令:mcp {other}(支持 status / url / tools / on / off / port / listen)"
        )),
    }
}

/// `cgw api <METHOD> <PATH> [JSON]` - the fallback entry point: it can call any endpoint.
/// With it, "anything the WebUI can do the CLI can do" holds structurally - even an endpoint that
/// has no first-class command yet can be called here. With `--json` it prints the raw body for pipes.
fn cmd_api(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    let method = require(rest, 0, "api <METHOD> <PATH> [JSON]")?.to_ascii_uppercase();
    let path = require(rest, 1, "api <METHOD> <PATH> [JSON]")?;
    if !path.starts_with('/') {
        return fail("PATH 必须以 / 开头".to_string());
    }
    let inline = rest.get(2).map(String::as_str);
    let body = match inline {
        Some(text) => Some(text.to_string()),
        None => {
            // Accept `--body` / `--file` / `--file -`, matching the task and batch spellings
            let has_body = args.flag_value("--body").is_some() || args.flag_value("--file").is_some();
            if has_body {
                Some(args.json_argument(2)?.to_string())
            } else {
                None
            }
        }
    };
    let _ = args;

    let response = server.call(&method, path, body.as_deref())?;
    if json {
        // Emit raw output, error bodies included, so `cgw api` can feed downstream tools
        say!("{}", response.body.trim());
    } else {
        say!("{method} {path} -> HTTP {}", response.status);
        if !response.body.trim().is_empty() {
            say!("{}", httpclient::pretty(response.body.trim()));
        }
    }
    Ok(if response.ok() {
        EXIT_OK
    } else {
        code_for_status(response.status, "")
    })
}

// ------------------------------------------------------------------ session

/// `cgw login [--username U] [--password P]` - trade for a session cookie and save it.
///
/// Loopback access needs no login; it matters only for a remote daemon or a non-loopback origin.
/// CI / agents should prefer `CGW_USERNAME` / `CGW_PASSWORD` (leaves no state on disk).
fn cmd_login(
    server: &Server,
    args: &Args,
    rest: &[String],
    json: bool,
) -> Result<u8, CliError> {
    let username = args
        .get("username")
        .map(str::to_string)
        .or_else(|| std::env::var("CGW_USERNAME").ok())
        .or_else(|| rest.first().cloned())
        .ok_or_else(|| CliError::usage("需要用户名:--username U,或 $CGW_USERNAME,或位置参数"))?;
    let password = args
        .get("password")
        .map(str::to_string)
        .or_else(|| std::env::var("CGW_PASSWORD").ok())
        .ok_or_else(|| CliError::usage("需要密码:--password P 或 $CGW_PASSWORD"))?;

    let payload = json!({ "username": username, "password": password }).to_string();
    let response = server.call("POST", "/api/login", Some(&payload))?;
    if !response.ok() {
        return Err(api_error(&response));
    }

    // The cookie comes from Set-Cookie; only a non-loopback daemon really needs it
    let cookie = response
        .header("set-cookie")
        .and_then(|value| value.split(';').next())
        .map(str::to_string);

    match cookie {
        Some(cookie) => {
            save_session(&server.host, Some(&cookie))?;
            server.set_session(Some(cookie.clone()));
            if json {
                say!("{}", json!({ "ok": true, "host": server.host, "saved": true }));
            } else {
                say!("已登录 {} 并保存会话。", server.host);
            }
        }
        None => {
            // No cookie means this daemon needs no auth (loopback), so logging in is pointless
            if json {
                say!("{}", json!({ "ok": true, "host": server.host, "saved": false }));
            } else {
                say!("{} 不需要登录(本机访问免鉴权)。", server.host);
            }
        }
    }
    Ok(EXIT_OK)
}

/// `cgw logout` - make the daemon forget this session and delete the local copy.
fn cmd_logout(server: &Server, json: bool) -> Result<u8, CliError> {
    let _ = server.call("POST", "/api/logout", None);
    save_session(&server.host, None)?;
    server.set_session(None);
    if json {
        say!("{}", json!({ "ok": true, "host": server.host }));
    } else {
        say!("已退出 {}。", server.host);
    }
    Ok(EXIT_OK)
}

/// `cgw doctor` - one command that shows whether work can happen right now.
///
/// For agents and troubleshooting: daemon up, login needed, client/project counts, warnings.
/// No failing step aborts the run; each is reported as it is.
fn cmd_doctor(server: &Server, json: bool) -> Result<u8, CliError> {
    let mut checks: Vec<(String, bool, String)> = Vec::new();

    let status = match server.json("GET", "/api/status", None) {
        Ok(status) => {
            checks.push((
                "daemon".to_string(),
                true,
                format!(
                    "{} {} @ {}",
                    status["app"].as_str().unwrap_or("?"),
                    status["version"].as_str().unwrap_or("?"),
                    status["url"].as_str().unwrap_or("-")
                ),
            ));
            Some(status)
        }
        Err(error) => {
            checks.push(("daemon".to_string(), false, error.message.clone()));
            None
        }
    };

    if status.is_some() {
        // Session / auth: reading /api/clients proves the path works
        match server.json("GET", "/api/clients", None) {
            Ok(body) => {
                let count = body["clients"].as_array().map(Vec::len).unwrap_or(0);
                checks.push(("auth".to_string(), true, format!("{count} 台客户机")));
            }
            Err(error) => checks.push(("auth".to_string(), false, error.message)),
        }
        match server.json("GET", "/api/projects", None) {
            Ok(body) => {
                let count = body["projects"].as_array().map(Vec::len).unwrap_or(0);
                let running = body["running"].as_object().map(|m| m.len()).unwrap_or(0);
                checks.push((
                    "projects".to_string(),
                    true,
                    format!("{count} 个项目,{running} 个正在跑"),
                ));
            }
            Err(error) => checks.push(("projects".to_string(), false, error.message)),
        }
        match server.json("GET", "/api/manifests", None) {
            Ok(body) => {
                let list = body["manifests"].as_array().cloned().unwrap_or_default();
                let invalid = list
                    .iter()
                    .filter(|item| item["valid"].as_bool() != Some(true))
                    .count();
                checks.push((
                    "manifests".to_string(),
                    invalid == 0,
                    format!("{} 份,{} 份校验不通过", list.len(), invalid),
                ));
            }
            Err(error) => checks.push(("manifests".to_string(), false, error.message)),
        }
        match server.json("GET", "/api/logs/days", None) {
            Ok(body) => checks.push((
                "logs".to_string(),
                body["failed"].as_bool() != Some(true),
                format!(
                    "{} 天,保留 {} 天",
                    body["days"].as_array().map(Vec::len).unwrap_or(0),
                    body["retainDays"].as_u64().unwrap_or(0)
                ),
            )),
            Err(error) => checks.push(("logs".to_string(), false, error.message)),
        }
    }

    let warnings: Vec<String> = status
        .as_ref()
        .and_then(|status| status["warning"].as_str())
        .map(|warning| vec![warning.to_string()])
        .unwrap_or_default();

    if json {
        say!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": checks.iter().all(|(_, ok, _)| *ok),
                "host": server.host,
                "checks": checks.iter().map(|(name, ok, detail)| json!({
                    "name": name, "ok": ok, "detail": detail
                })).collect::<Vec<_>>(),
                "warnings": warnings,
            }))
            .unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }

    for (name, ok, detail) in &checks {
        say!("{} {:<10} {detail}", if *ok { "OK  " } else { "FAIL" }, name);
    }
    for warning in &warnings {
        say!("WARN {warning}(wan 模式下没设访问凭据)");
    }
    Ok(if checks.iter().all(|(_, ok, _)| *ok) {
        EXIT_OK
    } else {
        EXIT_API
    })
}

/// `cgw basedir <clientId> <path> [--no-verify]` - set the base directory.
///
/// By default it really connects, runs `mkdir -p` and returns the absolute path (same as the WebUI);
/// `--no-verify` only saves without verifying.
fn cmd_basedir(server: &Server, args: &Args, rest: &[String], json: bool) -> Result<u8, CliError> {
    let id = require(rest, 0, "basedir <clientId> <路径> [--no-verify]")?;
    let dir = require(rest, 1, "basedir <clientId> <路径> [--no-verify]")?;
    let verify = !args.has("no-verify");

    let payload = json!({ "baseDir": dir, "verify": verify }).to_string();
    let body = server.json(
        "POST",
        &format!("/api/clients/{}/basedir", enc(id)),
        Some(&payload),
    )?;
    if json {
        return print_json(&body);
    }
    kv("ok", &body["ok"].to_string());
    kv("message", body["message"].as_str().unwrap_or(""));
    if let Some(resolved) = body["client"]["baseDirResolved"].as_str() {
        if !resolved.is_empty() {
            kv("resolved", resolved);
        }
    }
    Ok(if body["ok"].as_bool() == Some(true) {
        EXIT_OK
    } else {
        EXIT_API
    })
}

// ------------------------------------------------------------------ helpers

/// Shorthand for a usage error.
///
/// `Err(String)` does not become a `CliError` on its own (only `?` applies the `From` conversion), so
/// route every usage error through here and pin that meaning onto the exit code.
fn fail<T>(message: String) -> Result<T, CliError> {
    Err(CliError::usage(message))
}

fn kv(key: &str, value: &str) {    say!("{key:<16}: {value}");
}

/// Accepts `true/false/on/off/yes/no/1/0` - all of these spellings get typed on a command line.
fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" | "y" => Ok(true),
        "0" | "false" | "off" | "no" | "n" => Ok(false),
        other => Err(format!("要 true/false(on/off 也行),给的是:{other}")),
    }
}

fn count(value: &serde_json::Value) -> String {
    value
        .as_array()
        .map(|list| list.len())
        .unwrap_or(0)
        .to_string()
}

fn require<'a>(rest: &'a [String], index: usize, usage: &str) -> Result<&'a str, String> {
    rest.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("参数不够,用法:{usage}"))
}

fn print_json(value: &serde_json::Value) -> Result<u8, CliError> {
    say!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
    Ok(EXIT_OK)
}

/// Minimal escaping of the characters a path segment can contain.
fn enc(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch.to_string()
            } else {
                format!("%{:02X}", ch as u32)
            }
        })
        .collect()
}

// ------------------------------------------------------------------ help (single source)

/// Help entry point: `cgw help [path...]`, `cgw <command> -h`, `cgw help --webui`, `cgw help --json`.
///
/// Table and rendering live in `crate::help` (library) - the same data also feeds the MCP `cgw://cli`
/// resource, which a binary-local table could not reach.
fn print_help_for(args: &Args) -> Result<u8, CliError> {
    if args.has("json") {
        say!("{}", crate::help::json());
        return Ok(EXIT_OK);
    }
    if args.has("webui") {
        say!("{}", crate::help::webui_table());
        return Ok(EXIT_OK);
    }

    match crate::help::resolve(&args.positional) {
        crate::help::Resolved::Tree(text)
        | crate::help::Resolved::Command(text)
        | crate::help::Resolved::Group(text) => {
            say!("{text}");
            Ok(EXIT_OK)
        }
        crate::help::Resolved::Unknown { path, hints } => {
            let msg = match crate::i18n::current() {
                crate::i18n::Lang::ZhHans => {
                    format!("没有这条命令:{path}。这一层有:{}", hints.join(" / "))
                }
                crate::i18n::Lang::EnUS => {
                    format!("Unknown command: {path}. Available at this level: {}", hints.join(" / "))
                }
            };
            Err(CliError::usage(msg))
        }
    }
}


// ------------------------------------------------------------------ terminal shell

/// `cgw -shell` - start a CLI-style interactive shell in the current terminal.
///
/// Why: the app's own interactive form is CLI-style - scrolling log above, prompt below -
/// and the graphical part is just the WebUI. Running in PowerShell needs no extra window:
/// the current terminal gives that experience, with the same code on Linux / macOS (pure
/// stdin/stdout, no Windows console API).
///
/// Behavior:
///   * find a running daemon first; otherwise **start one** (`compilegateway-daemon` sits alongside),
///     and shut it down on exit if this shell started it;
///   * every typed line reuses the plain `cgw` dispatch, so `clients` / `projects` /
///     `run --wait` / `jobs` / `tree` ... all work straight from the shell;
///   * the daemon's main log scope scrolls above the prompt (incremental, no repeats).
fn shell(args: &Args) -> Result<u8, CliError> {
    use std::io::{BufRead, Write};

    let (server, child) = attach_or_start(args)?;
    let mut child = child;

    let status = server.json("GET", "/api/status", None).ok();
    // If the daemon is unreachable, do not start a shell that can do nothing
    if status.is_none() {
        if let Some(mut child) = child.take() {
            let _ = child.kill();
        }
        return Err(CliError::new(
            EXIT_TRANSPORT,
            format!("连不上 daemon({}):检查地址,或用 `--url` 指定一个", server.host),
        ));
    }
    let url = status
        .as_ref()
        .and_then(|status| status["url"].as_str())
        .unwrap_or("-")
        .to_string();

    say!("{APP_NAME} {VERSION} —— 终端 shell");
    say!("daemon: {url}   (输入 help 看命令,exit 退出)");
    if let Some(status) = &status {
        say!(
            "listen={}  language={}  auth={}",
            status["listen"].as_str().unwrap_or("-"),
            status["language"].as_str().unwrap_or("-"),
            if status["auth"]["required"].as_bool() == Some(true) {
                "已设置"
            } else {
                "未设置"
            }
        );
    }
    say!();

    // Dump the existing startup log first so the user does not face an empty screen
    let mut cursor = flush_logs(&server, 0, true);

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut exit_code = EXIT_OK;

    loop {
        say_raw!("cgw> ");
        let _ = std::io::stdout().flush();

        let line = match lines.next() {
            Some(Ok(line)) => line,
            // EOF (Ctrl+Z Enter / Ctrl+D) counts as exit
            _ => break,
        };
        let tokens = match tokenize(&line) {
            Ok(tokens) => tokens,
            Err(message) => {
                say!("{message}");
                continue;
            }
        };
        if tokens.is_empty() {
            continue;
        }

        let command = tokens[0].as_str();
        match command {
            "exit" | "quit" => break,
            // Every shell line uses the same help (drill-down)
            "help" | "?" => {
                let _ = print_help_for(&args);
            }
            "clear" | "cls" => {
                // ANSI clear screen + home cursor; PowerShell and common terminals accept it
                say_raw!("\x1b[2J\x1b[H");
                let _ = std::io::stdout().flush();
            }
            "webui" | "open" => {
                match status.as_ref().and_then(|status| status["url"].as_str()) {
                    Some(url) => say!("{url}"),
                    None => say!("daemon 没有报出地址"),
                }
            }
            "logs" => {
                // In the shell `logs` reads the main scope by default (the WebUI log page is per client)
                let scope = args_from(&tokens).get("scope").map(str::to_string);
                let follow = tokens.iter().any(|token| token == "--follow" || token == "-f");
                if follow {
                    say!("(盯着 main 日志域,-follow 会一直刷;Ctrl+C 结束)");
                    loop {
                        cursor = flush_logs(&server, cursor, false);
                        std::thread::sleep(std::time::Duration::from_millis(700));
                    }
                }
                match scope {
                    Some(scope) => {
                        let body = server.json(
                            "GET",
                            &format!("/api/logs/tail?scope={}&limit=200", enc(&scope)),
                            None,
                        )?;
                        for entry in body["entries"].as_array().cloned().unwrap_or_default() {
                            say!(
                                "{} [{:<5}] {}",
                                short_time(entry["ts_ms"].as_u64().unwrap_or(0)),
                                entry["level"].as_str().unwrap_or("out"),
                                entry["text"].as_str().unwrap_or("")
                            );
                        }
                    }
                    None => cursor = flush_logs(&server, 0, true),
                }
            }
            _ => {
                let parsed = args_from(&tokens);
                match dispatch(&server, &parsed) {
                    Ok(code) => {
                        if code != EXIT_OK {
                            exit_code = code;
                        }
                    }
                    // An error must not kill the shell - report it and continue (exit codes stay for exit)
                    Err(error) => {
                        say!("{}", error.message);
                        exit_code = error.code;
                    }
                }
            }
        }

        // After each command, show new log lines: command echo plus log
        cursor = flush_logs(&server, cursor, false);
    }

    if let Some(mut child) = child.take() {
        say!("正在关闭由本 shell 启动的 daemon…");
        let _ = child.kill();
        let _ = child.wait();
    }
    say!("再见。");
    Ok(exit_code)
}

/// Split a shell line into tokens, honouring single and double quotes (paths with spaces).
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;

    for ch in line.chars() {
        match quote {
            Some(mark) => {
                if ch == mark {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '"' | '\'' => {
                    quote = Some(ch);
                    started = true;
                }
                // `#` to end of line is a comment - handy for jotting notes in the shell
                '#' if !started && current.is_empty() => break,
                ch if ch.is_whitespace() => {
                    if started || !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                other => current.push(other),
            },
        }
    }

    if let Some(mark) = quote {
        return Err(format!("引号 {mark} 没有闭合"));
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn args_from(tokens: &[String]) -> Args {
    Args::parse(tokens)
}

/// Incrementally fetch and print the main log scope. Returns the new cursor (highest seq seen).
fn flush_logs(server: &Server, since: u64, first: bool) -> u64 {
    let path = if since == 0 && first {
        // The first pass dumps only the latest screenful
        "/api/logs/tail?scope=main&limit=25".to_string()
    } else {
        format!("/api/logs/tail?scope=main&since={since}")
    };
    let Ok(body) = server.json("GET", &path, None) else {
        return since;
    };
    let mut cursor = since;
    for entry in body["entries"].as_array().cloned().unwrap_or_default() {
        cursor = cursor.max(entry["seq"].as_u64().unwrap_or(0));
        say!(
            "{} [{:<5}] {}",
            short_time(entry["ts_ms"].as_u64().unwrap_or(0)),
            entry["level"].as_str().unwrap_or("out"),
            entry["text"].as_str().unwrap_or("")
        );
    }
    let _ = cursor;
    cursor
}

/// Find a usable daemon; start one when there is none.
///
/// Returns `(server, child started by us)` - when the child is `Some`, shell exit kills it.
fn attach_or_start(args: &Args) -> Result<(Server, Option<std::process::Child>), CliError> {
    // An explicit address is used as given: no guessing, no auto-start
    if args.get("url").is_some() || std::env::var("CGW_URL").is_ok() {
        let server = Server::resolve(args)?;
        return Ok((server, None));
    }
    if let Ok(server) = Server::resolve(args) {
        if server.json("GET", "/api/status", None).is_ok() {
            return Ok((server, None));
        }
    }

    // Nothing found: try to start the daemon sitting alongside
    let exe = daemon_binary().ok_or_else(|| {
        CliError::new(
            EXIT_TRANSPORT,
            "没有正在跑的 daemon,而且旁边找不到 compilegateway-daemon。\
             请先启动 daemon,或者用 --url http://host:port 指定一个。",
        )
    })?;

    // Redirect the child's output to a file, or it would overwrite the shell prompt
    let log_path = exe.with_file_name("daemon-console.log");
    let stdout = std::fs::File::create(&log_path).ok();
    let stderr = stdout.as_ref().and_then(|file| file.try_clone().ok());

    let mut command = std::process::Command::new(&exe);
    if let Some(file) = stdout {
        command.stdout(std::process::Stdio::from(file));
    }
    if let Some(file) = stderr {
        command.stderr(std::process::Stdio::from(file));
    }
    let child = command.spawn().map_err(|err| {
        CliError::new(
            EXIT_TRANSPORT,
            format!("启动 {} 失败:{err}", exe.display()),
        )
    })?;

    say!(
        "没有正在跑的 daemon,已在后台启动 {} (它的控制台输出写到 {})",
        exe.file_name().unwrap_or_default().to_string_lossy(),
        log_path.display()
    );

    // Wait until it writes runtime.json and /api/status answers
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Ok(server) = Server::resolve(args) {
            if server.json("GET", "/api/status", None).is_ok() {
                return Ok((server, Some(child)));
            }
        }
    }

    Err(CliError::new(
        EXIT_TRANSPORT,
        "daemon 起来了但一直没就绪;看看旁边的 daemon-console.log",
    ))
}

/// The daemon executable next to us (tried with the platform suffix).
fn daemon_binary() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))?;
    let names = if cfg!(windows) {
        ["compilegateway-daemon.exe", "compilegateway.exe"]
    } else {
        ["compilegateway-daemon", "compilegateway"]
    };
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}