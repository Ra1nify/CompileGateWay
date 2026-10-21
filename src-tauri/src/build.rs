//! Run one build: render the manifest into a command, run it in the remote
//! project directory, and stream output **line by line** into that client's log
//! scope.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use crate::clients::Client;
use crate::log;
use crate::manifest::{self, Manifest, SigningMode};
use crate::ssh::{Cancel, Session};

/// How many trailing output lines to attach on failure, to show where it broke.
const TAIL_LINES: usize = 25;

pub struct BuildOutcome {
    pub ok: bool,
    /// Cancelled by the user (the remote process may keep running, sshd permitting).
    pub cancelled: bool,
    pub message: String,
    pub exit_code: i32,
    pub command: String,
    /// Artifacts this build located (design doc §7.1: there can be several).
    pub artifacts: Vec<String>,
}

impl BuildOutcome {
    /// First artifact -- for the single-value field in a job record.
    pub fn primary(&self) -> &str {
        self.artifacts.first().map(String::as_str).unwrap_or("")
    }
}

/// Render the command without running it (for "preview command").
/// Returns `(command, artifacts)`.
///
/// `env` holds build-specific environment variables -- currently only
/// `DEVELOPER_DIR` from design doc §8.
pub fn preview(
    manifest: &Manifest,
    variables: &BTreeMap<String, String>,
    signing: SigningMode,
    workdir: &str,
    env: &[(String, String)],
) -> Result<(String, Vec<String>), Vec<String>> {
    let command = manifest::render_command(manifest, variables, signing, workdir, env)?;
    let artifacts = manifest::render_artifacts(manifest, variables);
    Ok((command, artifacts))
}

/// Merge job-level overrides with manifest defaults into the final artifact list
/// (design doc §7.1).
pub fn resolve_artifacts(manifest: &Manifest, variables: &BTreeMap<String, String>, overrides: &[String]) -> Vec<String> {
    let templates = manifest::artifact_templates(overrides, "");
    if !templates.is_empty() {
        return templates
            .into_iter()
            .filter_map(|template| {
                let rendered = manifest::render_template(manifest, variables, &template);
                let trimmed = rendered.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            })
            .collect();
    }
    manifest::render_artifacts(manifest, variables)
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &Client,
    workdir: &str,
    manifest: &Manifest,
    variables: &BTreeMap<String, String>,
    signing: SigningMode,
    artifact_override: &[String],
    env: &[(String, String)],
    timeout: Duration,
    scopes: &[String],
    cancel: &Cancel,
) -> BuildOutcome {
    let (command, default_artifacts) = match preview(manifest, variables, signing, workdir, env) {
        Ok(value) => value,
        Err(missing) => {
            return BuildOutcome {
                ok: false,
                cancelled: false,
                message: format!("缺少必填变量:{}", missing.join(", ")),
                exit_code: -1,
                command: String::new(),
                artifacts: Vec::new(),
            };
        }
    };

    let artifacts = resolve_artifacts(manifest, variables, artifact_override);
    let artifacts = if artifacts.is_empty() {
        default_artifacts
    } else {
        artifacts
    };

    log::log_multi(scopes, log::INFO, format!("$ {command}"));

    match tokio::time::timeout(timeout, run_inner(client, &command, scopes, cancel)).await {
        Ok(Ok((0, false))) => {
            let note = check_artifacts(client, workdir, &artifacts).await;
            BuildOutcome {
                ok: true,
                cancelled: false,
                message: if note.is_empty() {
                    "构建完成".to_string()
                } else {
                    format!("构建完成 {note}")
                },
                exit_code: 0,
                command,
                artifacts,
            }
        }
        Ok(Ok((exit, cancelled))) => BuildOutcome {
            ok: false,
            cancelled,
            message: if cancelled {
                "构建已被取消".to_string()
            } else if exit < 0 {
                // "No exit status (channel closed early)" and "non-zero exit" are
                // different things; keep them distinct
                "构建失败:通道提前关闭,没有拿到退出状态(见上方日志)".to_string()
            } else {
                format!("构建失败,退出码 {exit}")
            },
            exit_code: if cancelled { -1 } else { exit },
            command,
            artifacts,
        },
        Ok(Err((message, cancelled))) => BuildOutcome {
            ok: false,
            cancelled,
            message,
            exit_code: -1,
            command,
            artifacts,
        },
        Err(_) => BuildOutcome {
            ok: false,
            cancelled: false,
            message: format!("构建超时({}s)", timeout.as_secs()),
            exit_code: -1,
            command,
            artifacts,
        },
    }
}

/// Returns the remote exit code (**-1 = no exit status**); on failure it also
/// logs the tail of the output. The second value says whether cancellation ended
/// the run.
async fn run_inner(
    client: &Client,
    command: &str,
    scopes: &[String],
    cancel: &Cancel,
) -> Result<(i32, bool), (String, bool)> {
    let mut session = match Session::open_cancellable(client, cancel).await {
        Ok(session) => session,
        Err(message) => return Err((message, false)),
    };
    let mut tail: VecDeque<String> = VecDeque::new();

    let exec = match session
        .exec_stream_cancellable(command, cancel, |line| {
            log::log_multi(scopes, log::OUT, line);
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(line.to_string());
        })
        .await
    {
        Ok(exec) => exec,
        Err(message) => return Err((message, cancel.requested())),
    };

    if exec.cancelled {
        log::log_multi(scopes, log::WARN, "构建已被用户取消".to_string());
        return Ok((exec.code(), true));
    }

    if !exec.ok() {
        let last = tail
            .iter()
            .rev()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        log::log_multi(
            scopes,
            log::ERROR,
            format!("远端{};末尾输出:\n{last}", exec.exit_note()),
        );
    }

    Ok((exec.code(), false))
}

/// Check each artifact after the build and report its size (design doc §7.1 allows
/// several artifacts).
async fn check_artifacts(client: &Client, workdir: &str, artifacts: &[String]) -> String {
    if artifacts.is_empty() {
        return String::new();
    }

    let checks: Vec<String> = artifacts
        .iter()
        .map(|artifact| {
            let path = if artifact.starts_with('/') || artifact.starts_with('~') {
                artifact.clone()
            } else {
                format!("{}/{}", workdir.trim_end_matches('/'), artifact)
            };
            let quoted = crate::ssh::shell_path(&path);
            format!(
                "if [ -e {quoted} ]; then printf '%s ' \"$(du -sh {quoted} 2>/dev/null || ls -ld {quoted})\"; else printf '__CGW_MISSING__ %s\\n' {label}; fi",
                label = crate::ssh::shell_path(artifact)
            )
        })
        .collect();

    let mut session = match Session::open(client).await {
        Ok(session) => session,
        Err(_) => return String::new(),
    };
    let exec = match session.exec(&checks.join("; ")).await {
        Ok(exec) => exec,
        Err(_) => return String::new(),
    };

    let text = exec.combined();
    let missing: Vec<&str> = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("__CGW_MISSING__ "))
        .collect();
    let found: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("__CGW_MISSING__") && !line.trim().is_empty())
        .collect();

    match (found.is_empty(), missing.is_empty()) {
        (true, true) => String::new(),
        (_, true) => format!("产物 {}", found.join(" / ")),
        (true, false) => format!("(没找到产物 {})", missing.join(", ")),
        (false, false) => format!("产物 {}(缺少 {})", found.join(" / "), missing.join(", ")),
    }
}
