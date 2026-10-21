//! Model and persistence for the client (SSH target) list.
//!
//! Stored in `clients.json` next to config.json.
//! `password` is kept in **plain text** on this machine (SSH auth needs the original),
//! but the API never sends it back, only `hasPassword`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "clients.json";
/// Default base dir: design doc §8 uses `~/CompileGateway` so `rsync --delete` cannot
/// wipe a same-named folder in the home directory.
pub const DEFAULT_BASE_DIR: &str = "~/CompileGateway";

// ------------------------------------------------------------------ Auth method

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    /// Username + password
    Password,
    /// Username + private key file
    Key,
}

impl Default for AuthMethod {
    fn default() -> Self {
        AuthMethod::Password
    }
}

impl AuthMethod {
    pub fn code(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::Key => "key",
        }
    }

    pub fn parse(value: &str) -> Option<AuthMethod> {
        match value.trim().to_ascii_lowercase().as_str() {
            "password" => Some(AuthMethod::Password),
            "key" => Some(AuthMethod::Key),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ Model

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientAuth {
    pub method: AuthMethod,
    pub username: String,
    pub password: String,
    pub key_path: String,
    /// Private key passphrase. `ssh-keygen` encrypts keys by default, so users hit this
    /// constantly; like `password` it is **stored but never sent**.
    pub passphrase: String,
}

impl Default for ClientAuth {
    fn default() -> Self {
        ClientAuth {
            method: AuthMethod::Password,
            username: String::new(),
            password: String::new(),
            key_path: String::new(),
            passphrase: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub auth: ClientAuth,
    /// Free-form device tags.
    pub tags: Vec<String>,
    /// Base directory for sources/artifacts on that machine (remote path).
    pub base_dir: String,
    /// Absolute path the remote `pwd` returned the last time the base dir was checked.
    pub base_dir_resolved: String,
    /// Host key fingerprint recorded on first connect (TOFU). Empty means never connected.
    pub host_key: String,
    /// The **raw public key** of this host (`ssh-ed25519 AAAA...`).
    ///
    /// A fingerprint can only be compared, not turned into a `known_hosts` file, so the raw
    /// text is kept to export trust. It is a **cache** of `host_key`, dropped when the
    /// fingerprint changes. Older records may hold only a fingerprint.
    pub host_key_line: String,
    /// Host-level mutual exclusion lock switch for this specific client machine.
    /// When true, jobs targeted to this client run strictly serially.
    pub host_lock: bool,
}

impl Default for Client {
    fn default() -> Self {
        Client {
            id: String::new(),
            name: String::new(),
            host: String::new(),
            port: 22,
            auth: ClientAuth::default(),
            tags: Vec::new(),
            base_dir: DEFAULT_BASE_DIR.to_string(),
            base_dir_resolved: String::new(),
            host_key: String::new(),
            host_key_line: String::new(),
            host_lock: false,
        }
    }
}

impl Client {
    /// Shape returned to clients: no password text.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "host": self.host,
            "port": self.port,
            "auth": {
                "method": self.auth.method.code(),
                "username": self.auth.username,
                "keyPath": self.auth.key_path,
                "hasPassword": !self.auth.password.is_empty(),
                "hasPassphrase": !self.auth.passphrase.is_empty(),
            },
            "tags": self.tags,
            "baseDir": self.base_dir,
            "baseDirResolved": self.base_dir_resolved,
            "hostKey": self.host_key,
            // Report whether the raw text exists too; the UI uses this to show the export button
            "hasKeyLine": !self.host_key_line.is_empty(),
            "hostLock": self.host_lock,
        })
    }
}

// ------------------------------------------------------------------ Storage

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientStore {
    pub clients: Vec<Client>,
}

impl ClientStore {
    pub fn load(path: &Path) -> ClientStore {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, text + "\n")
    }
}

/// clients.json next to config.json.
pub fn store_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(FILE_NAME)
}

/// Generate a new client id.
pub fn new_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    format!("c-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Log scope / instance window label for this client.
pub fn log_scope(id: &str) -> String {
    format!("instance-{id}")
}
