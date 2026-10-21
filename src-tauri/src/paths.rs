//! Relative-path normalization inside a confined location (design doc §7.2 / §7.4).
//!
//! `from` / `to` / `sources` in a job JSON may only be **relative paths**, and must
//! stay under the matching root (`remote` = project remote root, `host` = the
//! daemon's managed host directory). This module enforces the **syntax** layer only:
//! absolute paths, `~`, drive letters and any `..` segment are rejected.
//!
//! Why the syntax layer suffices: v1 remote operations all run as
//! `cd <root> && ...`, and a relative path with no `..` and no leading `/` cannot
//! mathematically escape `root`. The only residual risk is a symlink **already
//! present** inside the project tree that points outside -- that requires such a
//! link to exist on the target, is outside this tool's write scope, and is recorded
//! as a known limitation in the docs.

use std::path::{Path, PathBuf};

use crate::i18n;

pub const SIDE_REMOTE: &str = "remote";
pub const SIDE_HOST: &str = "host";

/// Why path validation failed.
///
/// Two classes, because they are **different errors** in the UI: a syntax error means
/// the user wrote it wrong, while an escape means "after resolution the path left the
/// root" (a symlink). They deserve different messages and error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The path itself is invalid (empty, absolute, contains `..`, drive letter...).
    Syntax(String),
    /// Resolution landed outside the root.
    Escape(String),
}

impl std::fmt::Display for PathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathError::Syntax(message) | PathError::Escape(message) => formatter.write_str(message),
        }
    }
}

impl PathError {
    pub fn message(&self) -> &str {
        match self {
            PathError::Syntax(message) | PathError::Escape(message) => message,
        }
    }
}

/// Lets `?` work where `Result<_, String>` is expected -- most callers only want to
/// bubble the message up; only those distinguishing the two classes match on it.
impl From<PathError> for String {
    fn from(error: PathError) -> String {
        error.to_string()
    }
}

/// The value of the `side` field.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The target machine (over SSH).
    #[default]
    Remote,
    /// The machine the daemon itself runs on.
    Host,
}

impl Side {
    pub fn parse(value: &str) -> Result<Side, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            SIDE_REMOTE => Ok(Side::Remote),
            SIDE_HOST => Ok(Side::Host),
            other => Err(i18n::path_side_invalid(i18n::current(), other)),
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Side::Remote => SIDE_REMOTE,
            Side::Host => SIDE_HOST,
        }
    }

    pub fn is_remote(self) -> bool {
        matches!(self, Side::Remote)
    }
}

/// Normalize a user-written relative path into a "safe relative path". An empty
/// string means the root directory itself.
///
/// Rejects: absolute paths, drive letters, a leading `~`, any `..` segment, and the
/// Windows-reserved `\\?\` prefix.
pub fn normalize_relative(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(i18n::path_empty(i18n::current()));
    }
    if trimmed.contains('\0') {
        return Err(i18n::path_has_nul(i18n::current()));
    }

    // Unify separators before checking, so `..\\..\\x` cannot slip past.
    let unified = trimmed.replace('\\', "/");

    if unified.starts_with('/') {
        return Err(i18n::path_absolute_refused(i18n::current(), raw));
    }
    if unified.starts_with('~') {
        return Err(i18n::path_tilde_refused(i18n::current(), raw));
    }
    if unified.starts_with("//") {
        return Err(i18n::path_unc_refused(i18n::current(), raw));
    }
    // Drive letter: C: / c:/
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(i18n::path_drive_refused(i18n::current(), raw));
    }

    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(i18n::path_dotdot_refused(i18n::current(), raw)),
            other => parts.push(other),
        }
    }

    Ok(parts.join("/"))
}

/// Join a normalized relative path under the root (still string concatenation; the
/// filesystem is not touched).
pub fn join_root(root: &str, relative: &str) -> String {
    let base = root.trim().trim_end_matches('/');
    if relative.is_empty() {
        if base.is_empty() {
            ".".to_string()
        } else {
            base.to_string()
        }
    } else if base.is_empty() {
        relative.to_string()
    } else {
        format!("{base}/{relative}")
    }
}

/// A location reference carrying a `side`.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase", default)]
pub struct Location {
    /// Which side. Omitting it means `remote`.
    pub side: Side,
    /// **Relative** path under the project root (design doc §7.1); no absolute path
    /// or `..`. An empty string is the root itself, meaningful only in a few places
    /// (list the root, use the root as a target).
    pub path: String,
}

impl Location {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "side": self.side.code(), "path": self.path })
    }
}

/// Boundary check for a local path: after normalization it must still be inside root.
///
/// The syntax layer ([`normalize_relative`]) already blocks `..`; this adds a second
/// layer for the **host side** (the daemon runs on this machine, so symlinks can
/// really be resolved).
///
/// A missing root **or** target is normal (first copy into `staging/`, host managed
/// directory not created yet), so both sides resolve via [`resolve_existing`] to
/// "deepest existing ancestor + remainder" before the prefix check -- which does the
/// symlink check without demanding the path already exist.
pub fn confine_local(root: &Path, relative: &str) -> Result<std::path::PathBuf, PathError> {
    // Empty path = the root itself (listing the root, targeting the root)
    let safe = if relative.trim().is_empty() {
        String::new()
    } else {
        normalize_relative(relative).map_err(PathError::Syntax)?
    };
    let target = if safe.is_empty() {
        root.to_path_buf()
    } else {
        root.join(&safe)
    };

    let root_real = resolve_existing(root);
    let target_real = resolve_existing(&target);
    if !target_real.starts_with(&root_real) {
        return Err(PathError::Escape(i18n::path_escape(
            i18n::current(),
            relative,
        )));
    }
    Ok(target)
}

/// Resolve a path to "the real path of its deepest existing ancestor + the remaining
/// literal segments".
///
/// `canonicalize` needs the whole path to exist, but the paths checked here often do
/// not, so walk upward to the first resolvable ancestor and reattach the rest.
pub fn resolve_existing(path: &Path) -> std::path::PathBuf {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();

    loop {
        match probe.canonicalize() {
            Ok(real) => {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            Err(_) => match probe.file_name() {
                Some(name) => {
                    tail.push(name.to_os_string());
                    if !probe.pop() {
                        // Popped all the way to the root without resolving (a missing
                        // drive letter, say); return as is
                        return path.to_path_buf();
                    }
                }
                None => return path.to_path_buf(),
            },
        }
    }
}

/// The user home directory on the daemon's machine.
///
/// Design doc §6.1 requires "browsing starts at the user home by default, not the whole
/// filesystem" -- this is that root.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Path string for display.
///
/// Windows `canonicalize` adds the `\\?\` (verbatim path) prefix -- that belongs in
/// filesystem calls only, not in API responses or the UI.
pub fn display(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) => stripped.to_string(),
        None => text,
    }
}

// ------------------------------------------------------------------ env var compatibility layer

/// Per-user shared directory (`CGW_HOME`, default `~/.compilegateway`).
///
/// **Why it exists**: the daemon and the CLI may live in different directories -- once
/// `cgw` is on PATH its `current_exe()` is the PATH location, while the daemon's
/// `config.json` / `runtime.json` sit in the install directory. Both sides need a
/// meeting point they can find, and this is it.
///
/// The name is deliberately platform-neutral (no `%APPDATA%` / XDG /
/// `Library/Application Support` split): it is only where a "pointer file" lands, and
/// one uniform `~/.compilegateway` is easier to explain and to move -- Linux / macOS
/// behave identically, and setting `CGW_HOME` relocates the whole thing.
pub fn cgw_home() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("CGW_HOME") {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    home_dir().map(|home| home.join(".compilegateway"))
}

/// Make sure `CGW_HOME` exists (called before the daemon writes its pointer file).
pub fn ensure_cgw_home() -> Option<PathBuf> {
    let dir = cgw_home()?;
    if std::fs::create_dir_all(&dir).is_ok() {
        Some(dir)
    } else {
        None
    }
}

/// Confine an **absolute** candidate path inside `root` (for the §6.1 browse API).
///
/// Difference from [`confine_local`]: that one takes a "path relative to the root",
/// this one takes an absolute path clicked in the browse UI -- so first reject anything
/// not under root, then resolve symlinks to confirm it did not detour outside.
pub fn confine_absolute(root: &Path, candidate: &Path) -> Result<PathBuf, PathError> {
    if !candidate.is_absolute() {
        return Err(PathError::Syntax(i18n::path_absolute_required(
            i18n::current(),
            &candidate.display().to_string(),
        )));
    }
    let root_real = resolve_existing(root);
    let target_real = resolve_existing(candidate);
    if !target_real.starts_with(&root_real) {
        return Err(PathError::Escape(i18n::path_outside_browsable(
            i18n::current(),
            &root_real.display().to_string(),
            &candidate.display().to_string(),
        )));
    }
    Ok(target_real)
}

// ------------------------------------------------------------------ allowed host directories

/// Host directories allowed for operation (design doc §6.1).
///
/// By default only the user home; `allowedRoots` in `config.json` can add more (source
/// on another drive, say). **Every API must apply the same root set the same way**,
/// otherwise "browse says no, create_project says yes" contradictions appear -- exactly
/// what acceptance record §7 caught.
pub struct HostRoots {
    roots: Vec<PathBuf>,
}

impl HostRoots {
    /// User home + extra directories from config (missing ones are ignored, so one bad
    /// entry cannot void the whole policy).
    pub fn resolve(extra: &[String]) -> HostRoots {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(home) = home_dir() {
            roots.push(resolve_existing(&home));
        }
        for raw in extra {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            let candidate = resolve_existing(Path::new(trimmed));
            if candidate.is_dir() && !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
        HostRoots { roots }
    }

    /// Where browsing starts (the first root = user home).
    pub fn first(&self) -> Option<&Path> {
        self.roots.first().map(PathBuf::as_path)
    }

    pub fn all(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Which root contains this absolute path (used to name the allowed directories in
    /// error messages).
    pub fn containing(&self, candidate: &Path) -> Option<&Path> {
        let target = resolve_existing(candidate);
        self.roots
            .iter()
            .find(|root| target.starts_with(root))
            .map(PathBuf::as_path)
    }

    /// An absolute path must fall inside some root.
    /// **browse / create_project / detect all go through this one function.**
    pub fn confine(&self, candidate: &Path) -> Result<PathBuf, PathError> {
        if !candidate.is_absolute() {
            return Err(PathError::Syntax(i18n::path_absolute_required(
                i18n::current(),
                &candidate.display().to_string(),
            )));
        }
        if self.roots.is_empty() {
            return Err(PathError::Escape(i18n::path_no_home_or_roots(i18n::current())));
        }
        match self.containing(candidate) {
            Some(_) => Ok(resolve_existing(candidate)),
            None => {
                let allowed: Vec<String> =
                    self.roots.iter().map(|root| display(root)).collect();
                Err(PathError::Escape(i18n::path_outside_allowed_roots(
                    i18n::current(),
                    &candidate.display().to_string(),
                    &allowed,
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CGW_HOME` wins; without it, fall back to `~/.compilegateway`.
    #[test]
    fn cgw_home_prefers_the_env_var() {
        // These two cases modify process-level environment variables, so run serially
        // (other tests in this binary do not read it)
        let saved = std::env::var_os("CGW_HOME");
        std::env::set_var("CGW_HOME", "/tmp/cgw-home-test");
        assert_eq!(cgw_home().unwrap(), PathBuf::from("/tmp/cgw-home-test"));
        std::env::remove_var("CGW_HOME");
        let fallback = cgw_home();
        assert!(fallback.is_some(), "must at least resolve ~/.compilegateway");
        assert!(display(&fallback.unwrap()).ends_with(".compilegateway"));
        match saved {
            Some(value) => std::env::set_var("CGW_HOME", value),
            None => std::env::remove_var("CGW_HOME"),
        }
    }

    #[test]
    fn normalizes_and_accepts_plain_relative_paths() {
        assert_eq!(normalize_relative("a/b").unwrap(), "a/b");
        assert_eq!(normalize_relative("./a/./b/").unwrap(), "a/b");
        assert_eq!(normalize_relative("a//b").unwrap(), "a/b");
        assert_eq!(normalize_relative("a\\b").unwrap(), "a/b");
        assert_eq!(normalize_relative(".").unwrap(), "");
        assert_eq!(normalize_relative("a.b/c-d_e").unwrap(), "a.b/c-d_e");
    }

    #[test]
    fn rejects_escapes() {
        for bad in [
            "/etc/passwd",
            "..",
            "../x",
            "a/../../x",
            "a/..",
            "~/.ssh",
            "~/x",
            "C:/Windows",
            "c:\\windows",
            "//server/share",
            "a/b\u{0}c",
            "",
            "   ",
        ] {
            assert!(
                normalize_relative(bad).is_err(),
                "was rejected in theory yet passed: {bad:?}"
            );
        }
    }

    #[test]
    fn join_root_handles_empty_and_trailing_slash() {
        assert_eq!(join_root("~/Work", "a/b"), "~/Work/a/b");
        assert_eq!(join_root("~/Work/", "a"), "~/Work/a");
        assert_eq!(join_root("~/Work", ""), "~/Work");
        assert_eq!(join_root("", "a"), "a");
    }

    #[test]
    fn side_parsing() {
        assert_eq!(Side::parse("remote").unwrap(), Side::Remote);
        assert_eq!(Side::parse("HOST").unwrap(), Side::Host);
        assert!(Side::parse("elsewhere").is_err());
    }

    #[test]
    fn confine_local_stays_inside_root() {
        let root = std::env::temp_dir().join("cgw-paths-test");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        assert!(confine_local(&root, "sub/../sub").is_err());
        assert!(confine_local(&root, "sub").is_ok());
        assert!(confine_local(&root, "sub/not-yet").is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A host managed directory does not exist the first time it is used -- that must
    /// still work, and nested subdirectories must be creatable.
    #[test]
    fn confine_local_accepts_a_root_that_does_not_exist_yet() {
        let base = std::env::temp_dir().join("cgw-paths-missing-root");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let root = base.join("CompileGateway/proj");
        assert!(!root.exists(), "this test requires the root not to exist");

        let target = confine_local(&root, "artifacts/app.app").unwrap();
        assert!(target.starts_with(&root));
        assert!(confine_local(&root, "..").is_err());
        assert!(confine_local(&root, "/etc/passwd").is_err());

        // Empty path = the root itself
        assert_eq!(confine_local(&root, "").unwrap(), root);
        assert_eq!(confine_local(&root, "  ").unwrap(), root);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Browse boundary: an absolute path must fall under an allowed root (design doc §6.1).
    #[test]
    fn confine_absolute_keeps_browsing_inside_the_allowed_root() {
        let base = std::env::temp_dir().join("cgw-paths-browse");
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        std::fs::create_dir_all(home.join("src/app")).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        assert!(confine_absolute(&home, &home.join("src")).is_ok());
        assert!(confine_absolute(&home, &home).is_ok());
        // A path that does not exist yet is fine (the user wants to create a directory)
        assert!(confine_absolute(&home, &home.join("src/new")).is_ok());

        // Anything outside a root is rejected
        assert!(matches!(
            confine_absolute(&home, &outside),
            Err(PathError::Escape(_))
        ));
        assert!(confine_absolute(&home, &base).is_err());
        // Relative paths are rejected outright
        assert!(matches!(
            confine_absolute(&home, Path::new("src")),
            Err(PathError::Syntax(_))
        ));

        // A symlink detour must be rejected too
        let link = home.join("escape");
        if make_symlink(&outside, &link) {
            assert!(matches!(
                confine_absolute(&home, &link),
                Err(PathError::Escape(_))
            ));
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    fn make_symlink(target: &Path, link: &Path) -> bool {
        std::os::unix::fs::symlink(target, link).is_ok()
    }

    #[cfg(windows)]
    fn make_symlink(target: &Path, link: &Path) -> bool {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
}
