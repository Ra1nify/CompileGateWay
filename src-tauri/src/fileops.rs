//! In-project file tasks: copy / move / delete / export (design doc §7.1 §7.2).
//!
//! Each op side is `remote` (project root on the client, run over SSH) or `host`
//! (daemon-side managed dir, local filesystem). Same-side ops use the target's own
//! `cp -a` / `mv` / `rm -rf`; cross-side ops use **tar over SSH**, and a cross-side
//! `move` degrades to "copy, then delete source". Paths are bounded by [`crate::paths`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use walkdir::WalkDir;

use crate::clients::Client;
use crate::log;
use crate::paths::{self, Location, Side};
use crate::ssh::{shell_path, Cancel, Session};
use crate::sync;
use crate::zip;

/// Per cross-side transfer limit, same as [`crate::sync`].
const MAX_ARCHIVE_BYTES: usize = 512 * 1024 * 1024;

/// The two roots.
pub struct Roots {
    /// Project root on the client (usually `~/CompileGateway/<slug>`).
    pub remote: String,
    /// Daemon-side host managed directory.
    pub host: PathBuf,
}

pub struct OpOutcome {
    pub ok: bool,
    pub cancelled: bool,
    pub message: String,
    /// Paths this op touched (recorded in the job step log, design doc §7.3).
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// How many files and bytes were moved (previously `job.files` / `job.bytes` stayed 0).
    pub files: u64,
    pub bytes: u64,
}

/// Marker line in remote output that reports the counts.
const COUNT_MARK: &str = "__CGW_COUNT__";

/// Count suffix appended to remote ops: counted in the same SSH round trip.
///
/// Bytes come from `du -sk` (1K blocks), so they are approximate; the file count is exact.
fn remote_count_suffix(path: &str) -> String {
    let quoted = shell_path(path);
    format!(
        "; printf '{COUNT_MARK} %s %s\\n' \
         \"$(find {quoted} -type f 2>/dev/null | wc -l | tr -d ' ')\" \
         \"$(du -sk {quoted} 2>/dev/null | cut -f1 | tr -d ' ')\""
    )
}

fn parse_remote_count(stdout: &str) -> (u64, u64) {
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix(COUNT_MARK) {
            let mut parts = rest.split_whitespace();
            let files = parts.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let kb = parts.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            return (files, kb * 1024);
        }
    }
    (0, 0)
}

impl OpOutcome {
    fn fail(message: impl Into<String>) -> OpOutcome {
        OpOutcome {
            ok: false,
            cancelled: false,
            message: message.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            files: 0,
            bytes: 0,
        }
    }

    fn cancelled() -> OpOutcome {
        OpOutcome {
            ok: false,
            cancelled: true,
            message: "已被用户取消".to_string(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            files: 0,
            bytes: 0,
        }
    }

    fn ok(message: impl Into<String>, inputs: Vec<String>, outputs: Vec<String>) -> OpOutcome {
        OpOutcome {
            ok: true,
            cancelled: false,
            message: message.into(),
            inputs,
            outputs,
            files: 0,
            bytes: 0,
        }
    }

    /// Attach the counts where known; leave 0 where unknown instead of guessing.
    fn with_counts(mut self, files: u64, bytes: u64) -> OpOutcome {
        self.files = files;
        self.bytes = bytes;
        self
    }
}

/// Log sink: writes to both the job scope and the instance scope.
fn say(scopes: &[String], level: &'static str, message: impl Into<String>) {
    if !scopes.is_empty() {
        log::log_multi(scopes, level, message.into());
    }
}

// ------------------------------------------------------------------ Entry points

pub async fn copy(
    client: &Client,
    roots: &Roots,
    from: &Location,
    to: &Location,
    overwrite: bool,
    timeout: Duration,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    match tokio::time::timeout(
        timeout,
        transfer(client, roots, from, to, overwrite, false, cancel, scopes),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => OpOutcome::fail(format!("操作超时({}s)", timeout.as_secs())),
    }
}

pub async fn move_path(
    client: &Client,
    roots: &Roots,
    from: &Location,
    to: &Location,
    overwrite: bool,
    timeout: Duration,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    match tokio::time::timeout(
        timeout,
        transfer(client, roots, from, to, overwrite, true, cancel, scopes),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => OpOutcome::fail(format!("操作超时({}s)", timeout.as_secs())),
    }
}

pub async fn delete(
    client: &Client,
    roots: &Roots,
    target: &Location,
    timeout: Duration,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    match tokio::time::timeout(timeout, delete_inner(client, roots, target, cancel, scopes)).await {
        Ok(outcome) => outcome,
        Err(_) => OpOutcome::fail(format!("操作超时({}s)", timeout.as_secs())),
    }
}

pub async fn export(
    client: &Client,
    roots: &Roots,
    sources: &[Location],
    output_name: &str,
    timeout: Duration,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    match tokio::time::timeout(
        timeout,
        export_inner(client, roots, sources, output_name, cancel, scopes),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => OpOutcome::fail(format!("操作超时({}s)", timeout.as_secs())),
    }
}

// ------------------------------------------------------------------ Copy / move

#[allow(clippy::too_many_arguments)]
async fn transfer(
    client: &Client,
    roots: &Roots,
    from: &Location,
    to: &Location,
    overwrite: bool,
    remove_source: bool,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    let from_rel = match paths::normalize_relative(&from.path) {
        Ok(value) => value,
        Err(err) => return OpOutcome::fail(format!("from 不合法:{err}")),
    };
    let to_rel = match paths::normalize_relative(&to.path) {
        Ok(value) => value,
        Err(err) => return OpOutcome::fail(format!("to 不合法:{err}")),
    };
    if from_rel.is_empty() {
        return OpOutcome::fail("from 不能是根目录本身");
    }
    if to_rel.is_empty() {
        return OpOutcome::fail("to 不能是根目录本身");
    }

    // Reject same-side "move onto itself" so cp/mv cannot nest a dir inside itself.
    if from.side == to.side && (to_rel == from_rel || to_rel.starts_with(&format!("{from_rel}/"))) {
        return OpOutcome::fail(format!(
            "目标 {to_rel} 位于源 {from_rel} 之内,拒绝执行"
        ));
    }

    let label = if remove_source { "剪切" } else { "复制" };
    say(
        scopes,
        log::INFO,
        format!(
            "{label}: {}:{} -> {}:{}",
            from.side.code(),
            from_rel,
            to.side.code(),
            to_rel
        ),
    );

    let inputs = vec![format!("{}:{}", from.side.code(), from_rel)];
    let outputs = vec![format!("{}:{}", to.side.code(), to_rel)];

    match (from.side, to.side) {
        (Side::Remote, Side::Remote) => {
            // Count in the same SSH round trip, no extra round trip
            let counted = format!(
                "{} && cd {} && {}",
                remote_transfer_command(roots, &from_rel, &to_rel, overwrite, remove_source),
                shell_path(&roots.remote),
                remote_count_suffix(&to_rel).trim_start_matches("; "),
            );
            match run_remote(client, &counted, cancel).await {
                Ok(exec) if exec.cancelled => OpOutcome::cancelled(),
                Ok(exec) if exec.ok() => {
                    let (files, bytes) = parse_remote_count(&exec.combined());
                    OpOutcome::ok(
                        format!(
                            "{label}完成:{} -> {}({files} 个文件 / {})",
                            from_rel,
                            to_rel,
                            sync::human_bytes(bytes)
                        ),
                        inputs,
                        outputs,
                    )
                    .with_counts(files, bytes)
                }
                Ok(exec) => OpOutcome::fail(format!(
                    "远端{label}失败({}):{}",
                    exec.exit_note(),
                    exec.combined()
                )),
                Err(err) => OpOutcome::fail(err),
            }
        }
        (Side::Host, Side::Host) => {
            let from_abs = match paths::confine_local(&roots.host, &from_rel) {
                Ok(path) => path,
                Err(err) => return OpOutcome::fail(err),
            };
            let to_abs = match paths::confine_local(&roots.host, &to_rel) {
                Ok(path) => path,
                Err(err) => return OpOutcome::fail(err),
            };
            match host_transfer_blocking(from_abs, to_abs, overwrite, remove_source).await {
                Ok((files, bytes)) => OpOutcome::ok(
                    format!(
                        "{label}完成:{} -> {}({files} 个文件 / {})",
                        from_rel,
                        to_rel,
                        sync::human_bytes(bytes)
                    ),
                    inputs,
                    outputs,
                )
                .with_counts(files, bytes),
                Err(err) => OpOutcome::fail(err),
            }
        }
        // Cross-side: tar over SSH, then remove the source (move only).
        _ => {
            say(
                scopes,
                log::INFO,
                format!("跨侧{label}走 tar over SSH"),
            );
            match cross_side(client, roots, from.side, &from_rel, to.side, &to_rel, overwrite, cancel)
                .await
            {
                Err(outcome) => outcome,
                Ok((files, bytes)) => {
                    if remove_source {
                        match remove_one(client, roots, from.side, &from_rel, cancel).await {
                            Ok(()) => {}
                            Err(err) => {
                                return OpOutcome::fail(format!(
                                    "已复制到目标,但删除源失败:{err}"
                                ))
                            }
                        }
                    }
                    OpOutcome::ok(
                        format!(
                            "{label}完成(跨侧):{} -> {}({files} 个文件 / {})",
                            from_rel,
                            to_rel,
                            sync::human_bytes(bytes)
                        ),
                        inputs,
                        outputs,
                    )
                    .with_counts(files, bytes)
                }
            }
        }
    }
}

/// Remote same-side copy/move: relative paths after `cd <root>`; `..` is already rejected.
fn remote_transfer_command(
    roots: &Roots,
    from_rel: &str,
    to_rel: &str,
    overwrite: bool,
    remove_source: bool,
) -> String {
    let root = shell_path(&roots.remote);
    let from = shell_path(from_rel);
    let to = shell_path(to_rel);
    let parent = match to_rel.rsplit_once('/') {
        Some((dir, _)) => shell_path(dir),
        None => ".".to_string(),
    };

    let mut parts: Vec<String> = vec![format!("cd {root}")];
    parts.push(format!(
        "[ -e {from} ] || {{ echo 'cgw: 源不存在: {from_rel}' >&2; exit 66; }}"
    ));
    if !overwrite {
        parts.push(format!(
            "[ ! -e {to} ] || {{ echo 'cgw: 目标已存在(未勾选覆盖): {to_rel}' >&2; exit 17; }}"
        ));
    }
    parts.push(format!("mkdir -p {parent}"));
    if overwrite {
        parts.push(format!("rm -rf {to}"));
    }
    let verb = if remove_source { "mv" } else { "cp -a" };
    parts.push(format!("{verb} -- {from} {to}"));
    parts.join(" && ")
}

/// Remote temp dir name used for host -> remote uploads.
///
/// A fixed name is safe: one job per project, and each project has its own remote root.
/// The `.cgw` prefix keeps it from ever being pushed back (see `NEVER_PUSH`).
const CROSS_TMP_DIR: &str = ".cgw-copy-tmp";

/// host -> remote upload command: unpack into a remote temp dir, then move the single
/// top-level entry to `to`.
///
/// `to` is the full destination path (it may rename), matching same-side `cp -a` / `mv`.
/// The archive comes from `pack_path`; its top-level entry is `basename(from_rel)`.
fn upload_command(remote_root: &str, from_rel: &str, to_rel: &str, overwrite: bool) -> String {
    let root = shell_path(remote_root);
    let to = shell_path(to_rel);
    let parent = shell_path(relative_parent(to_rel));
    let base = shell_path(&basename(from_rel));
    let tmp = shell_path(CROSS_TMP_DIR);

    let mut parts = vec![format!("cd {root}")];
    if !overwrite {
        parts.push(format!(
            "[ ! -e {to} ] || {{ echo 'cgw: 目标已存在(未勾选覆盖): {to_rel}' >&2; exit 17; }}"
        ));
    }
    parts.push(format!("mkdir -p {parent}"));
    parts.push(format!("rm -rf {tmp} && mkdir -p {tmp}"));
    parts.push(format!("tar -xzf - -C {tmp}"));
    if overwrite {
        parts.push(format!("rm -rf {to}"));
    }
    parts.push(format!("mv -- {tmp}/{base} {to}"));
    parts.push(format!("rm -rf {tmp}"));
    parts.join(" && ")
}

/// Host-side same-side copy/move.
fn host_transfer(
    from: &Path,
    to: &Path,
    overwrite: bool,
    remove_source: bool,
) -> Result<(u64, u64), String> {
    if !from.exists() {
        return Err(format!("源不存在:{}", from.display()));
    }
    if to.exists() {
        if !overwrite {
            return Err(format!("目标已存在(未勾选覆盖):{}", to.display()));
        }
        remove_path(to)?;
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 {} 失败:{e}", parent.display()))?;
    }

    // Only a real move may use rename (instant on the same volume); copy must not,
    // since rename would move the source instead of leaving a copy.
    if remove_source {
        // Count before rename: rename moves no data, but the logged "N files / X" must be real.
        let counted = measure(from);
        if std::fs::rename(from, to).is_ok() {
            return Ok(counted);
        }
    }

    let (files, bytes, _links) = copy_tree(from, to)?;
    if remove_source {
        remove_path(from)?;
    }
    Ok((files, bytes))
}

/// [`host_transfer`] off the tokio worker.
///
/// A host-side copy/move walks and rewrites whole trees; running that inline in an `async fn`
/// blocks a runtime worker and freezes unrelated HTTP/MCP traffic.
async fn host_transfer_blocking(
    from: PathBuf,
    to: PathBuf,
    overwrite: bool,
    remove_source: bool,
) -> Result<(u64, u64), String> {
    tokio::task::spawn_blocking(move || host_transfer(&from, &to, overwrite, remove_source))
        .await
        .map_err(|err| format!("传输任务异常:{err}"))?
}

/// Count files and bytes under a path (symlinks not followed, same as copies).
fn measure(path: &Path) -> (u64, u64) {
    if path.is_file() {
        return (1, path.metadata().map(|meta| meta.len()).unwrap_or(0));
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    for entry in WalkDir::new(path).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() {
            files += 1;
            bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    (files, bytes)
}

/// Host-side dir/file copy. Dirs recurse by content; files keep their name.
/// Also reports how many symlinks were skipped (zip cannot express them, see `zip::copy_tree`).
fn copy_tree(from: &Path, to: &Path) -> Result<(u64, u64, u64), String> {
    if from.is_dir() {
        // `to` is the destination dir itself, so spread the contents of from into to
        zip::copy_tree(from, to).map_err(|e| format!("复制失败:{e}"))
    } else {
        std::fs::copy(from, to)
            .map(|bytes| (1, bytes, 0))
            .map_err(|e| format!("复制失败:{e}"))
    }
}

// ------------------------------------------------------------------ Delete

async fn delete_inner(
    client: &Client,
    roots: &Roots,
    target: &Location,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    let rel = match paths::normalize_relative(&target.path) {
        Ok(value) => value,
        Err(err) => return OpOutcome::fail(format!("path 不合法:{err}")),
    };
    if rel.is_empty() {
        return OpOutcome::fail("拒绝删除项目根目录本身");
    }

    say(
        scopes,
        log::INFO,
        format!("删除: {}:{rel}", target.side.code()),
    );

    // Count first: once removed there is nothing left to count. The host side uses the
    // filesystem; the remote side counts in the same command.
    let counted = match count_one(client, roots, target.side, &rel, cancel).await {
        Ok(counts) => counts,
        // A failed count does not block the delete; record 0
        Err(_) => (0, 0),
    };

    match remove_one(client, roots, target.side, &rel, cancel).await {
        Ok(()) => OpOutcome::ok(
            format!(
                "已删除:{}({} 个文件 / {})",
                rel,
                counted.0,
                sync::human_bytes(counted.1)
            ),
            vec![format!("{}:{rel}", target.side.code())],
            Vec::new(),
        )
        .with_counts(counted.0, counted.1),
        Err(err) if err == CANCEL_MARK => OpOutcome::cancelled(),
        Err(err) => OpOutcome::fail(err),
    }
}

/// Count files/bytes at a confined location (pre-delete; remote bytes are approximate).
async fn count_one(
    client: &Client,
    roots: &Roots,
    side: Side,
    rel: &str,
    cancel: &Cancel,
) -> Result<(u64, u64), String> {
    if side.is_remote() {
        let root = shell_path(&roots.remote);
        let command = format!(
            "cd {root} && [ -e {} ] && {} || true",
            shell_path(rel),
            remote_count_suffix(rel).trim_start_matches("; ")
        );
        let exec = run_remote(client, &command, cancel).await?;
        Ok(parse_remote_count(&exec.combined()))
    } else {
        let abs = paths::confine_local(&roots.host, rel)?;
        // Walking the tree to measure it is blocking file IO.
        tokio::task::spawn_blocking(move || measure(&abs))
            .await
            .map_err(|err| format!("统计任务异常:{err}"))
    }
}

const CANCEL_MARK: &str = "\u{0}cancelled";

async fn remove_one(
    client: &Client,
    roots: &Roots,
    side: Side,
    rel: &str,
    cancel: &Cancel,
) -> Result<(), String> {
    if side.is_remote() {
        let root = shell_path(&roots.remote);
        let path = shell_path(rel);
        let command = format!(
            "cd {root} && [ -e {path} ] || {{ echo 'cgw: 目标不存在: {rel}' >&2; exit 66; }}; rm -rf {path}"
        );
        match run_remote(client, &command, cancel).await {
            Ok(exec) if exec.cancelled => Err(CANCEL_MARK.to_string()),
            Ok(exec) if exec.ok() => Ok(()),
            Ok(exec) => Err(format!(
                "远端删除失败({}):{}",
                exec.exit_note(),
                exec.combined()
            )),
            Err(err) => Err(err),
        }
    } else {
        let abs = paths::confine_local(&roots.host, rel)?;
        // `rm -rf` on the host is blocking; keep it off the tokio worker.
        tokio::task::spawn_blocking(move || remove_path(&abs))
            .await
            .map_err(|err| format!("删除任务异常:{err}"))?
    }
}

fn remove_path(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| format!("删除 {} 失败:{e}", path.display()))
    } else {
        std::fs::remove_file(path).map_err(|e| format!("删除 {} 失败:{e}", path.display()))
    }
}

// ------------------------------------------------------------------ Export

async fn export_inner(
    client: &Client,
    roots: &Roots,
    sources: &[Location],
    output_name: &str,
    cancel: &Cancel,
    scopes: &[String],
) -> OpOutcome {
    if sources.is_empty() {
        return OpOutcome::fail("export 至少需要一个 sources 条目");
    }
    let name = sanitize_output_name(output_name);
    if name.is_empty() {
        return OpOutcome::fail("outputName 不合法");
    }

    // 1) Stage all sources into one temp dir, pulling remote sources first.
    let stage = roots
        .host
        .join(format!(".cgw-export-{}", projects_rand()));
    if let Err(err) = std::fs::create_dir_all(&stage) {
        return OpOutcome::fail(format!("创建暂存目录 {} 失败:{err}", stage.display()));
    }

    let mut inputs: Vec<String> = Vec::new();
    // zip cannot express symlinks: report the skipped count instead of quietly losing files
    let mut skipped_links = 0u64;

    let result = async {
        for (index, source) in sources.iter().enumerate() {
            let rel = paths::normalize_relative(&source.path).map_err(OpOutcome::fail)?;
            if rel.is_empty() {
                return Err(OpOutcome::fail("sources 里不允许出现根目录本身"));
            }
            inputs.push(format!("{}:{rel}", source.side.code()));

            let base = basename(&rel);
            // Prefix same-named sources with an index so they do not overwrite each other
            let duplicated = sources.iter().take(index).any(|other| {
                paths::normalize_relative(&other.path)
                    .map(|r| basename(&r) == base)
                    .unwrap_or(false)
            });
            let slot = if duplicated {
                format!("{}-{base}", index + 1)
            } else {
                base.clone()
            };
            let dest = stage.join(&slot);

            say(
                scopes,
                log::INFO,
                format!("导出暂存:{}:{rel} -> {}", source.side.code(), dest.display()),
            );

            if source.side.is_remote() {
                let (archive, _) = pull_archive(client, &roots.remote, &rel, cancel).await?;
                // Unpacking the staged archive is blocking file IO too.
                let dest_for_unpack = dest.clone();
                let unpacked =
                    tokio::task::spawn_blocking(move || extract_archive_into(&archive, &dest_for_unpack))
                        .await
                        .map_err(|err| OpOutcome::fail(format!("解包任务异常:{err}")))?;
                unpacked.map_err(OpOutcome::fail)?;
            } else {
                let abs = paths::confine_local(&roots.host, &rel).map_err(OpOutcome::fail)?;
                let dest_for_copy = dest.clone();
                let copied = tokio::task::spawn_blocking(move || zip::copy_tree(&abs, &dest_for_copy))
                    .await
                    .map_err(|err| OpOutcome::fail(format!("暂存任务异常:{err}")))?;
                let (_, _, links) =
                    copied.map_err(|e| OpOutcome::fail(format!("暂存失败:{e}")))?;
                skipped_links += links;
            }
        }
        Ok::<(), OpOutcome>(())
    }
    .await;

    if let Err(outcome) = result {
        let _ = std::fs::remove_dir_all(&stage);
        return outcome;
    }

    // 2) Pack into a zip or tar.gz under `<host>/exports/`.
    let export_dir = roots.host.join("exports");
    let out = export_dir.join(&name);
    if out.exists() {
        let _ = std::fs::remove_file(&out);
    }
    let is_tar_gz = name.to_ascii_lowercase().ends_with(".tar.gz")
        || name.to_ascii_lowercase().ends_with(".tgz");
    // Packing the staged tree walks it and compresses everything: blocking IO that must not
    // run on a tokio worker (see the same reasoning in `sync::push_with`).
    let stage_for_pack = stage.clone();
    let out_for_pack = out.clone();
    let packed = tokio::task::spawn_blocking(move || {
        if is_tar_gz {
            tar_gz_directory(&stage_for_pack, &out_for_pack)
        } else {
            zip::zip_directory(&stage_for_pack, &out_for_pack)
        }
    })
    .await;
    let _ = std::fs::remove_dir_all(&stage);

    let packed = match packed {
        Ok(result) => result,
        Err(err) => return OpOutcome::fail(format!("打包任务异常:{err}")),
    };

    match packed {
        Ok((files, bytes, links)) => {
            let note = if is_tar_gz {
                if links > 0 {
                    format!("(已保留 {links} 个符号链接)")
                } else {
                    String::new()
                }
            } else {
                let skipped = skipped_links + links;
                if skipped > 0 {
                    format!("(跳过了 {skipped} 个符号链接: zip 无法跨平台表达；如需保留符号链接请使用 .tar.gz)")
                } else {
                    String::new()
                }
            };
            say(
                scopes,
                log::INFO,
                format!(
                    "导出完成:{} 个文件 / {} -> {} {note}",
                    files,
                    sync::human_bytes(bytes),
                    out.display()
                ),
            );
            OpOutcome::ok(
                format!(
                    "已导出 {files} 个文件 / {} 到 {} {note}",
                    sync::human_bytes(bytes),
                    out.display()
                ),
                inputs,
                vec![out.display().to_string()],
            )
            .with_counts(files, bytes)
        }
        Err(err) => OpOutcome::fail(format!("打包失败:{err}")),
    }
}

/// Pack directory contents into a `.tar.gz` archive (symlinks preserved).
/// Returns `(files, bytes, links)`.
pub fn tar_gz_directory(root: &Path, out: &Path) -> std::io::Result<(u64, u64, u64)> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);

    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut links = 0u64;

    for entry in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel,
            _ => continue,
        };
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            builder.append_path_with_name(path, rel)?;
            links += 1;
        } else if file_type.is_dir() {
            builder.append_dir(rel, path)?;
        } else if file_type.is_file() {
            let meta = entry.metadata().ok();
            bytes += meta.map(|m| m.len()).unwrap_or(0);
            files += 1;
            builder.append_path_with_name(path, rel)?;
        }
    }
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok((files, bytes, links))
}

fn basename(rel: &str) -> String {
    rel.rsplit_once('/')
        .map(|(_, name)| name.to_string())
        .unwrap_or_else(|| rel.to_string())
}

/// Parent dir of a relative path; no slash means `.`, ready for `cd` / `tar -C`.
fn relative_parent(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// Keep only the file name, dropping `../`, drive letters and subdirs.
fn sanitize_output_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "export.zip".to_string();
    }
    let unified = trimmed.replace('\\', "/");
    let last = unified.rsplit('/').next().unwrap_or("").trim().to_string();
    let cleaned: String = last
        .chars()
        .filter(|c| !matches!(c, '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        return "export.zip".to_string();
    }
    let lower = cleaned.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        cleaned
    } else if lower.ends_with(".zip") {
        cleaned
    } else {
        format!("{cleaned}.zip")
    }
}

// ------------------------------------------------------------------ Cross-side transfer

/// Cross-side copy: source -> tar.gz -> unpack on the other side.
async fn cross_side(
    client: &Client,
    roots: &Roots,
    from_side: Side,
    from_rel: &str,
    to_side: Side,
    to_rel: &str,
    overwrite: bool,
    cancel: &Cancel,
) -> Result<(u64, u64), OpOutcome> {
    if from_side == to_side {
        return Err(OpOutcome::fail("cross_side 只处理跨侧传输"));
    }
    if from_side.is_remote() {
        // remote -> host
        let (archive, _) = pull_archive(client, &roots.remote, from_rel, cancel).await?;
        let dest = match paths::confine_local(&roots.host, to_rel) {
            Ok(path) => path,
            Err(err) => return Err(OpOutcome::fail(err)),
        };
        if dest.exists() {
            if !overwrite {
                return Err(OpOutcome::fail(format!(
                    "目标已存在(未勾选覆盖):{to_rel}"
                )));
            }
            let dest_for_remove = dest.clone();
            tokio::task::spawn_blocking(move || remove_path(&dest_for_remove))
                .await
                .map_err(|err| OpOutcome::fail(format!("清理任务异常:{err}")))?
                .map_err(OpOutcome::fail)?;
        }
        // Unpacking is blocking file IO.
        let dest_for_unpack = dest.clone();
        tokio::task::spawn_blocking(move || extract_archive_to(&archive, &dest_for_unpack))
            .await
            .map_err(|err| OpOutcome::fail(format!("解包任务异常:{err}")))?
            .map_err(OpOutcome::fail)
    } else {
        // host -> remote
        let source = match paths::confine_local(&roots.host, from_rel) {
            Ok(path) => path,
            Err(err) => return Err(OpOutcome::fail(err)),
        };
        if !source.exists() {
            return Err(OpOutcome::fail(format!("源不存在:{from_rel}")));
        }
        // Same semantics as the same-side case: `to` is the full destination path (it may
        // rename), not "unpack into this dir". So unpack into a remote temp dir, then `mv`
        // the single top-level entry to `to`.
        let base = basename(from_rel);
        let source_for_pack = source.clone();
        let base_for_pack = base.clone();
        let (archive, files, bytes) = tokio::task::spawn_blocking(move || {
            pack_path(&source_for_pack, &base_for_pack)
        })
        .await
        .map_err(|err| OpOutcome::fail(format!("打包任务异常:{err}")))?
        .map_err(OpOutcome::fail)?;
        let command = upload_command(&roots.remote, from_rel, to_rel, overwrite);

        let exec = {
            let mut session = Session::open_cancellable(client, cancel)
                .await
                .map_err(|err| OpOutcome::fail(err))?;
            session
                .exec_stdin_cancellable(&command, &archive, cancel)
                .await
                .map_err(|err| OpOutcome::fail(err))?
        };
        if exec.cancelled {
            return Err(OpOutcome::cancelled());
        }
        if !exec.ok() {
            return Err(OpOutcome::fail(format!(
                "远端解包失败({}):{}",
                exec.exit_note(),
                exec.combined()
            )));
        }
        Ok((files, bytes))
    }
}

/// Fetch an archive of one remote path.
///
/// The entry name uses only the basename (`tar -C <parent> -- <name>`), with no directory
/// chain, because `extract_archive_to` requires exactly one top-level entry in order to
/// rename it onto the full `to` path.
///
/// The command is a pure function so tests can pin this without opening SSH.
fn pull_archive_command(remote_root: &str, rel: &str) -> String {
    let root = shell_path(remote_root);
    let path = shell_path(rel);
    let dir = shell_path(relative_parent(rel));
    let base = shell_path(&basename(rel));
    format!(
        "cd {root} && [ -e {path} ] || {{ echo 'cgw: 源不存在: {rel}' >&2; exit 66; }}; \
         cd {dir} && tar -czf - -- {base}"
    )
}

async fn pull_archive(
    client: &Client,
    remote_root: &str,
    rel: &str,
    cancel: &Cancel,
) -> Result<(Vec<u8>, Option<String>), OpOutcome> {
    let command = pull_archive_command(remote_root, rel);

    let mut session = Session::open_cancellable(client, cancel)
        .await
        .map_err(|err| OpOutcome::fail(err))?;
    let host_key = session.host_key().map(str::to_string);
    let exec = session
        .exec_collect_cancellable(&command, cancel)
        .await
        .map_err(|err| OpOutcome::fail(err))?;

    if exec.cancelled {
        return Err(OpOutcome::cancelled());
    }
    if !exec.ok() {
        return Err(OpOutcome::fail(format!(
            "远端打包失败({}):{}",
            exec.exit_note(),
            exec.combined()
        )));
    }
    if exec.stdout.len() > MAX_ARCHIVE_BYTES {
        return Err(OpOutcome::fail(format!(
            "归档超过 {} 上限",
            sync::human_bytes(MAX_ARCHIVE_BYTES as u64)
        )));
    }
    Ok((exec.stdout, host_key))
}

/// Pack one local path into a tar.gz with a chosen entry name.
fn pack_path(source: &Path, entry_name: &str) -> Result<(Vec<u8>, u64, u64), String> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut files = 0u64;
    let mut bytes = 0u64;

    if source.is_file() {
        bytes += source.metadata().map(|m| m.len()).unwrap_or(0);
        files += 1;
        builder
            .append_path_with_name(source, entry_name)
            .map_err(|e| format!("打包 {entry_name} 失败:{e}"))?;
    } else {
        builder
            .append_dir(entry_name, source)
            .map_err(|e| format!("打包 {entry_name} 失败:{e}"))?;
        for entry in WalkDir::new(source).follow_links(false).sort_by_file_name() {
            let entry = entry.map_err(|e| format!("遍历 {entry_name} 失败:{e}"))?;
            let path = entry.path();
            let rel = match path.strip_prefix(source) {
                Ok(rel) if !rel.as_os_str().is_empty() => rel,
                _ => continue,
            };
            let file_type = entry.file_type();
            if file_type.is_symlink() {
                continue;
            }
            let name = format!(
                "{entry_name}/{}",
                rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            );
            if file_type.is_dir() {
                builder
                    .append_dir(&name, path)
                    .map_err(|e| format!("打包 {name} 失败:{e}"))?;
            } else if file_type.is_file() {
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                files += 1;
                builder
                    .append_path_with_name(path, &name)
                    .map_err(|e| format!("打包 {name} 失败:{e}"))?;
            }
        }
    }

    let encoder = builder.into_inner().map_err(|e| format!("收尾失败:{e}"))?;
    let data = encoder.finish().map_err(|e| format!("压缩失败:{e}"))?;
    if data.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "打包后超过 {} 上限",
            sync::human_bytes(MAX_ARCHIVE_BYTES as u64)
        ));
    }
    Ok((data, files, bytes))
}

/// Unpack into the parent dir of `dest`, then rename the single top-level entry to `dest`.
fn extract_archive_to(archive: &[u8], dest: &Path) -> Result<(u64, u64), String> {
    let parent = dest
        .parent()
        .ok_or_else(|| format!("{} 没有父目录", dest.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建 {} 失败:{e}", parent.display()))?;

    let tmp = parent.join(format!(".cgw-tmp-{}", projects_rand()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("创建暂存目录失败:{e}"))?;

    let result = (|| -> Result<(u64, u64), String> {
        let (files, bytes) = unpack_into(archive, &tmp)?;
        // Exactly one top-level entry: move it to the final name
        let entries: Vec<PathBuf> = std::fs::read_dir(&tmp)
            .map_err(|e| format!("读取暂存目录失败:{e}"))?
            .flatten()
            .map(|entry| entry.path())
            .collect();
        if entries.len() != 1 {
            return Err(format!(
                "归档里的顶层条目不是恰好一个(实际 {})",
                entries.len()
            ));
        }
        std::fs::rename(&entries[0], dest).map_err(|e| format!("就位失败:{e}"))?;
        Ok((files, bytes))
    })();

    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Unpack the archive straight into `dest` (used for export staging).
fn extract_archive_into(archive: &[u8], dest: &Path) -> Result<(u64, u64), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("创建 {} 失败:{e}", dest.display()))?;
    unpack_into(archive, dest)
}

fn unpack_into(archive: &[u8], dest: &Path) -> Result<(u64, u64), String> {
    let mut reader = tar::Archive::new(GzDecoder::new(archive));
    let mut files = 0u64;
    let mut bytes = 0u64;

    let entries = reader
        .entries()
        .map_err(|e| format!("读取归档失败:{e}"))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| format!("读取归档条目失败:{e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("归档条目路径非法:{e}"))?
            .into_owned();
        // Belt and braces: skip absolute paths and `..` in archives
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败:{e}"))?;
        }
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("创建目录失败:{e}"))?;
            continue;
        }
        let mut out = std::fs::File::create(&target).map_err(|e| format!("写入失败:{e}"))?;
        let copied = std::io::copy(&mut entry, &mut out).map_err(|e| format!("写入失败:{e}"))?;
        files += 1;
        bytes += copied;
    }

    Ok((files, bytes))
}

// ------------------------------------------------------------------ SSH

async fn run_remote(
    client: &Client,
    command: &str,
    cancel: &Cancel,
) -> Result<crate::ssh::Exec, String> {
    let mut session = Session::open_cancellable(client, cancel).await?;
    session.exec_collect_cancellable(command, cancel).await
}

fn projects_rand() -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_name_is_confined_to_a_filename() {
        assert_eq!(sanitize_output_name("a.zip"), "a.zip");
        assert_eq!(sanitize_output_name("a"), "a.zip");
        assert_eq!(sanitize_output_name("a.tar.gz"), "a.tar.gz");
        assert_eq!(sanitize_output_name("a.tgz"), "a.tgz");
        assert_eq!(sanitize_output_name("../../evil.zip"), "evil.zip");
        assert_eq!(sanitize_output_name("../../evil.tar.gz"), "evil.tar.gz");
        assert_eq!(sanitize_output_name("..\\..\\evil"), "evil.zip");
        assert_eq!(sanitize_output_name("C:/Windows/x.zip"), "x.zip");
        assert_eq!(sanitize_output_name("x?.zip"), "x.zip");
        // Fall back to the default when only dots are named
        assert_eq!(sanitize_output_name(".."), "export.zip");
        assert_eq!(sanitize_output_name(""), "export.zip");
    }

    #[test]
    fn remote_command_refuses_existing_target_without_overwrite() {
        let roots = Roots {
            remote: "~/Work/proj".to_string(),
            host: PathBuf::from("/tmp"),
        };
        let command =
            remote_transfer_command(&roots, "a/b.app", "c/d.app", false, false);
        assert!(command.starts_with("cd ~/Work/proj && "), "{command}");
        assert!(command.contains("[ ! -e c/d.app ]"), "{command}");
        assert!(!command.contains("rm -rf c/d.app"), "{command}");
        assert!(command.contains("cp -a -- a/b.app c/d.app"), "{command}");

        let command =
            remote_transfer_command(&roots, "a/b.app", "c/d.app", true, true);
        assert!(command.contains("rm -rf c/d.app"), "{command}");
        assert!(command.contains("mv -- a/b.app c/d.app"), "{command}");
    }

    /// Cross-side copy target semantics: `to` is the full path (may rename) in both directions.
    ///
    /// Regression test for the two 2026-09-28 §4 issues: remote -> host used to archive the
    /// whole relative path (top-level entry became the first dir), and host -> remote used to
    /// ignore the `to` file name.
    #[test]
    fn cross_side_copy_targets_the_full_destination_path() {
        // remote -> host: archive only the basename, after cd to its parent
        let pull = pull_archive_command("~/Work/proj", ".cgw/ipa/App.ipa");
        assert!(
            pull.ends_with("cd .cgw/ipa && tar -czf - -- App.ipa"),
            "{pull}"
        );
        assert!(!pull.contains("-- .cgw/ipa/App.ipa"), "{pull}");
        // Without a slash the parent is `.`
        let flat = pull_archive_command("~/Work/proj", "App.ipa");
        assert!(flat.ends_with("cd . && tar -czf - -- App.ipa"), "{flat}");

        // host -> remote: must really land on the `to` name, not the source basename
        let up = upload_command("~/Work/proj", "a/b.txt", "c/d.txt", false);
        assert!(up.contains("mv -- .cgw-copy-tmp/b.txt c/d.txt"), "{up}");
        assert!(up.contains("tar -xzf - -C .cgw-copy-tmp"), "{up}");
        assert!(up.contains("rm -rf .cgw-copy-tmp"), "{up}");
        assert!(up.contains("[ ! -e c/d.txt ]"), "{up}");
        // The temp dir is cleared then recreated, so no stale entries remain
        assert!(up.contains("rm -rf .cgw-copy-tmp && mkdir -p .cgw-copy-tmp"), "{up}");

        let over = upload_command("~/Work/proj", "a/b.txt", "d.txt", true);
        assert!(!over.contains("[ ! -e d.txt ]"), "{over}");
        assert!(over.contains("rm -rf d.txt"), "{over}");
        assert!(over.contains("mv -- .cgw-copy-tmp/b.txt d.txt"), "{over}");
    }

    #[test]
    fn relative_parent_walks_one_level() {
        assert_eq!(relative_parent("a/b/c.txt"), "a/b");
        assert_eq!(relative_parent("c.txt"), ".");
    }

    #[test]
    fn remote_command_quotes_hostile_paths() {
        let roots = Roots {
            remote: "~/Work".to_string(),
            host: PathBuf::from("/tmp"),
        };
        // Paths must already be normalized; this only checks the built command is quoted
        let command = remote_transfer_command(&roots, "a b/c;d", "x", true, false);
        assert!(command.contains("'a b/c;d'"), "{command}");
    }

    #[test]
    fn pack_and_unpack_round_trip() {
        let root = std::env::temp_dir().join("cgw-fileops-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/a.txt"), b"hello").unwrap();
        std::fs::write(root.join("src/sub/b.txt"), b"world!").unwrap();

        let (archive, files, bytes) = pack_path(&root.join("src"), "src").unwrap();
        assert_eq!(files, 2);
        assert_eq!(bytes, 11);

        let dest = root.join("out/payload");
        let (dfiles, dbytes) = extract_archive_to(&archive, &dest).unwrap();
        assert_eq!((dfiles, dbytes), (2, 11));
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dest.join("sub/b.txt")).unwrap(), b"world!");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn single_file_pack_keeps_its_name() {
        let root = std::env::temp_dir().join("cgw-fileops-file-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("App.ipa"), b"payload").unwrap();

        let (archive, files, _) = pack_path(&root.join("App.ipa"), "App.ipa").unwrap();
        assert_eq!(files, 1);

        let dest = root.join("moved/App.ipa");
        extract_archive_to(&archive, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"payload");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn host_transfer_respects_overwrite() {
        let root = std::env::temp_dir().join("cgw-fileops-host-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/f.txt"), b"1").unwrap();

        // Target missing: success
        let (_, _) = host_transfer(&root.join("a"), &root.join("b"), false, false).unwrap();
        assert_eq!(std::fs::read(root.join("b/f.txt")).unwrap(), b"1");

        // Target exists without overwrite: failure
        assert!(host_transfer(&root.join("a"), &root.join("b"), false, false).is_err());

        // Overwrite selected: success
        host_transfer(&root.join("a"), &root.join("b"), true, false).unwrap();

        // Move: the source is gone and the counts are real (the rename path used to report 0)
        let (files, bytes) =
            host_transfer(&root.join("b"), &root.join("c"), true, true).unwrap();
        assert!(!root.join("b").exists());
        assert!(root.join("c/f.txt").exists());
        assert_eq!((files, bytes), (1, 1), "a move must still report real file/byte counts");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unpack_skips_escaping_entries() {
        let root = std::env::temp_dir().join("cgw-fileops-escape-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // Hand-build an archive with ../. The tar crate rejects `..` in set_path,
        // so write the GNU header name field directly.
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        {
            let raw = b"../escaped.txt";
            let gnu = header.as_gnu_mut().unwrap();
            gnu.name[..raw.len()].copy_from_slice(raw);
        }
        header.set_cksum();
        builder.append(&header, &b"bad"[..]).unwrap();
        let data = builder.into_inner().unwrap().finish().unwrap();

        let dest = root.join("safe");
        let (files, _) = extract_archive_into(&data, &dest).unwrap();
        assert_eq!(files, 0, "escaping entries should be skipped");
        assert!(!root.join("escaped.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tar_gz_directory_packs_files() {
        let root = std::env::temp_dir().join("cgw-targz-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"bb").unwrap();

        let out = std::env::temp_dir().join("cgw-targz-test-out.tar.gz");
        let (files, bytes, links) = tar_gz_directory(&root, &out).unwrap();
        assert_eq!(files, 2);
        assert_eq!(bytes, 3);
        assert_eq!(links, 0);
        assert!(out.is_file());
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_dir_all(&root);
    }
}


