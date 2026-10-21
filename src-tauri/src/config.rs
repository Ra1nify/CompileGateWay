//! Locating, reading, saving and defaulting `config.json`.
//!
//! Lookup order:
//!   1. next to the exe (config.json) -- the shipped layout
//!   2. the current working directory -- handy for `cargo run`
//! If neither exists, a default config is written next to the exe.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "config.json";

/// The explanatory header written into a freshly created `config.json`.
///
/// Language-aware: the user opens this file to edit it, so it follows `language` like any
/// other user-visible text. `auto` cannot be honored here (the file does not exist yet), so
/// it falls back to the system locale via [`crate::i18n::current`].
pub fn comment(lang: crate::i18n::Lang) -> String {
    match lang {
        crate::i18n::Lang::ZhHans => "port: 0 = 自动挑选一个空闲端口,也可以写死端口号(被占用时报错,不重试);\
language: auto | zh-Hans | en-US;\
listen: lan = 只有本机能访问(localhost/127.0.0.1),wan = 允许其他设备用 本机IP:端口 访问;\
auth: 可选访问凭据。lan 与 wan 都不强制要求,但 wan 模式下留空会一直有安全警告;\
auth.loginIntervalSeconds: 同一来源 IP 两次登录尝试之间的最小间隔秒数,0 = 不限流,上限 86400;\
logRetainDays: 日志保留天数(按天滚动,文件名就是日期),默认 7,上限 3650;\
allowedRoots: 除了用户主目录之外还允许操作的宿主目录(数组);\
  browse、登记项目、探测构建类型共用这一套检查 ——\
  源码放在别的盘上(比如 F:\\CODEX)时把它加进来,加一次三处一起生效;\
mcp: 内置的 MCP 适配层(给 AI agent 用),跑在**另一个端口**上;\
  enabled: 是否随 daemon 一起起(默认 true);\
  listen: lan = 只听 127.0.0.1,wan = 允许其他设备访问(wan 且没设凭据时会一直警告);\
  port: 这个端口是**独立定义**的,和上面的 port 没有任何推导关系(不是主端口 + 1);\
    写死的端口 = 必须绑上,被占用就报错(和上面的 port 一个口径);\
    留空或 0 = 先试默认端口 3994,被占用就请系统分配一个空闲端口(实际端口见 runtime.json);\
  改这一组**立即生效**,不需要重启;\
hostLock: true = 同一台客户机一次只跑一个 job(主机级互斥,默认 false)。\
  不同项目本来可以并行,但同一台 Mac 上的 Xcode / JDK / 设备是共享资源;\
  打开之后别的项目在这台机器上跑着时会返回 hostBusy(排队则自动等),立即生效。"
            .to_string(),
        crate::i18n::Lang::EnUS => "port: 0 = auto-pick a free port; a fixed port must bind (an in-use port errors out, no retry);\
language: auto | zh-Hans | en-US;\
listen: lan = this machine only (localhost/127.0.0.1), wan = other devices may reach it at <local ip>:<port>;\
auth: optional access credentials. Neither lan nor wan requires them, but leaving them empty under wan keeps a security warning;\
auth.loginIntervalSeconds: minimum seconds between two login attempts from the same source IP; 0 = no throttling, max 86400;\
logRetainDays: log retention days (rolling by day; the file name is the date), default 7, max 3650;\
allowedRoots: host directories allowed in addition to the user home directory (array);\
  browse, project registration and build-type detection share this one check --\
  add a drive here (say F:\\CODEX) when sources live on it, and all three accept it at once;\
mcp: the built-in MCP adapter (for AI agents), served on **a separate port**;\
  enabled: start it alongside the daemon (default true);\
  listen: lan = 127.0.0.1 only, wan = other devices may connect (a permanent warning while wan has no credentials);\
  port: this port is **defined independently** and is not derived from `port` above (it is not main port + 1);\
    a fixed port must bind; an in-use one errors out (same rule as `port` above);\
    empty or 0 = try the default port 3994 first, then let the OS assign a free one (the actual port is in runtime.json);\
  this group takes effect **immediately**, no restart needed;\
hostLock: true = one job at a time per client machine (host-level mutex, default false).\
  Different projects could otherwise run in parallel, but Xcode / JDK / devices on one Mac are shared resources;\
  when enabled, another project running on that machine returns hostBusy (queued tasks simply wait). Takes effect immediately."
            .to_string(),
    }
}

// ------------------------------------------------------------------ Listen mode

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Listen {
    /// Listen on 127.0.0.1 only: reachable from this machine.
    Lan,
    /// Listen on 0.0.0.0: other devices can reach it at `host-ip:port`.
    Wan,
}

impl Default for Listen {
    fn default() -> Self {
        Listen::Lan
    }
}

impl Listen {
    pub fn code(self) -> &'static str {
        match self {
            Listen::Lan => "lan",
            Listen::Wan => "wan",
        }
    }

    pub fn parse(value: &str) -> Option<Listen> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lan" => Some(Listen::Lan),
            "wan" => Some(Listen::Wan),
            _ => None,
        }
    }

    /// Bind address: lan -> 127.0.0.1, wan -> 0.0.0.0
    pub fn bind_ip(self) -> Ipv4Addr {
        match self {
            Listen::Lan => Ipv4Addr::LOCALHOST,
            Listen::Wan => Ipv4Addr::UNSPECIFIED,
        }
    }

    pub fn allows_remote(self) -> bool {
        matches!(self, Listen::Wan)
    }
}

// ------------------------------------------------------------------ Credentials

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Auth {
    pub username: String,
    /// argon2 hash, never sent back to clients.
    pub password_hash: String,
    /// Minimum seconds between login attempts from one IP (design doc §4.3). 0 = unlimited.
    pub login_interval_seconds: u64,
}

impl Default for Auth {
    fn default() -> Self {
        Auth {
            username: String::new(),
            password_hash: String::new(),
            login_interval_seconds: crate::auth::DEFAULT_LOGIN_INTERVAL.as_secs(),
        }
    }
}

impl Auth {
    pub fn is_configured(&self) -> bool {
        !self.username.trim().is_empty() && !self.password_hash.is_empty()
    }

    /// Effective rate-limit interval, clamped to the allowed range.
    pub fn login_interval(&self) -> std::time::Duration {
        crate::auth::login_interval(self.login_interval_seconds)
    }
}

// ------------------------------------------------------------------ MCP adapter

/// Built-in MCP adapter settings: its own port, independent of the WebUI port.
/// Defaults to `enabled: true` + `listen: lan` + `port: 0` (auto-pick), so LAN exposure needs wan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
/// Unset or 0: try [`crate::mcp::DEFAULT_PORT`], then let the system pick a free one.
/// A hard-coded port must bind, otherwise it errors (same rule as the daemon's `port`).
pub struct Mcp {
    pub enabled: bool,
    pub listen: Listen,
    /// **Separately defined** port, unrelated to `Config::port` (not "main port + 1").
    pub port: Option<u16>,
}

impl Default for Mcp {
    fn default() -> Self {
        Mcp {
            enabled: true,
            listen: Listen::Lan,
            port: None,
        }
    }
}

impl Mcp {
    pub fn requested_port(&self) -> Option<u16> {
        self.port.filter(|p| *p != 0)
    }
}

// ------------------------------------------------------------------ Config

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Unset or 0: pick any free port automatically.
    pub port: Option<u16>,
    /// auto / zh-Hans / en-US
    pub language: String,
    pub listen: Listen,
    pub auth: Auth,
    /// Days of logs to keep (design doc §10; default 7, max 3650).
    pub log_retain_days: u64,
    /// Built-in MCP adapter (for AI agents).
    pub mcp: Mcp,
    /// Host directories allowed **in addition to** the user home (design doc §6.1).
    /// browse, project registration and build-type detection share this check, so add a
    /// directory such as `F:\CODEX` once and all three accept it.
    pub allowed_roots: Vec<String>,
    /// **Host-level mutex** (end of design doc §7.6.6): one job at a time per client.
    ///
    /// Projects could otherwise run in parallel, but Xcode / JDK / devices on one Mac are
    /// shared and two colliding builds fail in confusing ways. Off by default; when on,
    /// another project gets `hostBusy` while one runs, unless it queues.
    pub host_lock: bool,
    /// Only to document the generated config.json; the program never reads it.
    #[serde(rename = "_comment")]
    pub comment: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: None,
            language: "auto".to_string(),
            listen: Listen::Lan,
            auth: Auth::default(),
            log_retain_days: crate::logfile::DEFAULT_RETAIN_DAYS,
            mcp: Mcp::default(),
            allowed_roots: Vec::new(),
            host_lock: false,
            comment: comment(crate::i18n::current()),
        }
    }
}

impl Config {
    /// Port explicitly set in the config file (0 counts as unset).
    pub fn requested_port(&self) -> Option<u16> {
        self.port.filter(|p| *p != 0)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text + "\n")
    }
}

// ------------------------------------------------------------------ Loading

pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub created: bool,
}

pub enum LoadError {
    NoConfigDir,
    Read { path: PathBuf, err: std::io::Error },
    Parse { path: PathBuf, err: String },
    Write { path: PathBuf, err: std::io::Error },
}

pub fn load() -> Result<Loaded, LoadError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));

    let mut candidate = None;
    if let Some(dir) = &exe_dir {
        let p = dir.join(FILE_NAME);
        if p.is_file() {
            candidate = Some(p);
        }
    }
    if candidate.is_none() {
        let p = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(FILE_NAME);
        if p.is_file() {
            candidate = Some(p);
        }
    }

    match candidate {
        Some(path) => {
            let config = read_config(&path)?;
            Ok(Loaded {
                config,
                path,
                created: false,
            })
        }
        None => {
            let dir = exe_dir.ok_or(LoadError::NoConfigDir)?;
            let path = dir.join(FILE_NAME);
            let config = Config::default();
            config.save_to(&path).map_err(|err| LoadError::Write {
                path: path.clone(),
                err,
            })?;
            Ok(Loaded {
                config,
                path,
                created: true,
            })
        }
    }
}

pub fn read_config(path: &Path) -> Result<Config, LoadError> {
    let raw = std::fs::read_to_string(path).map_err(|err| LoadError::Read {
        path: path.to_path_buf(),
        err,
    })?;
    serde_json::from_str(&raw).map_err(|err| LoadError::Parse {
        path: path.to_path_buf(),
        err: err.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Old config files lack loginIntervalSeconds: fall back to 60s, not 0 (0 disables the limit).
    #[test]
    fn auth_defaults_login_interval_when_field_missing() {
        let config: Config = serde_json::from_str(r#"{"auth":{"username":"u","passwordHash":"h"}}"#)
            .expect("failed to parse");
        assert_eq!(config.auth.login_interval_seconds, 60);
        assert_eq!(config.auth.login_interval().as_secs(), 60);
    }

    #[test]
    fn login_interval_is_clamped() {
        let mut auth = Auth::default();
        assert_eq!(auth.login_interval().as_secs(), 60);

        auth.login_interval_seconds = 0;
        assert!(auth.login_interval().is_zero());

        auth.login_interval_seconds = 5;
        assert_eq!(auth.login_interval().as_secs(), 5);

        // Clamp to 24 hours so a huge value in the config cannot lock anyone out.
        auth.login_interval_seconds = u64::MAX;
        assert_eq!(auth.login_interval(), crate::auth::MAX_LOGIN_INTERVAL);
    }

    /// Old config files lack logRetainDays: fall back to 7, not 0 (0 clamps to 1 day).
    #[test]
    fn log_retain_days_defaults_to_seven_when_missing() {
        let config: Config = serde_json::from_str(r#"{"port":65251}"#).expect("failed to parse");
        assert_eq!(config.log_retain_days, crate::logfile::DEFAULT_RETAIN_DAYS);
        assert_eq!(config.log_retain_days, 7);
    }

    /// Fields other than the credential pair must not affect is_configured.
    #[test]
    fn auth_configured_requires_both_halves() {        let mut auth = Auth::default();
        assert!(!auth.is_configured());
        auth.username = "u".to_string();
        assert!(!auth.is_configured());
        auth.password_hash = "h".to_string();
        assert!(auth.is_configured());
    }
}
