//! In-process daemon singleton plus a few text helpers.
//!
//! Design doc §3: **the one core runs in a single long-lived daemon process**, and this
//! module is where that singleton lives. The shell (Tauri app / headless daemon) registers
//! [`WebState`] at boot; afterwards any module reaches it through [`web()`].

use std::sync::{Arc, OnceLock};

use crate::clients::Client;
use crate::config::{Config, Listen};
use crate::i18n::Texts;
use crate::server::WebState;

static WEB: OnceLock<Arc<WebState>> = OnceLock::new();

/// Register the daemon state. Called once from the boot flow.
pub fn set_web(state: Arc<WebState>) {
    let _ = WEB.set(state);
}

/// Once the WebUI runs, the CLI window's `status` command must read the current config.
pub fn web() -> Option<&'static Arc<WebState>> {
    WEB.get()
}

/// Window label of the form `instance-<id>` -> its client.
pub fn client_for_scope(scope: &str) -> Option<Client> {
    let id = scope.strip_prefix("instance-")?;
    web()?.clients_snapshot().into_iter().find(|c| c.id == id)
}

// ------------------------------------------------------------------ Text helpers

pub fn listen_text(t: &Texts, listen: Listen) -> &'static str {
    match listen {
        Listen::Lan => t.listen_lan,
        Listen::Wan => t.listen_wan,
    }
}

pub fn auth_text(t: &Texts, config: &Config) -> String {
    if config.auth.is_configured() {
        config.auth.username.clone()
    } else {
        t.auth_none.to_string()
    }
}

/// Client auth summary shared by the CLI and the instance windows.
pub fn auth_label(t: &Texts, client: &Client) -> String {
    let method = match client.auth.method {
        crate::clients::AuthMethod::Password => t.auth_password,
        crate::clients::AuthMethod::Key => t.auth_key,
    };
    let user = client.auth.username.trim();
    if user.is_empty() {
        method.to_string()
    } else {
        format!("{method} · {user}")
    }
}
