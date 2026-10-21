//! Access credentials: argon2 hashes + session tokens.

use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::{OsRng, RngCore};

/// Session lifetime.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
/// Minimum gap between two login attempts from one source IP (design doc §4.3).
///
/// Default for `auth.loginIntervalSeconds`; the effective gap comes from config,
/// see [`login_interval`].
pub const DEFAULT_LOGIN_INTERVAL: Duration = Duration::from_secs(60);
/// Gap ceiling, so an absurd config value cannot lock logins out for good.
pub const MAX_LOGIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
pub const COOKIE_NAME: &str = "cgw_session";

/// Config seconds -> real gap. 0 disables throttling (LAN debugging); huge values
/// clamp to [`MAX_LOGIN_INTERVAL`].
pub fn login_interval(seconds: u64) -> Duration {
    Duration::from_secs(seconds).min(MAX_LOGIN_INTERVAL)
}

pub struct Session {
    pub username: String,
    pub expires: Instant,
}

impl Session {
    pub fn alive(&self) -> bool {
        self.expires > Instant::now()
    }
}

/// Plaintext -> argon2 hash (PHC string).
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// 32 random bytes, hex-encoded.
pub fn random_token() -> String {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
