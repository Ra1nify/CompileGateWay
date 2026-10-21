//! Source sync: host machine <-> client machine.
//!
//! Transfer runs **tar inside the SSH channel**: pack the local dir to tar.gz onto the
//! remote command's stdin and unpack with `tar -xzf -`; the reverse streams back.
//! rsync is avoided (the daemon may run on Windows, where it is not provided), and the
//! cost is a **full transfer every time**; incremental sync is left for a later phase.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use walkdir::WalkDir;

use crate::clients::Client;
use crate::i18n::{self, Lang};
use crate::ssh::{shell_path, Cancel, Session};

/// In-memory archive limit; exceeding it fails fast rather than blowing up the daemon.
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;

/// Design doc §6.5: host-side build artifacts are **never pushed**. Fixed behavior, not a toggle.
/// Matched by name (same-named dirs/files at any depth are skipped), alongside the project excludes.
pub const NEVER_PUSH: [&str; 13] = [
    "DerivedData",
    ".cgw",
    "build",
    ".gradle",
    "node_modules",
    ".dart_tool",
    "Pods",
    ".symlinks",
    "target",
    ".venv",
    "__pycache__",
    ".next",
    "dist",
];

pub struct SyncOutcome {
    pub ok: bool,
    /// Cancelled by the user.
    pub cancelled: bool,
    pub message: String,
    pub files: u64,
    pub bytes: u64,
    /// Host fingerprint seen on this connection (persisted on first connect).
    pub host_key: Option<crate::ssh::HostKey>,
}

impl SyncOutcome {
    fn fail(message: impl Into<String>) -> SyncOutcome {
        SyncOutcome {
            ok: false,
            cancelled: false,
            message: message.into(),
            files: 0,
            bytes: 0,
            host_key: None,
        }
    }
}

// ------------------------------------------------------------------ Push

/// Push `source` to the remote `remote_dir`.
/// `clean` = true removes the remote dir before unpacking (the design doc's "overwrite").
pub async fn push(
    client: &Client,
    source: &Path,
    remote_dir: &str,
    excludes: &[String],
    clean: bool,
    timeout: Duration,
    cancel: &Cancel,
    lang: Lang,
) -> SyncOutcome {
    push_with(
        client, source, remote_dir, excludes, clean, false, timeout, cancel, lang,
    )
    .await
}

/// Same as above, plus the line-ending switch.
///
/// With `keep_crlf = false` (the default), shebang scripts are normalized CRLF -> LF: a
/// `gradlew` checked out on Windows is CRLF and fails on Mac with `/bin/sh^M: no such file
/// or directory` from the `\r` in the shebang (acceptance record §4.4).
#[allow(clippy::too_many_arguments)]
pub async fn push_with(
    client: &Client,
    source: &Path,
    remote_dir: &str,
    excludes: &[String],
    clean: bool,
    keep_crlf: bool,
    timeout: Duration,
    cancel: &Cancel,
    lang: Lang,
) -> SyncOutcome {
    if !source.is_dir() {
        return SyncOutcome::fail(i18n::sync_source_missing(lang, &source.display().to_string()));
    }

    // Overwrite rm -rf's the remote dir: keep it away from the base or home dir
    if clean {
        if let Err(message) = guard_clean_target(client, remote_dir, lang) {
            return SyncOutcome::fail(message);
        }
    }

    // Packing walks the whole tree, reads up to `MAX_NORMALIZE_BYTES` per script and gzips
    // up to `MAX_ARCHIVE_BYTES` — all **blocking**. It runs on the shared multi-thread runtime,
    // so it must not sit on a worker: one large mobile project would starve HTTP/MCP handlers.
    let source_owned = source.to_path_buf();
    let excludes_owned = excludes.to_vec();
    let packed = tokio::task::spawn_blocking(move || {
        build_archive_opts(&source_owned, &excludes_owned, keep_crlf, lang)
    })
    .await;
    let (archive, stats) = match packed {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => return SyncOutcome::fail(i18n::sync_pack_failed(lang, &err.to_string())),
        Err(err) => return SyncOutcome::fail(i18n::sync_pack_task_failed(lang, &err.to_string())),
    };
    let (files, bytes) = (stats.files, stats.bytes);

    let pushed = match tokio::time::timeout(
        timeout,
        push_inner(client, &archive, remote_dir, clean, cancel, lang),
    )
    .await
    {
        Ok(Ok(host_key)) => SyncOutcome {
            ok: true,
            cancelled: false,
            message: i18n::sync_pushed(lang, &stats.describe(lang), remote_dir),
            files,
            bytes,
            host_key,
        },
        Ok(Err((message, cancelled, host_key))) => SyncOutcome {
            cancelled,
            host_key,
            ..SyncOutcome::fail(message)
        },
        Err(_) => SyncOutcome::fail(i18n::sync_push_timeout(lang, timeout.as_secs())),
    };
    pushed
}

async fn push_inner(
    client: &Client,
    archive: &[u8],
    remote_dir: &str,
    clean: bool,
    cancel: &Cancel,
    lang: Lang,
) -> Result<Option<crate::ssh::HostKey>, (String, bool, Option<crate::ssh::HostKey>)> {
    let mut session = Session::open_cancellable(client, cancel)
        .await
        .map_err(|message| (message, cancel.requested(), None))?;
    let host_key = crate::ssh::HostKey::from_session(&session);

    let dir = shell_path(remote_dir);
    let command = if clean {
        format!("rm -rf {dir} && mkdir -p {dir} && tar -xzf - -C {dir}")
    } else {
        format!("mkdir -p {dir} && tar -xzf - -C {dir}")
    };

    let exec = session
        .exec_stdin_cancellable(&command, archive, cancel)
        .await
        .map_err(|message| (message, false, host_key.clone()))?;

    if exec.cancelled {
        return Err((i18n::sync_push_cancelled(lang), true, host_key));
    }
    if exec.ok() {
        Ok(host_key)
    } else {
        Err((
            i18n::sync_remote_unpack_failed(lang, &exec.exit_note(), &exec.combined()),
            false,
            host_key,
        ))
    }
}

// ------------------------------------------------------------------ Pull

/// Pull the remote `remote_dir` back to host `target`.
pub async fn pull(
    client: &Client,
    remote_dir: &str,
    target: &Path,
    timeout: Duration,
    cancel: &Cancel,
    lang: Lang,
) -> SyncOutcome {
    let fetched =
        match tokio::time::timeout(timeout, pull_inner(client, remote_dir, cancel, lang)).await {
            Ok(Ok(value)) => value,
            Ok(Err((message, cancelled, host_key))) => {
                return SyncOutcome {
                    cancelled,
                    host_key,
                    ..SyncOutcome::fail(message)
                }
            }
            Err(_) => return SyncOutcome::fail(i18n::sync_pull_timeout(lang, timeout.as_secs())),
        };

    let (archive, host_key) = fetched;
    // Same reasoning as the push side: gunzip + unpack is blocking file IO on a work path.
    let target_owned = target.to_path_buf();
    let unpacked =
        tokio::task::spawn_blocking(move || extract_archive(&archive, &target_owned)).await;
    match unpacked {
        Ok(Ok((files, bytes))) => SyncOutcome {
            ok: true,
            cancelled: false,
            message: i18n::sync_pulled(
                lang,
                files,
                &human_bytes(bytes),
                &target.display().to_string(),
            ),
            files,
            bytes,
            host_key,
        },
        Ok(Err(err)) => SyncOutcome {
            host_key,
            ..SyncOutcome::fail(i18n::sync_unpack_failed(lang, &err.to_string()))
        },
        Err(err) => SyncOutcome {
            host_key,
            ..SyncOutcome::fail(i18n::sync_unpack_task_failed(lang, &err.to_string()))
        },
    }
}

async fn pull_inner(
    client: &Client,
    remote_dir: &str,
    cancel: &Cancel,
    lang: Lang,
) -> Result<(Vec<u8>, Option<crate::ssh::HostKey>), (String, bool, Option<crate::ssh::HostKey>)> {
    let mut session = Session::open_cancellable(client, cancel)
        .await
        .map_err(|message| (message, cancel.requested(), None))?;
    let host_key = crate::ssh::HostKey::from_session(&session);

    let dir = shell_path(remote_dir);
    let exec = session
        .exec_collect_cancellable(&format!("tar -czf - -C {dir} ."), cancel)
        .await
        .map_err(|message| (message, false, host_key.clone()))?;

    if exec.cancelled {
        return Err((i18n::sync_pull_cancelled(lang), true, host_key));
    }
    if !exec.ok() {
        return Err((
            i18n::sync_remote_pack_failed(lang, &exec.exit_note(), &exec.combined()),
            false,
            host_key,
        ));
    }

    Ok((exec.stdout, host_key))
}

// ------------------------------------------------------------------ Pack / unpack

/// What went into the archive.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveStats {
    pub files: u64,
    pub bytes: u64,
    /// **Symlink entry count**. Links travel as links, not dereferenced (`tar`'s
    /// `--dereference` is what follows them, and that would pack the far side in).
    pub links: u64,
    /// Of those, the ones pointing **outside** the source dir. Informational only: the link
    /// itself travels, never the content it points at, so nothing is carried out.
    pub outside_links: u64,
    /// **Shebang scripts** normalized CRLF -> LF during push.
    ///
    /// For Windows workspaces: those scripts are CRLF on checkout, and after `chmod +x` on
    /// Mac/Linux the shebang's `\r` triggers `/bin/sh^M: no such file or directory`.
    /// Only files with `#!` are touched; binaries are never modified by a byte.
    pub crlf_normalized: u64,
}

/// A single file larger than this is not read (normalization only matters for scripts).
const MAX_NORMALIZE_BYTES: u64 = 4 * 1024 * 1024;

/// In a shebang script with CRLF, replace **only** trailing `\r\n` with `\n`.
///
/// `Some(new content)` means it really changed; `None` (unchanged, or not a script) lets the
/// caller pack it as-is, so both outcomes share one zero-cost path.
fn normalize_shebang(bytes: &[u8]) -> Option<Vec<u8>> {
    // UTF-8 BOM + `#!` also counts as a script
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if !body.starts_with(b"#!") || !bytes.contains(&b'\r') {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            out.push(b'\n');
            index += 2;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    if out == bytes {
        None
    } else {
        Some(out)
    }
}

impl ArchiveStats {
    /// Human-readable one-liner: `N files / X`, plus a link note when there are links.
    ///
    /// Takes a [`Lang`]: this text reaches the user as a job message in the WebUI / CLI.
    pub fn describe(&self, lang: crate::i18n::Lang) -> String {
        crate::i18n::archive_stats_summary(
            lang,
            self.files,
            &human_bytes(self.bytes),
            self.links,
            self.outside_links,
            self.crlf_normalized,
        )
    }
}

/// Does this link target escape the source dir?
///
/// Purely **lexical** (no filesystem access): absolute target -> yes; `..` above the root -> yes.
fn link_escapes(source: &Path, link_path: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return true;
    }
    // Depth of the link itself inside the source dir (levels of its containing dir)
    let mut depth = link_path
        .strip_prefix(source)
        .map(|rel| rel.components().count().saturating_sub(1))
        .unwrap_or(0);
    for component in target.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            std::path::Component::Normal(_) => depth += 1,
            // RootDir / Prefix (drive letter) / anything odd: treat as escaped
            _ => return true,
        }
    }
    false
}

/// Pack without options (used by tests and keep-as-is callers).
#[cfg(test)]
fn build_archive(source: &Path, excludes: &[String]) -> std::io::Result<(Vec<u8>, ArchiveStats)> {
    build_archive_opts(source, excludes, false, Lang::EnUS)
}

/// Optioned version. `keep_crlf = true` does **no** line-ending normalization (§4.4).
pub fn build_archive_opts(
    source: &Path,
    excludes: &[String],
    keep_crlf: bool,
    lang: Lang,
) -> std::io::Result<(Vec<u8>, ArchiveStats)> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    // **Must be turned off explicitly**: this crate defaults `follow_symlinks` to `true`,
    // unlike mainstream tar. Left on, packing a symlink writes the **target's content** into
    // the archive: one link to `/etc/passwd` would carry a file outside the project out.
    builder.follow_symlinks(false);

    let mut stats = ArchiveStats::default();

    let walker = WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry
                .path()
                .strip_prefix(source)
                .map(|rel| rel.as_os_str().is_empty() || !is_excluded(rel, excludes))
                .unwrap_or(true)
        });

    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(source) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel,
            _ => continue,
        };

        let file_type = entry.file_type();
        if file_type.is_symlink() {
            // Store links as links (design doc §7.2 relative-path normalization targets
            // **task paths**, not links already in the source tree). The old `continue`
            // here broke `node_modules/.bin`, CocoaPods and Flutter's `ios/.symlinks`.
            if let Ok(target) = std::fs::read_link(path) {
                if link_escapes(source, path, &target) {
                    stats.outside_links += 1;
                }
            }
            builder.append_path_with_name(path, rel)?;
            stats.links += 1;
        } else if file_type.is_dir() {
            builder.append_dir(rel, path)?;
        } else if file_type.is_file() {
            let metadata = entry.metadata().ok();
            let size = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
            let mtime = metadata
                .as_ref()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            // ---- Line-ending normalization (Windows workspace -> Unix target) -----
            // Scripts like `gradlew` are CRLF on Windows; after `chmod +x` they fail on Mac
            // with `/bin/sh^M: no such file or directory` from the extra CR in the shebang.
            // Only **shebang files** (surely meant to run as scripts) are touched.
            let mut normalized = None;
            if !keep_crlf && size > 0 && size <= MAX_NORMALIZE_BYTES {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Some(fixed) = normalize_shebang(&bytes) {
                        normalized = Some(fixed);
                    }
                }
            }

            if let Some(data) = normalized {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                // Normalized files are always scripts: set the executable bit (no remote chmod)
                header.set_mode(0o755);
                header.set_mtime(mtime);
                header.set_cksum();
                builder.append_data(&mut header, rel, data.as_slice())?;
                stats.crlf_normalized += 1;
                stats.bytes += data.len() as u64;
            } else {
                stats.bytes += size;
                builder.append_path_with_name(path, rel)?;
            }
            stats.files += 1;
        }
    }

    let encoder = builder.into_inner()?;
    let data = encoder.finish()?;
    if data.len() > MAX_ARCHIVE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            i18n::sync_archive_too_large(lang, &human_bytes(MAX_ARCHIVE_BYTES as u64)),
        ));
    }
    Ok((data, stats))
}

fn extract_archive(data: &[u8], target: &Path) -> std::io::Result<(u64, u64)> {
    std::fs::create_dir_all(target)?;

    let mut archive = tar::Archive::new(GzDecoder::new(data));
    let mut files = 0u64;
    let mut bytes = 0u64;
    // Directories seen in the stream. Unpacking a member requires its parent to exist
    // already, so we create them as we go; empty directories are recreated after the loop.
    // One pass only (S2) -- the old code walked a second `GzDecoder` purely to create them,
    // paying a full extra decompression of a 512MB-bounded archive.
    let mut dirs: Vec<PathBuf> = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();

        // Reject absolute paths and .. (unpack_in also blocks them, this is a first line)
        if is_unsafe_archive_path(&path) {
            continue;
        }

        if entry.header().entry_type().is_dir() {
            // Remember it; `unpack_in` on later members needs the parent chain present.
            let _ = std::fs::create_dir_all(target.join(&path));
            dirs.push(path);
            continue;
        }
        // Some writers emit a file before its directory entry; make sure the parent exists.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(target.join(parent));
            }
        }
        // Symlinks: still unpacked (the link itself is kept) but **not counted as files**,
        // matching the push side (a link is not "content"), so round-trip numbers agree.
        if entry.header().entry_type().is_symlink() {
            entry.unpack_in(target)?;
            continue;
        }
        files += 1;
        bytes += entry.size();
        entry.unpack_in(target)?;
    }

    // Directories that were empty in the source can be dropped by some tar writers; make
    // sure every directory the stream declared exists, even when it had no members.
    for path in dirs {
        let _ = std::fs::create_dir_all(target.join(path));
    }

    Ok((files, bytes))
}

/// Drop absolute paths, `..` and drive prefixes in archives. `unpack_in` blocks them too.
fn is_unsafe_archive_path(path: &Path) -> bool {
    path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
}

/// Normalize a path/pattern to the comparison form used by [`is_excluded`]:
/// forward slashes, no `.` segments, no empty segments, no trailing slash.
fn normalize_match_path(value: &str) -> String {
    let unified = value.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                // A `..` can only cancel a real segment; keep it when there is nothing to pop.
                if parts.pop().is_none() {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn is_excluded(rel: &Path, excludes: &[String]) -> bool {
    // Normalize the candidate once (S3): a raw `./dist` pattern, a Windows `a\b` pattern or a
    // `dist/`-style trailing slash used to miss silently, letting excluded files get synced.
    let rel_str = normalize_match_path(&rel.to_string_lossy());
    let name = rel
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();

    excludes.iter().any(|raw| {
        let pattern = normalize_match_path(raw);
        if pattern.is_empty() {
            return false;
        }
        if pattern.contains('/') {
            rel_str == pattern || rel_str.starts_with(&format!("{pattern}/"))
        } else {
            name == pattern
        }
    })
}

/// Safety gate for clean mode: never delete the base dir, home dir or root.
fn guard_clean_target(client: &Client, remote_dir: &str, lang: Lang) -> Result<(), String> {
    let target = remote_dir.trim().trim_end_matches('/');
    let base = client.base_dir.trim().trim_end_matches('/');

    if target.is_empty() || matches!(target, "~" | "/" | "." | "..") {
        return Err(i18n::sync_clean_refused(lang, remote_dir));
    }
    if !base.is_empty() && target == base {
        return Err(i18n::sync_clean_base_dir_refused(lang, remote_dir));
    }
    Ok(())
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::{AuthMethod, ClientAuth};
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cgw-sync-{name}"))
    }

    /// Shebang scripts: CRLF is normalized to LF before push, otherwise Mac reports
    /// `/bin/sh^M: no such file or directory` (acceptance record §4.4).
    #[test]
    fn pushes_scripts_with_lf_and_leaves_binaries_alone() {
        let src = tmp("crlf-src");
        let _ = fs::remove_dir_all(&src);
        fs::create_dir_all(&src).unwrap();

        // A gradlew checked out on Windows: shebang + CRLF
        fs::write(
            src.join("gradlew"),
            b"#!/bin/sh\r\necho hi\r\n".as_slice(),
        )
        .unwrap();
        // This binary happens to contain 0x0d 0x0a: it is **not** a script, touch no byte
        let binary: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        fs::write(src.join("logo.png"), &binary).unwrap();
        // Text without a shebang is untouched too (CRLF may be intentional)
        fs::write(src.join("notes.txt"), b"a\r\nb\r\n".as_slice()).unwrap();

        let (archive, stats) = build_archive_opts(&src, &[], false, Lang::EnUS).unwrap();
        assert_eq!(stats.crlf_normalized, 1, "only that one script should be normalized");
        assert!(
            stats.describe(Lang::EnUS).contains("CRLF"),
            "{}",
            stats.describe(Lang::EnUS)
        );

        let dst = tmp("crlf-dst");
        let _ = fs::remove_dir_all(&dst);
        fs::create_dir_all(&dst).unwrap();
        extract_archive(&archive, &dst).unwrap();

        assert_eq!(
            fs::read(dst.join("gradlew")).unwrap(),
            b"#!/bin/sh\necho hi\n".to_vec()
        );
        assert_eq!(fs::read(dst.join("logo.png")).unwrap(), binary, "the binary was modified");
        assert_eq!(
            fs::read(dst.join("notes.txt")).unwrap(),
            b"a\r\nb\r\n".to_vec()
        );

        // No normalization when an as-is push is explicitly requested
        let (_, untouched) = build_archive_opts(&src, &[], true, Lang::EnUS).unwrap();
        assert_eq!(untouched.crlf_normalized, 0);

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    #[test]
    fn normalize_shebang_only_touches_scripts() {
        assert!(normalize_shebang(b"plain\r\ntext\r\n").is_none());
        assert!(normalize_shebang(b"#!/bin/sh\necho ok\n").is_none(), "already LF, do not rebuild");
        assert!(normalize_shebang(b"\xef\xbb\xbf#!/bin/sh\r\n").is_some(), "BOM counts too");
        assert_eq!(
            normalize_shebang(b"#!/bin/sh\r\n\r\nx\r\n").unwrap(),
            b"#!/bin/sh\n\nx\n".to_vec()
        );
    }

    fn sample_client(base_dir: &str) -> Client {
        Client {
            id: "c-test".into(),
            name: "test".into(),
            host: "127.0.0.1".into(),
            port: 22,
            auth: ClientAuth {
                method: AuthMethod::Password,
                username: "u".into(),
                password: "p".into(),
                key_path: String::new(),
                passphrase: String::new(),
            },
            tags: Vec::new(),
            base_dir: base_dir.into(),
            ..Client::default()
        }
    }

    #[test]
    fn archive_round_trip_respects_excludes() {
        let src = tmp("src");
        let dst = tmp("dst");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);

        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("a.txt"), b"hello").unwrap();
        fs::write(src.join("sub/b.txt"), b"world").unwrap();
        fs::write(src.join(".git/config"), b"secret").unwrap();

        let excludes = vec![".git".to_string()];
        let (archive, stats) = build_archive(&src, &excludes).unwrap();
        assert_eq!(stats.files, 2, "only two files should be packed");
        assert_eq!(stats.bytes, 10);
        assert_eq!(stats.links, 0);

        let (out_files, out_bytes) = extract_archive(&archive, &dst).unwrap();
        assert_eq!(out_files, 2);
        assert_eq!(out_bytes, 10);
        assert!(dst.join("a.txt").exists());
        assert!(dst.join("sub/b.txt").exists());
        assert!(!dst.join(".git").exists(), ".git should be excluded");

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    /// S3 regression: a `./`-prefixed or backslash-separated pattern used to miss silently,
    /// so an "excluded" tree was packed anyway. Every spelling must match the same path.
    #[test]
    fn exclude_patterns_are_normalized_before_matching() {
        let cases = [
            "./ios/Pods",
            "ios/Pods",
            "ios/Pods/",
            "ios//Pods",
            ".\\ios\\Pods",
            "/ios/Pods",
        ];
        for pattern in cases {
            let excludes = vec![pattern.to_string()];
            let rel = Path::new("ios/Pods/SDWebImage/README.md");
            assert!(
                is_excluded(rel, &excludes),
                "pattern {pattern:?} should exclude {rel:?}"
            );
        }

        // A bare name still matches by file name, and the directory itself matches too.
        assert!(is_excluded(Path::new("ios/Pods"), &["./ios/Pods".to_string()]));

        // Unrelated paths must not be swallowed.
        assert!(!is_excluded(
            Path::new("ios/AppDelegate.swift"),
            &["./ios/Pods".to_string()]
        ));
    }

    /// S2 regression: extraction must recreate directories that were empty in the source,
    /// which is why the second pass existed. One pass has to keep that behaviour.
    #[test]
    fn extraction_keeps_empty_directories() {
        let src = tmp("emptysrc");
        let dst = tmp("emptydst");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);

        fs::create_dir_all(src.join("empty/nested")).unwrap();
        fs::create_dir_all(src.join("full")).unwrap();
        fs::write(src.join("full/file.txt"), b"x").unwrap();

        let (archive, _) = build_archive(&src, &[]).unwrap();
        extract_archive(&archive, &dst).unwrap();

        assert!(dst.join("full/file.txt").exists());
        assert!(dst.join("empty").is_dir(), "empty dir must survive");
        assert!(dst.join("empty/nested").is_dir(), "nested empty dir must survive");

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    /// Symlinks must travel **as links**; this was a real difference fixed in this round:
    /// the old `continue` broke `node_modules/.bin`, CocoaPods and Flutter's `ios/.symlinks`.
    #[test]
    fn symlinks_survive_the_round_trip() {
        let src = tmp("symsrc");
        let dst = tmp("symdst");
        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);

        fs::create_dir_all(src.join("bin")).unwrap();
        fs::write(src.join("bin/real.txt"), b"content").unwrap();
        fs::write(src.join("plain.txt"), b"plain").unwrap();
        let link = src.join("bin/link.txt");
        if !make_symlink("real.txt", &link) {
            eprintln!("(跳过:这个环境建不出符号链接)");
            let _ = fs::remove_dir_all(&src);
            return;
        }

        let (archive, stats) = build_archive(&src, &[]).unwrap();
        assert_eq!(stats.files, 2, "links do not count as files");
        assert_eq!(stats.bytes, 12);
        assert_eq!(stats.links, 1);
        assert_eq!(stats.outside_links, 0, "points inside the same dir, not outside");
        assert!(
            stats.describe(Lang::EnUS).contains("1 symlinks"),
            "{}",
            stats.describe(Lang::EnUS)
        );

        let (_, out_bytes) = extract_archive(&archive, &dst).unwrap();
        assert_eq!(out_bytes, 12, "the link itself contributes no bytes");
        let back = dst.join("bin/link.txt");
        let meta = fs::symlink_metadata(&back).unwrap();
        assert!(meta.file_type().is_symlink(), "it must come back as a link, not a copy");
        assert_eq!(fs::read_link(&back).unwrap(), std::path::Path::new("real.txt"));

        let _ = fs::remove_dir_all(&src);
        let _ = fs::remove_dir_all(&dst);
    }

    /// Links pointing **outside** the source dir: `outside_links` must count them (informational),
    /// and the target's content must **never** be packed (that would really carry the outside in).
    #[test]
    fn links_pointing_outside_are_reported_but_not_dereferenced() {
        let outside = tmp("symoutside");
        let src = tmp("syminside");
        let dst = tmp("symout-dst");
        for dir in [&outside, &src, &dst] {
            let _ = fs::remove_dir_all(dir);
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(outside.join("secret.txt"), b"top secret").unwrap();
        fs::write(src.join("ok.txt"), b"ok").unwrap();

        let link = src.join("escape.txt");
        if !make_symlink(outside.join("secret.txt"), &link) {
            eprintln!("(跳过:这个环境建不出符号链接)");
            for dir in [&outside, &src, &dst] {
                let _ = fs::remove_dir_all(dir);
            }
            return;
        }

        let (archive, stats) = build_archive(&src, &[]).unwrap();
        assert_eq!(stats.links, 1);
        assert_eq!(stats.outside_links, 1, "links pointing outside must be counted");
        // The archive must not contain the target's content: that is the `follow_symlinks(true)` bug
        let text = String::from_utf8_lossy(&archive);
        assert!(!text.contains("top secret"), "link target content must not be packed");

        extract_archive(&archive, &dst).unwrap();
        let back = dst.join("escape.txt");
        assert!(fs::symlink_metadata(&back).unwrap().file_type().is_symlink());

        for dir in [&outside, &src, &dst] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn link_escape_is_decided_lexically() {
        let source = std::path::Path::new("/proj");
        let link = std::path::Path::new("/proj/sub/link");
        // Same dir / `..` back one level but still inside the project
        assert!(!link_escapes(source, link, std::path::Path::new("a.txt")));
        assert!(!link_escapes(source, link, std::path::Path::new("../a.txt")));
        assert!(!link_escapes(source, link, std::path::Path::new("./x/y")));
        // Escapes above the project
        assert!(link_escapes(source, link, std::path::Path::new("../../a.txt")));
        assert!(link_escapes(
            source,
            std::path::Path::new("/proj/link"),
            std::path::Path::new("../a.txt")
        ));
        // Absolute path
        assert!(link_escapes(source, link, std::path::Path::new("/etc/passwd")));
    }

    /// Create a symlink; returns false where unsupported (needs developer mode on Windows).
    fn make_symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    #[test]
    fn unsafe_archive_paths_are_rejected() {
        // tar's Builder cannot write `..`, so test the predicate directly
        assert!(is_unsafe_archive_path(Path::new("../escape.txt")));
        assert!(is_unsafe_archive_path(Path::new("a/../../escape.txt")));
        assert!(is_unsafe_archive_path(Path::new("/etc/passwd")));
        assert!(!is_unsafe_archive_path(Path::new("a/b/c.txt")));
        assert!(!is_unsafe_archive_path(Path::new("./a.txt")));
    }

    #[test]
    fn clean_guard_protects_base_dir_and_home() {
        let client = sample_client("~/CompileGateway");
        assert!(guard_clean_target(&client, "~/CompileGateway", Lang::EnUS).is_err());
        assert!(guard_clean_target(&client, "~", Lang::EnUS).is_err());
        assert!(guard_clean_target(&client, "/", Lang::EnUS).is_err());
        assert!(guard_clean_target(&client, "./", Lang::EnUS).is_err());
        assert!(guard_clean_target(&client, "~/CompileGateway/spicecompose", Lang::EnUS).is_ok());
    }

    #[test]
    fn excludes_match_names_and_relative_paths() {
        let excludes = vec![".git".to_string(), "ios/Pods".to_string()];
        assert!(is_excluded(Path::new(".git"), &excludes));
        assert!(is_excluded(Path::new("sub/.git"), &excludes));
        assert!(is_excluded(Path::new("ios/Pods"), &excludes));
        assert!(is_excluded(Path::new("ios/Pods/Manifest.lock"), &excludes));
        assert!(!is_excluded(Path::new("ios/Runner.xcodeproj"), &excludes));
    }

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}

