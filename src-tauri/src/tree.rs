//! Project file tree (design doc §7.4).
//!
//! Read-only, one directory per request, expanded level by level; v1 has no
//! search or index. Two hard constraints: (1) the project root is the boundary —
//! `..`, absolute paths and symlink escapes are rejected via
//! [`crate::paths::normalize_relative`]/[`crate::paths::confine_local`] on the
//! host, plus a `pwd -P` prefix check remotely; (2) arbitrary shell arguments
//! are never spliced into `ls` — the remote command is a fixed script that takes
//! the path as `"$1"`/`"$2"` positional arguments.

use std::path::{Path, PathBuf};

use crate::clients::Client;
use crate::paths::{self, Side};
use crate::ssh::{Cancel, Session};

/// Max entries returned at once — keep huge directories from flooding the page.
pub const MAX_ENTRIES: usize = 2000;
pub const TREE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Error markers used by the remote script so they cannot mix with normal output.
const ERR_NO_ROOT: &str = "__CGW_ERR_ROOT__";
const ERR_NO_DIR: &str = "__CGW_ERR_DIR__";
const ERR_ESCAPE: &str = "__CGW_ERR_ESCAPE__";

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    /// Path relative to the project root (the "relative path" of §7.4).
    pub path: String,
    /// `dir` / `file` / `symlink` / `other`
    pub kind: String,
    pub size: u64,
    /// Unix seconds; 0 when unavailable.
    pub modified_at: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tree {
    pub side: String,
    /// Current directory relative to the root ("" = root).
    pub path: String,
    /// Resolved root (remote: result of `pwd -P`; host: local absolute path).
    pub root: String,
    /// Parent directory; null at the root.
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    /// Entries were cut off by [`MAX_ENTRIES`].
    pub truncated: bool,
    pub message: String,
}

#[derive(Debug)]
pub struct TreeError {
    /// Stable error code, for the API.
    pub code: &'static str,
    pub message: String,
}

impl TreeError {
    fn new(code: &'static str, message: impl Into<String>) -> TreeError {
        TreeError {
            code,
            message: message.into(),
        }
    }
}

/// List the `relative` directory on the `side` side.
pub async fn list(
    client: &Client,
    project_remote_root: &str,
    host_root: &Path,
    side: Side,
    relative: &str,
    cancel: &Cancel,
) -> Result<Tree, TreeError> {
    // Empty = the root itself (the most common call)
    let rel = if relative.trim().is_empty() {
        String::new()
    } else {
        paths::normalize_relative(relative).map_err(|err| TreeError::new("badPath", err))?
    };
    let parent = parent_of(&rel);

    if side.is_remote() {
        list_remote(client, project_remote_root, &rel, parent, cancel).await
    } else {
        list_host(host_root, &rel, parent)
    }
}

/// Relative path of the parent; None at the root.
fn parent_of(rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    Some(match rel.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    })
}

// ------------------------------------------------------------------ remote

/// Fixed remote script — the path appears only as `$1` / `$2`, never spliced
/// into the command text.
pub const REMOTE_SCRIPT: &str = r#"
root=$1
rel=$2
root_real=$(cd "$root" 2>/dev/null && pwd -P) || { echo "__CGW_ERR_ROOT__"; exit 66; }
if [ -n "$rel" ]; then dir="$root/$rel"; else dir="$root"; fi
cd "$dir" 2>/dev/null || { echo "__CGW_ERR_DIR__"; exit 66; }
here=$(pwd -P)
case "$here" in
  "$root_real"|"$root_real"/*) ;;
  *) echo "__CGW_ERR_ESCAPE__"; exit 66 ;;
esac
echo "__CGW_ROOT__$root_real"
# GNU stat and BSD stat print differently; probe first
if stat -c '%s' . >/dev/null 2>&1; then
  cgw_stat() { stat -c '%F|%s|%Y' -- "$1" 2>/dev/null; }
else
  cgw_stat() { stat -f '%HT|%z|%m' -- "$1" 2>/dev/null; }
fi
for f in * .[!.]* ..?*; do
  [ -e "$f" ] || [ -L "$f" ] || continue
  info=$(cgw_stat "$f")
  [ -n "$info" ] || continue
  printf '%s\t%s\n' "$info" "$f"
done
"#;

/// Remote directory listing command. The path is a positional argument, never
/// spliced into the script.
pub fn remote_command(root: &str, rel: &str) -> String {
    format!(
        "sh -c {} cgw-tree {} {}",
        crate::manifest::shell_quote(REMOTE_SCRIPT),
        crate::manifest::shell_quote(root),
        crate::manifest::shell_quote(rel),
    )
}

async fn list_remote(
    client: &Client,
    root: &str,
    rel: &str,
    parent: Option<String>,
    cancel: &Cancel,
) -> Result<Tree, TreeError> {
    let mut session = Session::open_cancellable(client, cancel)
        .await
        .map_err(|err| TreeError::new("sshFailed", err))?;

    let command = remote_command(root, rel);
    let exec = match tokio::time::timeout(
        TREE_TIMEOUT,
        session.exec_collect_cancellable(&command, cancel),
    )
    .await
    {
        Ok(Ok(exec)) => exec,
        Ok(Err(err)) => return Err(TreeError::new("sshFailed", err)),
        Err(_) => {
            return Err(TreeError::new(
                "timeout",
                format!("列目录超时({}s)", TREE_TIMEOUT.as_secs()),
            ))
        }
    };

    let text = exec.combined();
    for (mark, code, message) in [
        (ERR_NO_ROOT, "noRoot", "项目根目录在目标机上不存在"),
        (ERR_NO_DIR, "noDir", "目录不存在"),
        (ERR_ESCAPE, "escape", "该路径解析后跑到了项目根目录之外(符号链接),拒绝访问"),
    ] {
        if text.contains(mark) {
            return Err(TreeError::new(code, format!("{message}:{rel}")));
        }
    }

    let mut resolved = String::new();
    let mut entries = Vec::new();
    let mut truncated = false;

    for line in text.lines() {
        if let Some(value) = line.strip_prefix("__CGW_ROOT__") {
            resolved = value.trim().to_string();
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            continue;
        }
        if let Some(entry) = parse_entry(line, rel) {
            entries.push(entry);
        }
    }

    entries.sort_by(|a, b| {
        // Directories first, then by name
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Tree {
        side: Side::Remote.code().to_string(),
        path: rel.to_string(),
        root: resolved,
        parent,
        entries,
        truncated,
        message: String::new(),
    })
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "dir" => 0,
        "file" => 1,
        _ => 2,
    }
}

/// One line `<kind>|<size>|<mtime>\t<name>`.
fn parse_entry(line: &str, rel: &str) -> Option<Entry> {
    let (info, name) = line.split_once('\t')?;
    let mut parts = info.split('|');
    let kind_raw = parts.next().unwrap_or("").trim();
    let size = parts.next().unwrap_or("0").trim().parse::<u64>().unwrap_or(0);
    let modified = parts.next().unwrap_or("0").trim().parse::<u64>().unwrap_or(0);

    let name = name.to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    let lower = kind_raw.to_ascii_lowercase();
    let kind = if lower.contains("directory") || lower == "dir" {
        "dir"
    } else if lower.contains("symbolic link") || lower.contains("link") {
        "symlink"
    } else if lower.contains("regular") || lower.contains("file") {
        "file"
    } else {
        "other"
    };

    Some(Entry {
        path: if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        },
        name,
        kind: kind.to_string(),
        size,
        modified_at: modified,
    })
}

// ------------------------------------------------------------------ controlled file read (design doc §7.4)

/// Max bytes read at once. §7.4 says "file contents are not returned by default;
/// add a controlled read/download API if text viewing is needed" — this is that
/// API: capped, and it **says** when it truncated.
pub const MAX_READ_BYTES: u64 = 256 * 1024;

/// Decide whether this "does not look like text": a NUL byte, or too low a
/// share of printable characters.
///
/// NUL alone misses binaries such as JPEG/zip with no NUL; the ratio alone
/// flags some text encodings. The union of both errs toward "treat as binary"
/// (better to show nothing).
pub fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return true;
    }
    if bytes.is_empty() {
        return false;
    }
    // Look at the first 8 KB only: later bytes add little to the text/binary
    // call, and scanning a whole large file would be wasted work
    let sample = &bytes[..bytes.len().min(8192)];
    let printable = sample
        .iter()
        .filter(|byte| {
            matches!(**byte, 0x09 | 0x0a | 0x0d) || (0x20..0x7f).contains(*byte) || **byte >= 0x80
        })
        .count();
    // Every byte of a multi-byte UTF-8 char is >= 0x80, so CJK text is not misjudged
    printable * 100 / sample.len() < 90
}

/// Content read back.
#[derive(Debug, Clone)]
pub struct FileText {
    pub side: String,
    /// Path relative to the project root.
    pub path: String,
    /// Bytes actually read (may be below the file size — see `truncated`).
    pub bytes: u64,
    /// Real file size.
    pub size: u64,
    /// Above the cap, so only the leading part was read.
    pub truncated: bool,
    /// Looks binary, so `text` is empty.
    pub binary: bool,
    pub text: String,
}

/// Remote file read script. Same pattern as the listing: the path appears only
/// as `$1`/`$2`, first confirm we are **inside the project root** (`pwd -P`
/// prefix check, blocking symlink escapes), then cap with `head -c`.
pub const READ_SCRIPT: &str = r#"
root=$1
rel=$2
limit=$3
root_real=$(cd "$root" 2>/dev/null && pwd -P) || { echo "__CGW_ERR_ROOT__"; exit 66; }
[ -n "$rel" ] || { echo "__CGW_ERR_REL__"; exit 66; }
full="$root/$rel"
dir=$(dirname -- "$full")
base=$(basename -- "$full")
cd "$dir" 2>/dev/null || { echo "__CGW_ERR_DIR__"; exit 66; }
here=$(pwd -P)
case "$here" in
  "$root_real"|"$root_real"/*) ;;
  *) echo "__CGW_ERR_ESCAPE__"; exit 66 ;;
esac
[ -f "$base" ] || { echo "__CGW_ERR_NOTFILE__"; exit 66; }
# GNU stat and BSD stat print differently; probe first (same trick as the listing script)
if stat -c '%s' . >/dev/null 2>&1; then
  cgw_size() { stat -c '%s' -- "$1" 2>/dev/null; }
else
  cgw_size() { stat -f '%z' -- "$1" 2>/dev/null; }
fi
size=$(cgw_size "$base")
echo "__CGW_SIZE__$size"
head -c "$limit" -- "$base"
"#;

#[cfg(test)]
mod read_tests {
    use super::*;

    #[test]
    fn detects_binary_content() {
        assert!(looks_binary(b"abc\0def"));
        assert!(looks_binary(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10])); // JPEG header (with NUL)
        assert!(!looks_binary(b""));
        assert!(!looks_binary(b"hello world\n"));
        assert!(!looks_binary("中文内容测试\n".as_bytes()));
        // A "binary without NUL" built from control characters
        assert!(looks_binary(&[0x01u8; 100]));
    }

    #[test]
    fn read_script_guards_the_root_boundary() {
        assert!(READ_SCRIPT.contains("root=$1"));
        assert!(READ_SCRIPT.contains("rel=$2"));
        assert!(READ_SCRIPT.contains("limit=$3"));
        assert!(READ_SCRIPT.contains("pwd -P"));
        assert!(READ_SCRIPT.contains("__CGW_ERR_ESCAPE__"));
        assert!(READ_SCRIPT.contains("[ -f \"$base\" ]"));
        assert!(READ_SCRIPT.contains("head -c"));
    }
}

/// Read one file in a controlled way (the "later controlled read API" of §7.4).
///
/// The same three boundaries as listing:
/// * the path is normalized first (rejecting `..` / absolute paths), then
///   confined to the project root;
/// * the host compares the prefix after resolving symlinks, the remote side
///   compares `pwd -P` after `cd` in the shell;
/// * at most [`MAX_READ_BYTES`] bytes, and **truncation is reported**
///   (`truncated`).
///
/// When the content is not text (`looks_binary`) only metadata is returned —
/// stuffing binary into JSON is pointless, wastes bandwidth and can stall the
/// front end.
pub async fn read(
    client: &Client,
    project_remote_root: &str,
    host_root: &Path,
    side: Side,
    relative: &str,
    cancel: &Cancel,
) -> Result<FileText, TreeError> {
    let rel = paths::normalize_relative(relative).map_err(|err| TreeError::new("badPath", err))?;
    if rel.is_empty() {
        return Err(TreeError::new("badPath", "要读的是文件路径,不能是项目根目录本身"));
    }

    let (bytes, size) = if side.is_remote() {
        read_remote(client, project_remote_root, &rel, cancel).await?
    } else {
        read_host(host_root, &rel)?
    };

    let truncated = size > bytes.len() as u64;
    let binary = looks_binary(&bytes);
    Ok(FileText {
        side: side.code().to_string(),
        path: rel,
        bytes: bytes.len() as u64,
        size,
        truncated,
        binary,
        text: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&bytes).to_string()
        },
    })
}

fn read_host(root: &Path, rel: &str) -> Result<(Vec<u8>, u64), TreeError> {
    let path = paths::confine_local(root, rel).map_err(|err| match err {
        paths::PathError::Escape(message) => TreeError::new("escape", message),
        paths::PathError::Syntax(message) => TreeError::new("badPath", message),
    })?;
    let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let resolved = std::fs::canonicalize(&path)
        .map_err(|err| TreeError::new("noFile", format!("读不到 {rel}:{err}")))?;
    if !resolved.starts_with(&resolved_root) {
        return Err(TreeError::new(
            "escape",
            format!("该路径解析后跑到了项目根目录之外(符号链接),拒绝读取:{rel}"),
        ));
    }
    let meta = std::fs::metadata(&resolved)
        .map_err(|err| TreeError::new("noFile", format!("读不到 {rel}:{err}")))?;
    if !meta.is_file() {
        return Err(TreeError::new("notFile", format!("不是普通文件:{rel}")));
    }
    let size = meta.len();
    let file = std::fs::File::open(&resolved)
        .map_err(|err| TreeError::new("noFile", format!("{}: {err}", resolved.display())))?;
    // Only the first MAX_READ_BYTES bytes, never the whole file in memory
    let mut buffer = Vec::new();
    use std::io::Read;
    std::io::Read::take(file, MAX_READ_BYTES)
        .read_to_end(&mut buffer)
        .map_err(|err| TreeError::new("readFailed", format!("{}: {err}", resolved.display())))?;
    Ok((buffer, size))
}

async fn read_remote(
    client: &Client,
    root: &str,
    rel: &str,
    cancel: &Cancel,
) -> Result<(Vec<u8>, u64), TreeError> {
    let mut session = Session::open_cancellable(client, cancel)
        .await
        .map_err(|err| TreeError::new("sshFailed", err))?;

    let command = format!(
        "sh -c {} -- {} {} {}",
        crate::manifest::shell_quote(READ_SCRIPT),
        crate::manifest::shell_quote(root),
        crate::manifest::shell_quote(rel),
        MAX_READ_BYTES,
    );
    let exec = match tokio::time::timeout(
        TREE_TIMEOUT,
        session.exec_collect_cancellable(&command, cancel),
    )
    .await
    {
        Ok(Ok(exec)) => exec,
        Ok(Err(err)) => return Err(TreeError::new("sshFailed", err)),
        Err(_) => return Err(TreeError::new("sshFailed", "读取超时")),
    };
    if exec.cancelled {
        return Err(TreeError::new("cancelled", "已取消"));
    }

    for (marker, code, message) in [
        ("__CGW_ERR_ROOT__", "noRoot", "远程项目根目录不存在"),
        ("__CGW_ERR_REL__", "badPath", "要读的是文件路径"),
        ("__CGW_ERR_DIR__", "noFile", "远程目录不存在"),
        ("__CGW_ERR_ESCAPE__", "escape", "路径跑到了项目根目录之外,拒绝读取"),
        ("__CGW_ERR_NOTFILE__", "notFile", "不是普通文件"),
    ] {
        if exec.stdout.starts_with(marker.as_bytes()) {
            return Err(TreeError::new(code, message));
        }
    }

    // First line is `__CGW_SIZE__<size>`, the rest is content
    let split = exec
        .stdout
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| TreeError::new("readFailed", "远程没回大小"))?;
    let head = String::from_utf8_lossy(&exec.stdout[..split]).to_string();
    let size: u64 = head
        .strip_prefix("__CGW_SIZE__")
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| TreeError::new("readFailed", format!("远程没回大小:{head}")))?;
    Ok((exec.stdout[split + 1..].to_vec(), size))
}

// ------------------------------------------------------------------ host

fn list_host(root: &Path, rel: &str, parent: Option<String>) -> Result<Tree, TreeError> {
    let dir = paths::confine_local(root, rel).map_err(|err| match err {
        paths::PathError::Escape(message) => TreeError::new("escape", message),
        paths::PathError::Syntax(message) => TreeError::new("badPath", message),
    })?;

    let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let resolved = std::fs::canonicalize(&dir).map_err(|err| {
        // Root missing vs root present but subdirectory missing: different codes
        if resolved_root.is_dir() {
            TreeError::new("noDir", format!("目录不存在:{}", dir.display()))
        } else {
            TreeError::new("noRoot", format!("项目根目录不存在:{}", err))
        }
    })?;

    if !resolved.starts_with(&resolved_root) {
        return Err(TreeError::new(
            "escape",
            format!("该路径解析后跑到了项目根目录之外(符号链接),拒绝访问:{rel}"),
        ));
    }

    let read = std::fs::read_dir(&resolved)
        .map_err(|err| TreeError::new("noDir", format!("{}: {err}", resolved.display())))?;

    let mut entries = Vec::new();
    let mut truncated = false;
    for item in read.flatten() {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = item.file_name().to_string_lossy().to_string();
        // Following symlinks would expose metadata outside the root, so use
        // symlink_metadata
        let Ok(meta) = item.metadata().and_then(|_| std::fs::symlink_metadata(item.path())) else {
            continue;
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(Entry {
            path: if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            },
            name,
            kind: kind.to_string(),
            size: if kind == "file" { meta.len() } else { 0 },
            modified_at: modified,
        });
    }

    entries.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(Tree {
        side: Side::Host.code().to_string(),
        path: rel.to_string(),
        root: paths::display(&resolved_root),
        parent,
        entries,
        truncated,
        message: String::new(),
    })
}

/// Absolute location of a relative path on the host (for "start a task with
/// this path").
pub fn host_absolute(host_root: &Path, relative: &str) -> Result<PathBuf, String> {
    Ok(paths::confine_local(host_root, relative)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_never_splices_the_path_into_the_script() {
        let hostile = "a b'; rm -rf /; echo '";
        let command = remote_command("~/Work", hostile);

        // Command shape: `sh -c '<script>' cgw-tree '<root>' '<rel>'`
        assert!(command.starts_with("sh -c "), "{command}");
        assert!(command.contains(" cgw-tree "), "{command}");

        // The hostile path appears **only** as a single-quoted argument, with its
        // quotes escaped as '\''
        let escaped = format!("'{}'", hostile.replace('\'', r"'\''"));
        assert_eq!(
            command.matches(&escaped).count(),
            1,
            "the hostile path must appear exactly once as an escaped argument: {command}"
        );
        // No bare path fragment besides that
        assert!(
            !command.contains(&format!(" {hostile}")),
            "the path must never appear unquoted: {command}"
        );

        // The script body holds only positional parameters, no path
        assert!(REMOTE_SCRIPT.contains("root=$1"));
        assert!(REMOTE_SCRIPT.contains("rel=$2"));
        assert!(REMOTE_SCRIPT.contains("\"$root/$rel\""));
        assert!(!REMOTE_SCRIPT.contains(hostile));
    }

    #[test]
    fn remote_script_guards_the_root_boundary() {
        // Boundary check: compare pwd -P after entering the directory
        assert!(REMOTE_SCRIPT.contains("pwd -P"));
        assert!(REMOTE_SCRIPT.contains("__CGW_ERR_ESCAPE__"));
        assert!(REMOTE_SCRIPT.contains("$root_real\"/*"));
        // Both GNU and BSD stat must be handled
        assert!(REMOTE_SCRIPT.contains("stat -c"));
        assert!(REMOTE_SCRIPT.contains("stat -f"));
        // Must not follow symlinks
        assert!(REMOTE_SCRIPT.contains("[ -L "));
    }

    #[test]
    fn parses_posix_and_bsd_stat_output() {
        let gnu = "directory|4096|1700000000\tiosApp";
        let entry = parse_entry(gnu, "src").unwrap();
        assert_eq!(entry.name, "iosApp");
        assert_eq!(entry.path, "src/iosApp");
        assert_eq!(entry.kind, "dir");
        assert_eq!(entry.modified_at, 1_700_000_000);

        let bsd = "Regular File|1234|1700000001\tApp.ipa";
        let entry = parse_entry(bsd, "").unwrap();
        assert_eq!(entry.kind, "file");
        assert_eq!(entry.size, 1234);
        assert_eq!(entry.path, "App.ipa");

        let link = "Symbolic Link|11|1700000002\tlink";
        assert_eq!(parse_entry(link, "a").unwrap().kind, "symlink");

        // Spaces, pipes and the part after a tab in the name must be kept as-is
        let odd = "Regular File|1|1\tmy file (1).txt";
        assert_eq!(parse_entry(odd, "").unwrap().name, "my file (1).txt");

        assert!(parse_entry("garbage", "").is_none());
        assert!(parse_entry("Regular File|1|1\t", "").is_none());
        assert!(parse_entry("Regular File|1|1\t..", "").is_none());
    }

    #[test]
    fn parent_walks_up_to_the_root() {
        assert_eq!(parent_of(""), None);
        assert_eq!(parent_of("a"), Some(String::new()));
        assert_eq!(parent_of("a/b"), Some("a".to_string()));
        assert_eq!(parent_of("a/b/c"), Some("a/b".to_string()));
    }

    #[test]
    fn host_listing_stays_inside_the_root() {
        let root = std::env::temp_dir().join("cgw-tree-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"x").unwrap();

        let tree = list_host(&root, "", None).unwrap();
        let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
        // Directories come first
        assert_eq!(names, vec!["sub", "a.txt"]);
        assert_eq!(tree.entries[0].kind, "dir");
        assert_eq!(tree.entries[1].kind, "file");
        assert_eq!(tree.entries[1].size, 5);
        assert!(tree.entries[1].modified_at > 0);
        assert_eq!(tree.entries[1].path, "a.txt");
        assert!(tree.parent.is_none());

        let tree = list_host(&root, "sub", Some(String::new())).unwrap();
        // sub holds deep/ (directory) and b.txt; directories come first
        let names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["deep", "b.txt"]);
        assert_eq!(tree.entries[1].path, "sub/b.txt");
        assert_eq!(tree.parent.as_deref(), Some(""));

        // Escapes are always rejected: syntax errors and symlink escapes get
        // different codes
        assert_eq!(list_host(&root, "../..", None).unwrap_err().code, "badPath");
        assert_eq!(list_host(&root, "/etc", None).unwrap_err().code, "badPath");
        assert_eq!(list_host(&root, "nope", None).unwrap_err().code, "noDir");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The host-managed directory does not exist on first use — that must still
    /// work, and must create nested subdirectories.
    #[test]
    fn host_root_listing_and_missing_subdirectory_are_distinguished() {
        let base = std::env::temp_dir().join("cgw-tree-missing");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Root missing -> noRoot
        let root = base.join("CompileGateway/proj");
        assert_eq!(list_host(&root, "", None).unwrap_err().code, "noRoot");

        // Root present, subdirectory missing -> noDir
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(list_host(&root, "nope/deep", None).unwrap_err().code, "noDir");

        // The root itself can be listed (empty path is the common call)
        let tree = list_host(&root, "", None).unwrap();
        assert!(tree.entries.is_empty());
        assert!(!tree.root.starts_with(r"\\?\"), "the Windows verbatim prefix must not leak out: {}", tree.root);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn entries_are_capped() {
        let root = std::env::temp_dir().join("cgw-tree-cap");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..(MAX_ENTRIES + 20) {
            std::fs::write(root.join(format!("f{index:05}")), b"").unwrap();
        }
        let tree = list_host(&root, "", None).unwrap();
        assert_eq!(tree.entries.len(), MAX_ENTRIES);
        assert!(tree.truncated);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn host_listing_marks_symlinks_without_following_them() {
        let root = std::env::temp_dir().join("cgw-tree-link");
        let outside = std::env::temp_dir().join("cgw-tree-outside");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();

        let link = root.join("escape");
        if !make_symlink(&outside, &link) {
            // Without permission to create symlinks, skip (needs developer mode
            // on Windows)
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }

        let tree = list_host(&root, "", None).unwrap();
        let entry = tree.entries.iter().find(|e| e.name == "escape").unwrap();
        assert_eq!(entry.kind, "symlink", "symlinks must be reported as such");
        // Descending through the link must be rejected
        let err = list_host(&root, "escape", None).unwrap_err();
        assert_eq!(err.code, "escape");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
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
