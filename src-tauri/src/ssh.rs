//! SSH connections over pure-Rust russh (no system ssh / libssh).
//!
//! `Session` is a connected, authenticated session reusable for many commands (`sync` uses it);
//! `run` / `probe` / `ensure_base_dir` are one-shot wrappers. Host keys use TOFU: trust and
//! record the first fingerprint, then reject any change.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Config, Handle, Handler};
use russh::keys::{load_secret_key, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;

use crate::clients::{AuthMethod, Client};

const PROBE_COMMAND: &str = "echo CGW_OK";

/// Total budget for connect + handshake + auth; without it a peer that accepts TCP but never
/// completes the handshake hangs forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to send a keepalive while idle. See the note in [`Session::open_with_state`].
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// What a single connection observed for the host key.
#[derive(Default, Clone)]
pub struct HostKeyState {
    /// Fingerprint the peer actually presented this time.
    seen: Option<String>,
    /// Public key line the peer presented (`ssh-ed25519 AAAA...`).
    seen_line: Option<String>,
    /// Differs from the recorded value (connection rejected).
    mismatch: bool,
}

struct ProbeHandler {
    expected: String,
    state: Arc<Mutex<HostKeyState>>,
}

impl Handler for ProbeHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let (fingerprint, line) = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => (
                key.fingerprint(HashAlg::Sha256).to_string(),
                key.to_openssh().ok(),
            ),
            // Host certificates are rare; treat them as "server changed identity".
            PublicKeyOrCertificate::Certificate(_) => ("certificate".to_string(), None),
        };

        let mut state = crate::lock::lock(&self.state);
        state.seen = Some(fingerprint.clone());
        state.seen_line = line;

        // Nothing recorded yet -> trust on first use; otherwise it must match.
        if !self.expected.is_empty() && self.expected != fingerprint {
            state.mismatch = true;
            return Ok(false);
        }
        Ok(true)
    }
}

// ------------------------------------------------------------------ cancellation

/// Cooperative cancel flag: read wherever the executor can check, and closing the channel
/// once set.
#[derive(Clone, Default)]
pub struct Cancel(Arc<std::sync::atomic::AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    pub fn from_flag(flag: Arc<std::sync::atomic::AtomicBool>) -> Cancel {
        Cancel(flag)
    }

    pub fn flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.0.clone()
    }

    pub fn requested(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

// ------------------------------------------------------------------ session

pub struct Exec {
    /// Remote `ExitStatus`. `None` means it never arrived (channel closed early, connection
    /// dropped) and is **not** success — the old `u32` default of 0 hid truncated commands
    /// (acceptance record 2026-09-28 §4, item 3).
    pub exit: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Cooperative cancel hit: the command was cut short (the remote process may run on,
    /// depending on sshd).
    pub cancelled: bool,
}

impl Exec {
    pub fn ok(&self) -> bool {
        self.exit == Some(0) && !self.cancelled
    }

    /// Display exit code: `-1` when missing (same convention as cancelled, never pretend 0).
    pub fn code(&self) -> i32 {
        self.exit.map_or(-1, |code| code as i32)
    }

    /// Short failure reason for error messages: a missing status must say the channel closed,
    /// not report a fake "exit code 0".
    pub fn exit_note(&self) -> String {
        match self.exit {
            Some(code) => format!("退出码 {code}"),
            None => "通道提前关闭,没有拿到退出状态".to_string(),
        }
    }

    /// Merge stdout / stderr text, used to show the user why a command failed.
    pub fn combined(&self) -> String {
        let out = String::from_utf8_lossy(&self.stdout).trim().to_string();
        let err = String::from_utf8_lossy(&self.stderr).trim().to_string();
        match (out.is_empty(), err.is_empty()) {
            (false, false) => format!("{out}\n{err}"),
            (false, true) => out,
            (true, false) => err,
            (true, true) => String::new(),
        }
    }
}

pub struct Session {
    handle: Handle<ProbeHandler>,
    username: String,
    host_key: Option<String>,
    host_key_line: Option<String>,
}

impl Session {
    /// Connect + authenticate; returns user-facing error text on failure. Bounded by
    /// [`CONNECT_TIMEOUT`] — a peer that accepts TCP but stalls the handshake would hang forever.
    pub async fn open(client: &Client) -> Result<Session, String> {
        match tokio::time::timeout(CONNECT_TIMEOUT, Self::open_inner(client)).await {
            Ok(result) => result,
            Err(_) => Err(format!(
                "连接或认证超时({}s),对端可能没有完成 SSH 握手",
                CONNECT_TIMEOUT.as_secs()
            )),
        }
    }

    /// Same, but cancellable: a set flag aborts the connect immediately.
    pub async fn open_cancellable(client: &Client, cancel: &Cancel) -> Result<Session, String> {
        tokio::select! {
            result = Self::open(client) => result,
            _ = wait_cancel(cancel) => Err("已取消".to_string()),
        }
    }

    async fn open_inner(client: &Client) -> Result<Session, String> {
        Self::open_reported(client).await.0
    }

    /// Like [`Session::open`], but also returns the host fingerprint the peer presented.
    ///
    /// For probe-style callers (test connection): the UI needs the structured value to offer
    /// "trust this new fingerprint" instead of parsing the error string.
    pub async fn open_reported(client: &Client) -> (Result<Session, String>, HostKeyState) {
        let state = Arc::new(Mutex::new(HostKeyState::default()));
        let result = Self::open_with_state(client, state.clone()).await;
        let snapshot = crate::lock::lock(&state).clone();
        (result, snapshot)
    }

    async fn open_with_state(
        client: &Client,
        state: Arc<Mutex<HostKeyState>>,
    ) -> Result<Session, String> {
        if client.host.trim().is_empty() {
            return Err("没有填写 IP 地址".to_string());
        }

        let handler = ProbeHandler {
            expected: client.host_key.clone(),
            state: state.clone(),
        };

        // Periodic keepalive instead of an inactivity timeout: a long silent build
        // (Kotlin/Native linking, Gradle config) must not be killed for 60s of quiet — the
        // server must answer keepalives, and russh's keepalive_max (default 3) still drops a
        // dead peer. Each operation's own `tokio::time::timeout` is the real deadline.
        let config = Config {
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            inactivity_timeout: None,
            ..Config::default()
        };

        let handle = client::connect(
            Arc::new(config),
            (client.host.as_str(), client.port),
            handler,
        )
        .await
        .map_err(|err| format!("无法连接 {}:{} - {err}", client.host, client.port))?;

        let (host_key, host_key_line, mismatch) = {
            let state = crate::lock::lock(&state);
            (state.seen.clone(), state.seen_line.clone(), state.mismatch)
        };
        if mismatch {
            return Err(format!(
                "主机公钥指纹与上次记录的不一致,已拒绝连接。上次:{} 本次:{}",
                client.host_key,
                host_key.clone().unwrap_or_default()
            ));
        }

        let username = if client.auth.username.trim().is_empty() {
            default_username()
        } else {
            client.auth.username.clone()
        };
        if username.is_empty() {
            return Err("没有填写用户名".to_string());
        }

        let mut session = Session {
            handle,
            username: username.clone(),
            host_key,
            host_key_line,
        };

        let auth = match client.auth.method {
            AuthMethod::Password => {
                if client.auth.password.is_empty() {
                    return Err("没有填写密码".to_string());
                }
                session
                    .handle
                    .authenticate_password(username.clone(), client.auth.password.clone())
                    .await
            }
            AuthMethod::Key => {
                let key = load_private_key(
                    &client.auth.key_path,
                    &client.auth.passphrase,
                )?;
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), Some(HashAlg::Sha256));
                session
                    .handle
                    .authenticate_publickey(username.clone(), key)
                    .await
            }
        };

        match auth {
            Ok(AuthResult::Success) => Ok(session),
            Ok(_) => Err(format!("认证失败:服务器拒绝了用户 {username} 的凭据")),
            Err(err) => Err(format!("认证出错:{err}")),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn host_key(&self) -> Option<&str> {
        self.host_key.as_deref()
    }

    /// Host public key line seen this connection (used when exporting `known_hosts`).
    pub fn host_key_line(&self) -> Option<&str> {
        self.host_key_line.as_deref()
    }

    pub async fn exec(&mut self, command: &str) -> Result<Exec, String> {
        self.exec_inner(command, None, &Cancel::default()).await
    }

    /// Run a command, invoking the callback **per line** as output arrives (stdout and stderr
    /// merged into one line stream), so build logs stream instead of arriving at the end.
    pub async fn exec_stream<F>(&mut self, command: &str, on_line: F) -> Result<Exec, String>
    where
        F: FnMut(&str),
    {
        self.exec_stream_cancellable(command, &Cancel::default(), on_line)
            .await
    }

    /// Same, but cancellable: a set flag cuts the channel and returns early.
    pub async fn exec_stream_cancellable<F>(
        &mut self,
        command: &str,
        cancel: &Cancel,
        mut on_line: F,
    ) -> Result<Exec, String>
    where
        F: FnMut(&str),
    {
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|err| format!("打开会话失败:{err}"))?;
        channel
            .exec(true, command)
            .await
            .map_err(|err| format!("执行远端命令失败:{err}"))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit: Option<u32> = None;
        let mut pending = String::new();
        let mut cancelled = false;

        loop {
            if cancel.requested() {
                cancelled = true;
                break;
            }
            // `Channel::wait()` is a cancel-safe mpsc recv; the timer branch only exists so a
            // command with no output still reacts promptly to cancellation.
            let next = tokio::select! {
                msg = channel.wait() => msg,
                _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => continue,
            };
            let Some(msg) = next else { break };

            match msg {
                ChannelMsg::Data { data } => {
                    stdout.extend_from_slice(&data);
                    pending.push_str(&String::from_utf8_lossy(&data));
                    flush_lines(&mut pending, &mut on_line);
                }
                ChannelMsg::ExtendedData { data, .. } => {
                    stderr.extend_from_slice(&data);
                    pending.push_str(&String::from_utf8_lossy(&data));
                    flush_lines(&mut pending, &mut on_line);
                }
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                _ => {}
            }
        }

        let tail = pending.trim_end().to_string();
        if !tail.is_empty() {
            on_line(&tail);
        }
        if cancelled {
            let _ = channel.close().await;
        }

        Ok(Exec {
            exit,
            stdout,
            stderr,
            cancelled,
        })
    }

    /// Run a command with `input` written to its stdin (used when pushing archives).
    pub async fn exec_stdin(&mut self, command: &str, input: &[u8]) -> Result<Exec, String> {
        self.exec_inner(command, Some(input), &Cancel::default()).await
    }

    /// Same, but cancellable: stdin is written in chunks and the flag is checked between them,
    /// so a large archive can be cut off mid-transfer.
    pub async fn exec_stdin_cancellable(
        &mut self,
        command: &str,
        input: &[u8],
        cancel: &Cancel,
    ) -> Result<Exec, String> {
        self.exec_inner(command, Some(input), cancel).await
    }

    /// Collecting exec (used when pulling archives), also cancellable.
    pub async fn exec_collect_cancellable(
        &mut self,
        command: &str,
        cancel: &Cancel,
    ) -> Result<Exec, String> {
        self.exec_inner(command, None, cancel).await
    }

    async fn exec_inner(
        &mut self,
        command: &str,
        input: Option<&[u8]>,
        cancel: &Cancel,
    ) -> Result<Exec, String> {
        if cancel.requested() {
            return Ok(Exec {
                exit: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                cancelled: true,
            });
        }

        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|err| format!("打开会话失败:{err}"))?;
        channel
            .exec(true, command)
            .await
            .map_err(|err| format!("执行远端命令失败:{err}"))?;

        if let Some(bytes) = input {
            const CHUNK: usize = 256 * 1024;
            let mut offset = 0;
            while offset < bytes.len() {
                if cancel.requested() {
                    let _ = channel.close().await;
                    return Ok(Exec {
                        exit: None,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                        cancelled: true,
                    });
                }
                let end = (offset + CHUNK).min(bytes.len());
                let mut rest = &bytes[offset..end];
                channel
                    .data(&mut rest)
                    .await
                    .map_err(|err| format!("发送数据失败:{err}"))?;
                offset = end;
            }
            channel
                .eof()
                .await
                .map_err(|err| format!("关闭远端输入失败:{err}"))?;
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit: Option<u32> = None;
        let mut cancelled = false;
        loop {
            if cancel.requested() {
                cancelled = true;
                break;
            }
            let next = tokio::select! {
                msg = channel.wait() => msg,
                _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => continue,
            };
            let Some(msg) = next else { break };
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                _ => {}
            }
        }
        if cancelled {
            let _ = channel.close().await;
        }

        Ok(Exec {
            exit,
            stdout,
            stderr,
            cancelled,
        })
    }
}

// ------------------------------------------------------------------ one-shot wrappers

/// Host key seen in one connection: fingerprint plus the public key line.
///
/// Both are needed: the fingerprint for TOFU comparison, the line for **export** — a
/// fingerprint is a one-way digest and cannot rebuild the base64 `known_hosts` wants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostKey {
    pub fingerprint: String,
    /// `ssh-ed25519 AAAA...`; unavailable for certificates, hence optional.
    pub line: Option<String>,
}

impl HostKey {
    pub fn from_session(session: &Session) -> Option<HostKey> {
        session.host_key().map(|fingerprint| HostKey {
            fingerprint: fingerprint.to_string(),
            line: session.host_key_line().map(str::to_string),
        })
    }
}

pub struct Probe {
    pub ok: bool,
    pub message: String,
    pub output: String,
    /// Host key fingerprint seen this connection (persist it on first connect).
    pub host_key: Option<String>,
    /// Host public key **line** seen this connection; needed to export the trust as
    /// `known_hosts`.
    pub host_key_line: Option<String>,
    /// Absolute path echoed by the remote command (used to verify the base directory).
    pub resolved: Option<String>,
    /// On a fingerprint mismatch, **the fingerprint the peer actually presented**.
    ///
    /// "Mismatch, connection refused" alone is useless: the user must see the new fingerprint
    /// and accept it in one click (rebuilding a macOS VM or reinstalling changes host keys).
    pub presented_key: Option<String>,
    /// Public key line of the key the peer presented (stored when trusting it, so export needs
    /// no reconnect).
    pub presented_key_line: Option<String>,
    /// Whether this failure was a fingerprint mismatch rather than a network/auth problem.
    pub mismatch: bool,
}

impl Probe {
    fn fail(message: impl Into<String>) -> Probe {
        Probe {
            ok: false,
            message: message.into(),
            output: String::new(),
            host_key: None,
            host_key_line: None,
            resolved: None,
            presented_key: None,
            presented_key_line: None,
            mismatch: false,
        }
    }
}

/// Run one remote command; bounded by `timeout`, exit code 0 means success.
pub async fn run(client: &Client, command: &str, timeout: Duration) -> Probe {
    match tokio::time::timeout(timeout, run_inner(client, command)).await {
        Ok(result) => result,
        Err(_) => Probe::fail(format!("连接超时({}s)", timeout.as_secs())),
    }
}

async fn run_inner(client: &Client, command: &str) -> Probe {
    let mut session = match Session::open(client).await {
        Ok(session) => session,
        Err(message) => return Probe::fail(message),
    };
    let host_key = session.host_key().map(str::to_string);
    let host_key_line = session.host_key_line().map(str::to_string);

    match session.exec(command).await {
        Ok(exec) => Probe {
            ok: exec.ok(),
            message: if exec.ok() {
                format!(
                    "连接成功:{}@{}:{}",
                    session.username(),
                    client.host,
                    client.port
                )
            } else {
                format!("远端命令失败({})", exec.exit_note())
            },
            output: exec.combined(),
            host_key,
            host_key_line,
            resolved: None,
            presented_key: None,
            presented_key_line: None,
            mismatch: false,
        },
        Err(message) => Probe {
            host_key,
            host_key_line,
            ..Probe::fail(message)
        },
    }
}

/// Connection self-test: `echo CGW_OK` and check the echo really came back.
///
/// On a fingerprint mismatch the result also carries the fingerprint the peer presented
/// (`presented_key`), so the UI can offer a "trust this new fingerprint" button instead of
/// trapping the user in an error message — rebuilding a macOS VM or reinstalling changes host
/// keys routinely, that is not an attack.
pub async fn probe(client: &Client, timeout: Duration) -> Probe {
    let probe_future = async {
        let (opened, state) = Session::open_reported(client).await;
        let mut session = match opened {
            Ok(session) => session,
            Err(message) => {
                let mut result = Probe::fail(message);
                result.presented_key = state.seen;
                result.presented_key_line = state.seen_line;
                result.mismatch = state.mismatch;
                return result;
            }
        };

        let host_key = session.host_key().map(str::to_string);
        let host_key_line = session.host_key_line().map(str::to_string);
        let exec = match session.exec(PROBE_COMMAND).await {
            Ok(exec) => exec,
            Err(message) => {
                let mut result = Probe::fail(message);
                result.host_key = host_key;
                result.host_key_line = host_key_line;
                result.presented_key = state.seen;
                result.presented_key_line = state.seen_line;
                result.mismatch = state.mismatch;
                return result;
            }
        };

        let mut result = Probe {
            ok: exec.ok(),
            message: if exec.ok() {
                format!(
                    "连接成功:{}@{}:{}",
                    session.username(),
                    client.host,
                    client.port
                )
            } else {
                format!("远端命令失败({})", exec.exit_note())
            },
            output: exec.combined(),
            host_key,
            host_key_line,
            resolved: None,
            presented_key: state.seen,
            presented_key_line: state.seen_line,
            mismatch: state.mismatch,
        };
        if result.ok && !result.output.contains("CGW_OK") {
            result.ok = false;
            result.message = format!("命令输出异常:{:?}", result.output.trim());
        }
        result
    };

    match tokio::time::timeout(timeout, probe_future).await {
        Ok(result) => result,
        Err(_) => Probe::fail(format!("连接超时({}s)", timeout.as_secs())),
    }
}
/// Ensure the base directory exists and return the remote absolute path.
pub async fn ensure_base_dir(client: &Client, base_dir: &str, timeout: Duration) -> Probe {
    let path = shell_path(base_dir);
    let mut result = run(client, &format!("mkdir -p {path} && cd {path} && pwd"), timeout).await;
    if result.ok {
        let resolved = result
            .output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string();
        if resolved.is_empty() {
            result.ok = false;
            result.message = "远端没有回显目录路径".to_string();
        } else {
            result.resolved = Some(resolved.clone());
            result.message = format!("基准目录就绪:{resolved}");
        }
    }
    result
}

/// Read a private key, including passphrase-protected ones (`ssh-keygen` encrypts by default).
///
/// russh's `load_secret_key(path, password)` takes the passphrase, so the work here is **clear
/// errors**: a missing and a wrong passphrase both surface as an opaque parse error the user
/// cannot act on.
///
/// Supported: OpenSSH (encrypted or not), PKCS#1, PKCS#8, SEC1, and the traditional PEM
/// encryption whose `DEK-Info` is **AES-128-CBC**. Other legacy PEM ciphers (AES-256 /
/// AES-192-CBC, DES-EDE3-CBC) are unsupported; those are detected below and get a conversion
/// command instead of a bogus "wrong passphrase".
pub fn load_private_key(
    path: &str,
    passphrase: &str,
) -> Result<russh::keys::PrivateKey, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("没有选择密钥文件".to_string());
    }
    let passphrase = passphrase.trim();
    let given = if passphrase.is_empty() {
        None
    } else {
        Some(passphrase)
    };

    match load_secret_key(path, given) {
        Ok(key) => Ok(key),
        Err(err) => {
            let text = err.to_string();

            // Catch one **deterministic** failure first: traditional PEM encryption
            // (`Proc-Type` + `DEK-Info`) with a cipher other than AES-128-CBC. The backend
            // only accepts AES-128-CBC, treats the rest as plaintext, fails to parse the
            // ciphertext and reports "wrong passphrase" — sending the user in the wrong
            // direction entirely (the passphrase is fine, the cipher is unsupported).
            if let Some(cipher) = unsupported_legacy_pem_cipher(path) {
                return Err(format!(
                    "读取密钥失败:{path} 用的是传统 PEM 加密,算法是 {cipher},而这里只支持\
                     这种格式里的 AES-128-CBC(`ssh-keygen` 生成的就是它)。两种转换办法:\n  \
                     1) 转成 PKCS#8(推荐):openssl pkcs8 -topk8 -in \"{path}\" -out 新的.key\n  \
                     2) 在原来的机器上重新生成:`ssh-keygen -t ed25519 -f 新的.key`(默认格式直接支持)"
                ));
            }

            let looks_encrypted = {
                let lower = text.to_ascii_lowercase();
                lower.contains("encrypted")
                    || lower.contains("password")
                    || lower.contains("passphrase")
                    || lower.contains("decrypt")
                    // ssh-key answers a bad passphrase with only `SshKey: cryptographic
                    // error`, which alone gives no hint, so count it as encrypted-looking.
                    || lower.contains("cryptographic")
            };

            if is_putty_ppk(path) {
                if given.is_none() && looks_encrypted {
                    return Err(format!(
                        "读取密钥失败:{path} 看起来是**带口令的**私钥,请在「密钥口令」里填上它。({text})"
                    ));
                }
                let out_path = if path.to_ascii_lowercase().ends_with(".ppk") {
                    format!("{}.key", &path[..path.len() - 4])
                } else {
                    format!("{path}.key")
                };
                return Err(format!(
                    "读取密钥失败:PuTTY 密钥 ({path}) 无法加载 ({text})。\n\
                     若口令确认无误仍无法读取，通常是采用了不受支持的 PPK 加密或衍生算法。推荐转换办法:\n  \
                     1) 图形界面: 打开 PuTTYgen -> 顶部菜单 Conversions -> Export OpenSSH key\n  \
                     2) 命令行: puttygen \"{path}\" -O private-openssh -o \"{out_path}\""
                ));
            }
            Err(match (given, looks_encrypted) {
                // Passphrase given and still failing -> almost certainly the wrong passphrase
                // (or the file is not encrypted with it)
                (Some(_), _) => format!(
                    "读取密钥失败:密钥口令不对,或者这个文件不是用该口令加密的。({text})"
                ),
                // No passphrase given while the backend says encrypted -> tell the user to fill it in
                (None, true) => format!(
                    "读取密钥失败:{path} 看起来是**带口令的**私钥,请在「密钥口令」里填上它。({text})"
                ),
                _ => format!("读取密钥失败:{path} - {text}"),
            })
        }
    }
}


/// Return the unsupported cipher of a traditional PEM file, or `None`.
///
/// Traditional PEM encryption is plaintext headers plus a base64 body:
///
/// ```text
/// -----BEGIN EC PRIVATE KEY-----
/// Proc-Type: 4,ENCRYPTED
/// DEK-Info: AES-256-CBC,7A9C836CC31454B7FD0C61D54BA79B5F
/// ```
///
/// Only `AES-128-CBC` is accepted (what `ssh-keygen -m PEM -N <pass>` produces); other
/// ciphers are rejected explicitly here, **not** turned into a "wrong passphrase".
fn unsupported_legacy_pem_cipher(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut encrypted = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Proc-Type:") && line.contains("ENCRYPTED") {
            encrypted = true;
        }
        if let Some(rest) = line.strip_prefix("DEK-Info:") {
            let cipher = rest.split(',').next().unwrap_or("").trim().to_string();
            if encrypted && !cipher.is_empty() && cipher != "AES-128-CBC" {
                return Some(cipher);
            }
        }
    }
    None
}

/// Check if a path points to a PuTTY `.ppk` file (by extension or header).
fn is_putty_ppk(path: &str) -> bool {
    if path.to_ascii_lowercase().ends_with(".ppk") {
        return true;
    }
    if let Ok(text) = std::fs::read_to_string(path) {
        if text.lines().next().map(|l| l.trim_start().starts_with("PuTTY-User-Key-File")).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Safe remote form of a path: common `~` paths stay unquoted (quoting stops `~` expansion),
/// paths with spaces or special characters are wrapped in single quotes.
pub fn shell_path(path: &str) -> String {
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+'));
    if plain {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Poll for the cancel flag every 200ms.
async fn wait_cancel(cancel: &Cancel) {
    while !cancel.requested() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Emit complete lines from the buffer to the callback; an incomplete tail stays buffered.
fn flush_lines(pending: &mut String, on_line: &mut impl FnMut(&str)) {
    while let Some(at) = pending.find('\n') {
        let line = pending[..at].trim_end_matches('\r').to_string();
        pending.drain(..=at);
        if !line.trim().is_empty() {
            on_line(&line);
        }
    }
}

fn default_username() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

#[cfg(test)]
mod exec_tests {
    use super::Exec;

    fn exec(exit: Option<u32>, cancelled: bool) -> Exec {
        Exec {
            exit,
            stdout: Vec::new(),
            stderr: Vec::new(),
            cancelled,
        }
    }

    /// A channel closed early with no `ExitStatus` must **not** count as success.
    ///
    /// `exit` used to start at 0, so "no status" and "exit code 0" looked identical and a
    /// truncated command was recorded as success (acceptance record 2026-09-28 §4, item 3).
    #[test]
    fn missing_exit_status_is_not_success() {
        let missing = exec(None, false);
        assert!(!missing.ok());
        assert_eq!(missing.code(), -1);
        assert!(
            missing.exit_note().contains("没有拿到退出状态"),
            "{}",
            missing.exit_note()
        );

        assert!(exec(Some(0), false).ok());
        assert!(!exec(Some(1), false).ok());
        assert_eq!(exec(Some(1), false).code(), 1);
        assert_eq!(exec(Some(2), false).exit_note(), "退出码 2");

        // Cancelled — even with exit 0 — is not success
        assert!(!exec(Some(0), true).ok());
        assert!(!exec(None, true).ok());
    }
}

#[cfg(test)]
mod key_tests {
    use super::{load_private_key, unsupported_legacy_pem_cipher, HashAlg};

    /// A passphrase-protected key (`ssh-keygen` encrypts by default) must load.
    ///
    /// The fixture is a throwaway test key whose passphrase is the public string below — only
    /// to prove the passphrase works and the missing-passphrase hint is actionable.
    #[test]
    fn reads_an_encrypted_private_key_with_its_passphrase() {
        let key = include_str!("../fixtures/encrypted-ed25519.key");
        let dir = std::env::temp_dir().join("cgw-ssh-key-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("encrypted.key");
        std::fs::write(&path, key).unwrap();
        let path_text = path.display().to_string();

        // Right passphrase -> loads
        assert!(
            load_private_key(&path_text, "cgw-test-pass").is_ok(),
            "带口令的私钥应当能解开"
        );

        // No passphrase -> an error that tells the user to fill in the key passphrase
        let err = load_private_key(&path_text, "").unwrap_err();
        assert!(err.contains("带口令"), "the error gives the user nothing to act on: {err}");
        assert!(err.contains("密钥口令"), "{err}");

        // Wrong passphrase -> also a human-readable message
        let err = load_private_key(&path_text, "wrong-passphrase").unwrap_err();
        assert!(err.contains("口令"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unencrypted key still loads without a passphrase.
    #[test]
    fn reads_a_plain_private_key() {
        let key = include_str!("../fixtures/plain-ed25519.key");
        let dir = std::env::temp_dir().join("cgw-ssh-key-plain");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain.key");
        std::fs::write(&path, key).unwrap();
        assert!(load_private_key(&path.display().to_string(), "").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Empty path / missing file: the error must be readable, not a raw parser error.
    #[test]
    fn reports_unusable_paths_clearly() {
        assert!(load_private_key("", "").unwrap_err().contains("没有选择密钥文件"));
        let err = load_private_key("C:/definitely/not/here.key", "").unwrap_err();
        assert!(err.contains("读取密钥失败"), "{err}");
    }

    // ---------------------------------------------------------------- PuTTY PPK

    /// SSH `string`: 4-byte big-endian length plus content (the PPK format is built from these).
    fn ssh_string(data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(data);
        out
    }

    /// SSH public key blob: string(algorithm) + string(key).
    fn ssh_public_blob(algorithm: &str, key: &[u8]) -> Vec<u8> {
        let mut out = ssh_string(algorithm.as_bytes());
        out.extend(ssh_string(key));
        out
    }

    /// SSH `mpint`: minimal two's-complement, with a 0x00 prepended when the top bit is set.
    ///
    /// PPK stores the Ed25519 private exponent as an mpint (not two bare strings).
    fn ssh_mpint(data: &[u8]) -> Vec<u8> {
        let mut body = data.to_vec();
        while body.len() > 1 && body[0] == 0 {
            body.remove(0);
        }
        if body.first().map(|byte| byte & 0x80 != 0).unwrap_or(false) {
            body.insert(0, 0);
        }
        ssh_string(&body)
    }

    /// Build a PuTTY PPK from the **documented format** (v2 or v3, unencrypted).
    ///
    /// No puttygen exists on this machine, so a real PuTTY file cannot be tested; but the
    /// `Private-MAC` is checked by the peer — a wrong MAC would not load, so "it loads" proves
    /// the MAC algorithm, field order and string/mpint encodings all line up.
    fn make_ppk(
        version: u32,
        algorithm: &str,
        public_blob: &[u8],
        private_blob: &[u8],
        comment: &str,
    ) -> String {
        use hmac::{Hmac, KeyInit, Mac};
        use sha1::{Digest, Sha1};

        let mut buffer = Vec::new();
        buffer.extend(ssh_string(algorithm.as_bytes()));
        buffer.extend(ssh_string(b"none"));
        buffer.extend(ssh_string(comment.as_bytes()));
        buffer.extend(ssh_string(public_blob));
        buffer.extend(ssh_string(private_blob));

        let mac = match version {
            2 => {
                // v2 HMAC key = SHA1("putty-private-key-file-mac-key") (no passphrase when unencrypted)
                let key = Sha1::digest(b"putty-private-key-file-mac-key");
                let mut mac = Hmac::<Sha1>::new_from_slice(&key).unwrap();
                mac.update(&buffer);
                mac.finalize().into_bytes().to_vec()
            }
            // v3 unencrypted uses an **empty key** + SHA256
            3 => {
                let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&[]).unwrap();
                mac.update(&buffer);
                mac.finalize().into_bytes().to_vec()
            }
            _ => panic!("only v2 / v3 are built here"),
        };

        let b64 = |data: &[u8]| {
            use russh::keys::ssh_key::encoding::base64::{Base64, Encoding};
            Base64::encode_string(data)
        };
        let hex: String = mac.iter().map(|byte| format!("{byte:02x}")).collect();

        format!(
            "PuTTY-User-Key-File-{version}: {algorithm}\n\
             Encryption: none\n\
             Comment: {comment}\n\
             Public-Lines: 1\n{}\n\
             Private-Lines: 1\n{}\n\
             Private-MAC: {hex}\n",
            b64(public_blob),
            b64(private_blob)
        )
    }

    /// Pull an ed25519 (public key blob, private seed) pair out of the fixture.
    fn ed25519_fixture_parts() -> (Vec<u8>, Vec<u8>) {
        let key = russh::keys::load_secret_key(
            concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/plain-ed25519.key"),
            None,
        )
        .expect("the fixture should load");
        match key.key_data() {
            russh::keys::ssh_key::private::KeypairData::Ed25519(pair) => (
                pair.public.as_ref().to_vec(),
                pair.private.as_ref().to_vec(),
            ),
            other => panic!("the fixture should be ed25519: {other:?}"),
        }
    }

    /// PuTTY `.ppk` keys (v2 and v3) must load — earlier tests only covered OpenSSH and PEM.
    #[test]
    fn reads_putty_ppk_private_keys() {
        use russh::keys::ssh_key::PublicKey;

        let (public, private) = ed25519_fixture_parts();
        // The base64 in `ssh-ed25519 AAAA...` decodes to string(algorithm) + string(key) —
        // the 32 key bytes **also** carry a length prefix; omit it and the first bytes are
        // read as a length, giving an "internal overflow error".
        let public_blob = ssh_public_blob("ssh-ed25519", &public);

        for version in [2u32, 3u32] {
            let text = make_ppk(version, "ssh-ed25519", &public_blob, &ssh_mpint(&private), "cgw-test");
            let dir = std::env::temp_dir().join("cgw-ssh-ppk-test");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("key-v{version}.ppk"));
            std::fs::write(&path, &text).unwrap();
            let path_text = path.display().to_string();

                        let key = load_private_key(&path_text, "")
                .unwrap_or_else(|err| panic!("PPK v{version} failed to load: {err}"));
            assert_eq!(key.algorithm().as_str(), "ssh-ed25519");

            // Loading is not enough: it must sign, and the signature must verify against the public key
            let namespace = "cgw-test";
            let message = b"CompileGateway signs this";
            let signature = key.sign(namespace, HashAlg::Sha256, message).unwrap();
            let expected = PublicKey::from_bytes(&public_blob).unwrap();
            assert!(
                expected.verify(namespace, message, &signature).is_ok(),
                "the signature made by the PPK v{version} key does not verify"
            );

            let _ = std::fs::remove_file(&path);
        }
    }

    /// An encrypted PPK (`Encryption: aes256-cbc`) without a passphrase needs an actionable hint.
    #[test]
    fn reports_an_encrypted_ppk_without_a_passphrase() {
        let (public, private) = ed25519_fixture_parts();
        let public_blob = ssh_public_blob("ssh-ed25519", &public);

        let text = make_ppk(3, "ssh-ed25519", &public_blob, &ssh_mpint(&private), "cgw-test")
            .replace("Encryption: none", "Encryption: aes256-cbc");
        let dir = std::env::temp_dir().join("cgw-ssh-ppk-enc");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("enc.ppk");
        std::fs::write(&path, text).unwrap();

        let err = load_private_key(&path.display().to_string(), "").unwrap_err();
        assert!(err.contains("带口令"), "should say the key is passphrase-protected: {err}");
        assert!(err.contains("密钥口令"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    /// An unsupported or failing PPK with a passphrase should report clear puttygen conversion guidance.
    #[test]
    fn reports_unsupported_or_wrong_passphrase_ppk_with_conversion_hint() {
        let (public, private) = ed25519_fixture_parts();
        let public_blob = ssh_public_blob("ssh-ed25519", &public);

        let text = make_ppk(3, "ssh-ed25519", &public_blob, &ssh_mpint(&private), "cgw-test")
            .replace("Encryption: none", "Encryption: aes256-cbc");
        let dir = std::env::temp_dir().join("cgw-ssh-ppk-err");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.ppk");
        std::fs::write(&path, text).unwrap();

        let err = load_private_key(&path.display().to_string(), "wrong-passphrase").unwrap_err();
        assert!(err.contains("PuTTY 密钥"), "should recognize PPK: {err}");
        assert!(err.contains("puttygen"), "should provide puttygen command: {err}");
        assert!(err.contains("Export OpenSSH key"), "should provide GUI hint: {err}");
        let _ = std::fs::remove_file(&path);
    }

    /// Keys copied from elsewhere, or saved by an editor on Windows, often have CRLF line
    /// endings — that must not make them unreadable (our own `.ps1` scripts were bitten too).
    #[test]
    fn tolerates_crlf_line_endings() {
        let plain = include_str!("../fixtures/plain-ed25519.key").replace('\n', "\r\n");
        assert_eq!(
            load_fixture("crlf.key", &plain, "").expect("a CRLF plain key should load"),
            "ssh-ed25519"
        );

        let encrypted =
            include_str!("../fixtures/encrypted-ed25519.key").replace('\n', "\r\n");
        assert_eq!(
            load_fixture("crlf-enc.key", &encrypted, "cgw-test-pass")
                .expect("a CRLF encrypted key should load"),
            "ssh-ed25519"
        );

        // Traditional PEM too (its base64 body is split across lines, so it is the most
        // sensitive to line endings)
        let pem = include_str!("../fixtures/rsa-pkcs1.key").replace('\n', "\r\n");
        assert_eq!(
            load_fixture("crlf-pem.key", &pem, "").expect("a CRLF PKCS#1 key should load"),
            "ssh-rsa"
        );
    }

    /// Write a fixture into a temp directory and read it back — `load_private_key` only takes a path.
    fn load_fixture(name: &str, body: &str, passphrase: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join("cgw-ssh-pem-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let result = load_private_key(&path.display().to_string(), passphrase)
            .map(|key| key.algorithm().as_str().to_string());
        let _ = std::fs::remove_file(&path);
        result
    }

    /// **Traditional PEM** formats must load.
    ///
    /// These are not OpenSSH's `BEGIN OPENSSH PRIVATE KEY`: this is what `openssl` and many
    /// exporters produce, and only the OpenSSH form had coverage before.
    ///
    /// | fixture | header | form |
    /// |---|---|---|
    /// | `rsa-pkcs1.key` | `BEGIN RSA PRIVATE KEY` | PKCS#1 (RSA) |
    /// | `rsa-pkcs1-enc.key` | same + `Proc-Type: 4,ENCRYPTED` | PKCS#1 + traditional PEM (AES-128-CBC) |
    /// | `rsa-pkcs8.key` | `BEGIN PRIVATE KEY` | PKCS#8 |
    /// | `ec-pem.key` | `BEGIN EC PRIVATE KEY` | SEC1 (EC) |
    #[test]
    fn reads_traditional_pem_private_keys() {
        let cases = [
            ("rsa-pkcs1.key", include_str!("../fixtures/rsa-pkcs1.key"), "ssh-rsa"),
            ("rsa-pkcs8.key", include_str!("../fixtures/rsa-pkcs8.key"), "ssh-rsa"),
            (
                "ec-pem.key",
                include_str!("../fixtures/ec-pem.key"),
                "ecdsa-sha2-nistp256",
            ),
        ];
        for (name, body, expected) in cases {
            let got = load_fixture(name, body, "")
                .unwrap_or_else(|err| panic!("{name} failed to load: {err}"));
            assert_eq!(got, expected, "wrong algorithm decoded from {name}");
        }
    }

    /// Traditional PEM encryption (PKCS#1 + `Proc-Type`/`DEK-Info`) must handle all three
    /// cases: right passphrase, none, wrong.
    #[test]
    fn reads_a_traditional_pem_encrypted_key() {
        let body = include_str!("../fixtures/rsa-pkcs1-enc.key");

        let got = load_fixture("rsa-pkcs1-enc.key", body, "cgw-test-pass")
            .expect("a traditional PEM encrypted key should unlock with its passphrase");
        assert_eq!(got, "ssh-rsa");

        // No passphrase: say "fill in the key passphrase", not a raw backend error
        let err = load_fixture("rsa-pkcs1-enc.key", body, "").unwrap_err();
        assert!(err.contains("密钥口令"), "{err}");

        // Wrong passphrase: also a human-readable message
        let err = load_fixture("rsa-pkcs1-enc.key", body, "wrong-passphrase").unwrap_err();
        assert!(err.contains("口令"), "{err}");
    }

    /// The two **encrypted** PEM forms `openssl` produces (`ssh-keygen` cannot):
    ///
    /// | fixture | header | encryption | result |
    /// |---|---|---|---|
    /// | `rsa-pkcs8-enc.key` | `BEGIN ENCRYPTED PRIVATE KEY` | PKCS#8 + PBES2 | **supported** |
    /// | `ec-pem-enc.key` | `BEGIN EC PRIVATE KEY` + `Proc-Type` | traditional PEM + **AES-256**-CBC | **unsupported**, but explained |
    ///
    /// Both use the public passphrase `cgw-test-pass`; regenerate them with
    /// `fixtures/make-openssl-keys.sh`.
    ///
    /// The second is the real problem found this round: the backend (ssh-key) only accepts
    /// `AES-128-CBC` in traditional PEM, so AES-256 is treated as **unencrypted**, the
    /// ciphertext fails to parse and it reports "wrong passphrase" — the passphrase is right
    /// and the user is sent in the wrong direction. It is now detected explicitly with a
    /// conversion command (see `unsupported_legacy_pem_cipher`).
    #[test]
    fn handles_openssl_encrypted_pem() {
        // Encrypted PKCS#8: the modern openssl default, supported
        let pkcs8 = load_fixture(
            "rsa-pkcs8-enc.key",
            include_str!("../fixtures/rsa-pkcs8-enc.key"),
            "cgw-test-pass",
        )
        .expect("a PKCS#8 encrypted key should unlock");
        assert_eq!(pkcs8, "ssh-rsa");

        // Traditional PEM + AES-256: unsupported, but the error must be **actionable**
        let body = include_str!("../fixtures/ec-pem-enc.key");
        let err = load_fixture("ec-pem-enc.key", body, "cgw-test-pass").unwrap_err();
        assert!(
            err.contains("AES-256-CBC"),
            "should name the actual cipher: {err}"
        );
        assert!(
            err.contains("只支持") && err.contains("AES-128-CBC"),
            "should name the supported cipher: {err}"
        );
        assert!(
            err.contains("openssl pkcs8 -topk8"),
            "should give a conversion command the user can run: {err}"
        );
        assert!(
            !err.contains("口令不对"),
            "must not report an unsupported format as a wrong passphrase: {err}"
        );

        // Also pin the detection logic itself: the same file with AES-128 must not be rejected
        let swaps = [
            ("AES-256-CBC", Some("AES-256-CBC")),
            ("AES-192-CBC", Some("AES-192-CBC")),
            ("DES-EDE3-CBC", Some("DES-EDE3-CBC")),
            ("AES-128-CBC", None),
        ];
        let dir = std::env::temp_dir().join("cgw-ssh-pem-cipher");
        std::fs::create_dir_all(&dir).unwrap();
        for (cipher, expected) in swaps {
            let path = dir.join("probe.key");
            let text = format!(
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: {cipher},00112233445566778899AABBCCDDEEFF\n\nAAAA\n-----END RSA PRIVATE KEY-----\n"
            );
            std::fs::write(&path, &text).unwrap();
            let got = unsupported_legacy_pem_cipher(&path.display().to_string());
            assert_eq!(got.as_deref(), expected, "cipher={cipher}");
        }
        // Without a Proc-Type (same DEK-Info line) the file does not count as encrypted
        let path = dir.join("probe.key");
        std::fs::write(
            &path,
            "-----BEGIN RSA PRIVATE KEY-----\nDEK-Info: AES-256-CBC,00\n\nAAAA\n",
        )
        .unwrap();
        assert!(unsupported_legacy_pem_cipher(&path.display().to_string()).is_none());
        // An unreadable file must not panic
        assert!(unsupported_legacy_pem_cipher("C:/definitely/not/here.key").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Loading is not enough — the decoded key must **really sign**, and the matching public
    /// key must verify.
    ///
    /// This is what rules out "parsed but the key material is incomplete or wrong": signing
    /// touches the real private numbers and verification requires the public key to match.
    #[test]
    fn pem_keys_can_actually_sign() {
        use russh::keys::ssh_key::PublicKey;

        let cases: [(&str, &str, &str, &str, HashAlg); 5] = [
            (
                "rsa-pkcs1.key",
                include_str!("../fixtures/rsa-pkcs1.key"),
                include_str!("../fixtures/rsa-pkcs1.key.pub"),
                "",
                HashAlg::Sha256,
            ),
            (
                "rsa-pkcs8.key",
                include_str!("../fixtures/rsa-pkcs8.key"),
                include_str!("../fixtures/rsa-pkcs8.key.pub"),
                "",
                HashAlg::Sha256,
            ),
            (
                "rsa-pkcs1-enc.key",
                include_str!("../fixtures/rsa-pkcs1-enc.key"),
                include_str!("../fixtures/rsa-pkcs1-enc.key.pub"),
                "cgw-test-pass",
                HashAlg::Sha256,
            ),
            (
                "rsa-pkcs8-enc.key",
                include_str!("../fixtures/rsa-pkcs8-enc.key"),
                include_str!("../fixtures/rsa-pkcs8-enc.key.pub"),
                "cgw-test-pass",
                HashAlg::Sha256,
            ),
            (
                "ec-pem.key",
                include_str!("../fixtures/ec-pem.key"),
                include_str!("../fixtures/ec-pem.key.pub"),
                "",
                HashAlg::Sha256,
            ),
        ];

        let namespace = "cgw-test";
        let message = b"CompileGateway signs this";

        for (name, key_body, pub_body, passphrase, hash) in cases {
            let dir = std::env::temp_dir().join("cgw-ssh-pem-sign");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, key_body).unwrap();
            let key = load_private_key(&path.display().to_string(), passphrase)
                .unwrap_or_else(|err| panic!("{name}: {err}"));
            let _ = std::fs::remove_file(&path);

            let signature = key
                .sign(namespace, hash, message)
                .unwrap_or_else(|err| panic!("cannot sign with {name}: {err}"));
            let public = PublicKey::from_openssh(pub_body.trim())
                .unwrap_or_else(|err| panic!("cannot read the public key fixture for {name}: {err}"));
            assert!(
                public.verify(namespace, message, &signature).is_ok(),
                "{name}: verification with the matching public key failed"
            );
            // Changing one byte must break verification — otherwise the assertion above proves nothing
            assert!(
                public.verify(namespace, b"CompileGateway signs THIS", &signature).is_err(),
                "{name}: modified content still verified"
            );
        }
    }
}







