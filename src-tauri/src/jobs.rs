//! Async job registry (design doc §10.1 / §7.3).
//!
//! Executing endpoints (push / pull / build) no longer block until they finish; they
//! return a jobId immediately, so a successful call only means the job was accepted.
//! Status, logs and cancellation all go through `/api/jobs/*`.
//!
//! Also enforces the project-level mutex of §7.5: one running job per project.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many past jobs are kept.
pub const MAX_JOBS: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobState {
    /// Queued, not started yet (design doc §7.6.6); a project's queue head is promoted to Running.
    Pending,
    Running,
    Ok,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn code(self) -> &'static str {
        match self {
            JobState::Pending => "pending",
            JobState::Running => "running",
            JobState::Ok => "ok",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, JobState::Pending | JobState::Running)
    }
}

/// A job's own log scope (design doc §10.1, GET /api/jobs/{id}/logs).
pub fn log_scope(job_id: &str) -> String {
    format!("job-{job_id}")
}

/// Execution record of one step in a batch workflow (design doc §7.3).
///
/// `index` is the step number within the batch, not an offset inside the job, so a
/// mid-batch retry keeps the same numbering the CLI and UI already saw.
#[derive(Clone)]
pub struct JobStep {
    pub index: usize,
    pub task_id: String,
    pub name: String,
    pub kind: String,
    pub state: &'static str,
    pub started_at: u64,
    pub finished_at: u64,
    pub message: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub exit_code: i32,
    /// Structured failure reason (design doc §10.1): `artifactMissing` / `inputInvalid` /
    /// `taskInvalid` ... An empty string means no specific reason.
    pub code: String,
}

impl JobStep {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "index": self.index,
            "taskId": self.task_id,
            "name": self.name,
            "kind": self.kind,
            "state": self.state,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
            "elapsedSeconds": if self.finished_at > self.started_at {
                (self.finished_at - self.started_at) / 1000
            } else if self.state == "running" {
                now_ms().saturating_sub(self.started_at) / 1000
            } else {
                0
            },
            "message": self.message,
            "inputs": self.inputs,
            "outputs": self.outputs,
            "exitCode": self.exit_code,
            "code": self.code,
        })
    }
}

pub struct Job {
    pub id: String,
    pub project_id: String,
    pub client_id: String,
    pub kind: &'static str,
    /// The batch this job executes (used on retry to find the step definitions).
    pub batch_id: Option<String>,
    /// Who started it (design doc §10 `X-Client`): `webui` / `cli` / `mcp` / `daemon`.
    /// Log lines written during the build carry it so the UI can filter by source.
    pub source: String,
    pub title: String,
    pub state: JobState,
    pub started_at: u64,
    pub finished_at: u64,
    pub message: String,
    pub exit_code: i32,
    pub artifacts: Vec<String>,
    pub files: u64,
    pub bytes: u64,
    pub steps: Vec<JobStep>,
    /// **Resolved input snapshot** (design doc §7.6.7): which input values this run used.
    ///
    /// Without it, editing task variables and then retrying silently runs the new values,
    /// and nothing can explain why the retry does not match the previous run.
    pub inputs: BTreeMap<String, String>,
    /// Outputs each step **verified**: `"<step id>.outputs.<name>" -> real path` (design doc §7.6.5).
    ///
    /// Persisting them is required because a mid-batch retry does not re-run earlier steps,
    /// so later steps can only resolve their outputs from this record.
    pub produced: BTreeMap<String, String>,
    /// Cooperative cancellation: the executor reads it wherever it can check.
    pub cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn primary_artifact(&self) -> &str {
        self.artifacts.first().map(String::as_str).unwrap_or("")
    }

    /// Batch index of the first unsuccessful step (the retry point); None when all succeeded.
    pub fn first_unfinished_step(&self) -> Option<usize> {
        if let Some(step) = self.steps.iter().find(|step| step.state != "ok") {
            return Some(step.index);
        }
        if self.steps.is_empty() && self.state != JobState::Ok {
            // No step records yet (running but not started): start from the beginning
            return Some(0);
        }
        None
    }

    pub fn to_json(&self) -> serde_json::Value {
        let elapsed = if self.state == JobState::Pending {
            // Pending has not started, so do not compute an astronomical value from `started_at = 0`
            0
        } else if self.finished_at > self.started_at {
            (self.finished_at - self.started_at) / 1000
        } else {
            now_ms().saturating_sub(self.started_at) / 1000
        };
        serde_json::json!({
            "id": self.id,
            "projectId": self.project_id,
            "clientId": self.client_id,
            "kind": self.kind,
            "batchId": self.batch_id,
            "source": self.source,
            "title": self.title,
            "state": self.state.code(),
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
            "elapsedSeconds": elapsed,
            "message": self.message,
            "exitCode": self.exit_code,
            "artifactPath": self.primary_artifact(),
            "artifacts": self.artifacts,
            "files": self.files,
            "bytes": self.bytes,
            "steps": self.steps.iter().map(JobStep::to_json).collect::<Vec<_>>(),
            "inputs": self.inputs,
            "stepOutputs": self.produced,
            "retryFromStep": self.first_unfinished_step(),
            "cancelRequested": self.cancel.load(Ordering::Relaxed),
        })
    }
}

/// Information returned when a start is blocked by the project-level mutex.
#[derive(Debug)]
pub struct Busy {
    pub job_id: String,
    pub kind: &'static str,
    pub running_seconds: u64,
    /// `true` = the blocker is **another project on the same client** (host lock, design doc
    /// §7.6.6 last item); `false` = this project itself.
    pub host: bool,
    /// Which project the blocking job belongs to (used to explain host-level conflicts).
    pub holder_project_id: String,
}

/// Extra information for starting a job.
///
/// Bundled because the three fields always travel together: `batch_id` is §7.3 retry
/// ownership, `source` is the §10 log source, `host_lock` is the §7.6.6 host-mutex switch.
#[derive(Debug, Clone, Default)]
pub struct StartOptions<'a> {
    /// Which batch this job belongs to (for retries).
    pub batch_id: Option<String>,
    /// Source (`webui` / `cli` / `mcp` / `daemon`, design doc §10).
    pub source: &'a str,
    /// Host lock (design doc §7.6.6): also block while another project runs on this client.
    pub host_lock: bool,
}

/// Handed to the executor: job id plus cancel flag.
pub struct JobHandle {
    pub id: String,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Registry {
    jobs: Mutex<VecDeque<Job>>,
    /// Persistence path. Unset = memory only (which is what unit tests use).
    ///
    /// `OnceLock` rather than `Mutex<Option<..>>`: the path is written once at startup and is
    /// then read on **every** `persist`, so a lock here would only add contention on the hot path.
    store: std::sync::OnceLock<std::path::PathBuf>,
}

/// On-disk job snapshot.
///
/// `Job` holds `Arc<AtomicBool>` (cancel flag) and `&'static str` (kind), which cannot be
/// serialized directly, so this separate shape stores and restores it. A restored cancel
/// flag is naturally "never requested".
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StoredJob {
    id: String,
    project_id: String,
    client_id: String,
    kind: String,
    batch_id: Option<String>,
    source: String,
    title: String,
    state: String,
    started_at: u64,
    finished_at: u64,
    message: String,
    exit_code: i32,
    artifacts: Vec<String>,
    files: u64,
    bytes: u64,
    steps: Vec<StoredStep>,
    /// Input snapshot (§7.6.7): retry replays it to reproduce that version of the inputs.
    inputs: BTreeMap<String, String>,
    /// Verified step outputs (§7.6.5): retry resolves `{{steps.x.outputs.y}}` from them.
    produced: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct StoredStep {
    index: usize,
    task_id: String,
    name: String,
    kind: String,
    state: String,
    started_at: u64,
    finished_at: u64,
    message: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    exit_code: i32,
    code: String,
}

/// Known job kinds. `Job.kind` is `&'static str`, so values read from disk map back to statics.
const KNOWN_KINDS: [&str; 8] = [
    "build",
    "push",
    "pull",
    "copy",
    "move",
    "delete",
    "export",
    "batch",
];

fn kind_static(kind: &str) -> &'static str {
    KNOWN_KINDS
        .iter()
        .copied()
        .find(|known| *known == kind)
        .unwrap_or("task")
}

fn kind_to_static(kind: &str) -> &'static str {
    kind_static(kind)
}

fn state_from_code(code: &str) -> JobState {
    match code {
        "pending" => JobState::Pending,
        "ok" => JobState::Ok,
        "failed" => JobState::Failed,
        "cancelled" => JobState::Cancelled,
        // A running job cannot survive a process restart; the caller turns it into failed
        _ => JobState::Running,
    }
}

/// Step state is `&'static str`, so values read from disk also converge to known ones.
fn step_state_static(state: &str) -> &'static str {
    match state {
        "pending" | "running" | "ok" | "failed" | "cancelled" | "skipped" => {
            // These literals are static strings, so match and return them directly
            match state {
                "pending" => "pending",
                "running" => "running",
                "ok" => "ok",
                "failed" => "failed",
                "cancelled" => "cancelled",
                _ => "skipped",
            }
        }
        _ => "failed",
    }
}

impl StoredJob {
    fn from_job(job: &Job) -> StoredJob {
        StoredJob {
            id: job.id.clone(),
            project_id: job.project_id.clone(),
            client_id: job.client_id.clone(),
            kind: job.kind.to_string(),
            batch_id: job.batch_id.clone(),
            source: job.source.clone(),
            title: job.title.clone(),
            state: job.state.code().to_string(),
            started_at: job.started_at,
            finished_at: job.finished_at,
            message: job.message.clone(),
            exit_code: job.exit_code,
            artifacts: job.artifacts.clone(),
            files: job.files,
            bytes: job.bytes,
            steps: job
                .steps
                .iter()
                .map(|step| StoredStep {
                    index: step.index,
                    task_id: step.task_id.clone(),
                    name: step.name.clone(),
                    kind: step.kind.clone(),
                    state: step.state.to_string(),
                    started_at: step.started_at,
                    finished_at: step.finished_at,
                    message: step.message.clone(),
                    inputs: step.inputs.clone(),
                    outputs: step.outputs.clone(),
                    exit_code: step.exit_code,
                    code: step.code.clone(),
                })
                .collect(),
            inputs: job.inputs.clone(),
            produced: job.produced.clone(),
        }
    }

    /// Rebuild the in-memory [`Job`].
    ///
    /// A job still running in the old process is always recorded as failed — its execution
    /// thread died with the process, and pretending otherwise means waiting forever.
    fn into_job_with_queue(self, active_queue_ids: &[String]) -> Job {
        let interrupted = self.state == "running";
        let mut state = state_from_code(&self.state);
        let mut message = self.message;
        let mut finished_at = self.finished_at;
        if interrupted {
            state = JobState::Failed;
            message = if message.is_empty() {
                "daemon 重启前这个任务还没结束,已标记为中断".to_string()
            } else {
                format!("{message}(daemon 重启,任务中断)")
            };
            finished_at = if finished_at == 0 { now_ms() } else { finished_at };
        }
        if self.state == "pending" {
            if active_queue_ids.iter().any(|id| id == &self.id) {
                state = JobState::Pending;
                message = if message.is_empty() {
                    "排队中".to_string()
                } else {
                    message
                };
                finished_at = 0;
            } else {
                state = JobState::Cancelled;
                message = "daemon 重启,排队中的任务不会自动继续;请重新提交".to_string();
                finished_at = now_ms();
            }
        }

        Job {
            id: self.id,
            project_id: self.project_id,
            client_id: self.client_id,
            kind: kind_to_static(&self.kind),
            batch_id: self.batch_id,
            source: if self.source.is_empty() {
                crate::log::SOURCE_DAEMON.to_string()
            } else {
                self.source
            },
            title: self.title,
            state,
            started_at: self.started_at,
            finished_at,
            message,
            exit_code: self.exit_code,
            artifacts: self.artifacts,
            files: self.files,
            bytes: self.bytes,
            steps: self
                .steps
                .into_iter()
                .map(|step| {
                    let step_interrupted = step.state == "running" || step.state == "pending";
                    JobStep {
                        index: step.index,
                        task_id: step.task_id,
                        name: step.name,
                        kind: step.kind,
                        state: if interrupted && step_interrupted {
                            if step.state == "running" {
                                "failed"
                            } else {
                                "skipped"
                            }
                        } else {
                            step_state_static(&step.state)
                        },
                        started_at: step.started_at,
                        finished_at: step.finished_at,
                        message: step.message,
                        inputs: step.inputs,
                        outputs: step.outputs,
                        exit_code: step.exit_code,
                        code: step.code,
                    }
                })
                .collect(),
            inputs: self.inputs,
            produced: self.produced,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

}

/// Persistence path for job history (next to `config.json`).
pub fn store_path(config_path: &std::path::Path) -> std::path::PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("jobs.json")
}

impl Registry {
    /// Attach the persistence path (called once at startup).
    pub fn attach_store(&self, path: std::path::PathBuf) {
        let _ = self.store.set(path);
    }

    /// The attached persistence path, if any.
    fn store_path(&self) -> Option<&std::path::Path> {
        self.store.get().map(std::path::PathBuf::as_path)
    }

    /// Restore history from disk (design doc §12.1: recent job state survives a daemon restart).
    pub fn restore(&self) {
        self.restore_with_active_queue(&[]);
    }

    /// Restore history from disk, preserving `pending` status for any jobs that have
    /// their queue plans restored from `queued_runs.json`.
    pub fn restore_with_active_queue(&self, active_queue_ids: &[String]) {
        let Some(path) = self.store_path() else {
            return;
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(stored) = serde_json::from_str::<Vec<StoredJob>>(&raw) else {
            // A broken file is treated as absent; history is not worth blocking startup
            return;
        };
        let mut jobs = crate::lock::lock(&self.jobs);
        jobs.clear();
        // The file stores newest first, so push back in reverse
        for entry in stored.into_iter().rev() {
            jobs.push_back(entry.into_job_with_queue(active_queue_ids));
        }
        while jobs.len() > MAX_JOBS {
            jobs.pop_front();
        }
    }

    /// Write the current history back to disk. Persistence failures are not reported — losing
    /// history must not disturb running jobs.
    ///
    /// **Call this while holding the `jobs` lock is not required, and holding it is harmful.**
    /// The serialization below can produce a multi-hundred-KB document, so the caller should
    /// drop the guard first: take [`Self::snapshot_locked`] inside the lock, then hand the
    /// result here. Every call site follows that pattern.
    fn persist_snapshot(&self, snapshot: &[StoredJob]) {
        let Some(path) = self.store_path() else {
            return;
        };
        let Ok(text) = serde_json::to_string_pretty(snapshot) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, text + "\n");
    }

    /// Snapshot the history for persistence. Call this **while holding the `jobs` lock**.
    fn snapshot_locked(jobs: &VecDeque<Job>) -> Vec<StoredJob> {
        jobs.iter().rev().map(StoredJob::from_job).collect()
    }
}

#[derive(Debug)]
pub enum CancelError {
    NotFound,
    Finished,
}

impl Registry {
    /// Take a project's execution slot and register a running job; returns [`Busy`] if taken.
    pub fn start(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
    ) -> Result<JobHandle, Busy> {
        self.start_with_batch(project_id, client_id, kind, title, None)
    }

    /// Same, but records which batch this job belongs to (for retries).
    ///
    /// No host lock here — that is decided by `config.host_lock` and read fresh by the daemon
    /// through [`Self::start_sourced`]; these shortcuts are mainly for tests.
    pub fn start_with_batch(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
        batch_id: Option<String>,
    ) -> Result<JobHandle, Busy> {
        self.start_sourced(
            project_id,
            client_id,
            kind,
            title,
            StartOptions {
                batch_id,
                source: crate::log::SOURCE_DAEMON,
                host_lock: false,
            },
        )
    }

    /// Full entry point: records batch ownership and the start source.
    pub fn start_sourced(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
        options: StartOptions<'_>,
    ) -> Result<JobHandle, Busy> {
        let StartOptions {
            batch_id,
            source,
            host_lock,
        } = options;
        let mut jobs = crate::lock::lock(&self.jobs);

        if let Some(job) = jobs
            .iter()
            .find(|job| job.project_id == project_id && job.state == JobState::Running)
        {
            return Err(Busy {
                job_id: job.id.clone(),
                kind: job.kind,
                running_seconds: now_ms().saturating_sub(job.started_at) / 1000,
                host: false,
                holder_project_id: job.project_id.clone(),
            });
        }

        // Host level: another project on the same client is busy too (shared Xcode / JDK / devices)
        if host_lock {
            if let Some(job) = jobs
                .iter()
                .find(|job| job.client_id == client_id && job.state == JobState::Running)
            {
                return Err(Busy {
                    job_id: job.id.clone(),
                    kind: job.kind,
                    running_seconds: now_ms().saturating_sub(job.started_at) / 1000,
                    host: true,
                    holder_project_id: job.project_id.clone(),
                });
            }
        }

        let id = new_job_id();
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.push_back(Job {
            id: id.clone(),
            project_id: project_id.to_string(),
            client_id: client_id.to_string(),
            kind,
            batch_id,
            source: crate::log::normalize_source(source),
            title: title.to_string(),
            state: JobState::Running,
            started_at: now_ms(),
            finished_at: 0,
            message: String::new(),
            exit_code: -1,
            artifacts: Vec::new(),
            files: 0,
            bytes: 0,
            steps: Vec::new(),
            inputs: BTreeMap::new(),
            produced: BTreeMap::new(),
            cancel: cancel.clone(),
        });
        while jobs.len() > MAX_JOBS {
            jobs.pop_front();
        }
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);

        Ok(JobHandle { id, cancel })
    }

    /// Start source of this job (the executor needs it when writing logs).
    pub fn source(&self, id: &str) -> String {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.source.clone())
            .unwrap_or_else(|| crate::log::SOURCE_DAEMON.to_string())
    }

    /// Register a **pending** job (design doc §7.6.6).
    ///
    /// Unlike [`Self::start_sourced`] this runs no project-level mutex check and does not
    /// start anything — it only queues. The job already running is untouched, which is what
    /// §7.6.6 item 2 wants: later submissions become pending rather than silently overwriting
    /// or running in parallel.
    pub fn start_queued(
        &self,
        project_id: &str,
        client_id: &str,
        kind: &'static str,
        title: &str,
        batch_id: Option<String>,
        source: &str,
    ) -> JobHandle {
        let mut jobs = crate::lock::lock(&self.jobs);
        let id = new_job_id();
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.push_back(Job {
            id: id.clone(),
            project_id: project_id.to_string(),
            client_id: client_id.to_string(),
            kind,
            batch_id,
            source: crate::log::normalize_source(source),
            title: title.to_string(),
            state: JobState::Pending,
            started_at: 0,
            finished_at: 0,
            message: "排队中".to_string(),
            exit_code: -1,
            artifacts: Vec::new(),
            files: 0,
            bytes: 0,
            steps: Vec::new(),
            inputs: BTreeMap::new(),
            produced: BTreeMap::new(),
            cancel: cancel.clone(),
        });
        while jobs.len() > MAX_JOBS {
            jobs.pop_front();
        }
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
        JobHandle { id, cancel }
    }

    /// **Atomically** promote a project's queue head from pending to running.
    ///
    /// "Is anything running" and "promote the head" must happen under one lock, otherwise two
    /// concurrent requests can both see an idle project and split one batch's steps across two
    /// parallel jobs (§7.5 forbids that).
    ///
    /// With `host_lock` on, another project running on the same client also blocks the
    /// promotion until that machine is free (design doc §7.6.6 last item).
    pub fn promote_next(&self, project_id: &str, host_lock: bool) -> Option<String> {
        self.promote_next_with(project_id, |_| host_lock)
    }

    /// Like [`Self::promote_next`], but evaluates host locking dynamically per client.
    pub fn promote_next_with(
        &self,
        project_id: &str,
        is_host_locked: impl Fn(&str) -> bool,
    ) -> Option<String> {
        // "Check idle + promote head" must stay in one critical section; the write to disk
        // happens after it, so the lock is held only for the state transition.
        let (id, snapshot) = {
            let mut jobs = crate::lock::lock(&self.jobs);
            if jobs
                .iter()
                .any(|job| job.project_id == project_id && job.state == JobState::Running)
            {
                return None;
            }
            let client_id = jobs
                .iter()
                .find(|job| job.project_id == project_id && job.state == JobState::Pending)?
                .client_id
                .clone();
            if is_host_locked(&client_id)
                && jobs
                    .iter()
                    .any(|job| job.client_id == client_id && job.state == JobState::Running)
            {
                return None;
            }
            let job = jobs
                .iter_mut()
                .find(|job| job.project_id == project_id && job.state == JobState::Pending)?;
            job.state = JobState::Running;
            job.started_at = now_ms();
            job.message = String::new();
            let id = job.id.clone();
            (id, Self::snapshot_locked(&jobs))
        };
        self.persist_snapshot(&snapshot);
        Some(id)
    }

    /// Project ids that have queued jobs (FIFO: by the first queued entry).
    ///
    /// With the host lock on, "which machine just became free" can make **another project's**
    /// entry runnable, so the wrap-up must sweep every queue, not only the project that just
    /// finished.
    pub fn projects_with_pending(&self) -> Vec<String> {
        let jobs = crate::lock::lock(&self.jobs);
        let mut ids: Vec<String> = Vec::new();
        for job in jobs.iter().filter(|job| job.state == JobState::Pending) {
            if !ids.contains(&job.project_id) {
                ids.push(job.project_id.clone());
            }
        }
        ids
    }

    /// Current state of a queued job (cancel has to tell "pending" from "running" first).
    pub fn state_of(&self, id: &str) -> Option<JobState> {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.state)
    }

    /// Explicitly end a pending job (cancel / missing queue record / restart cleanup).
    ///
    /// Only affects `Pending` — a running job has its own cancel path (`cancel` flag plus
    /// executor cooperation).
    pub fn abandon(&self, id: &str, reason: &str) -> bool {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return false;
        };
        if job.state != JobState::Pending {
            return false;
        }
        job.state = JobState::Cancelled;
        job.finished_at = now_ms();
        job.message = reason.to_string();
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
        true
    }

    /// Position of a pending job within its own project queue (1 = head); `None` when absent.
    pub fn queue_position(&self, id: &str) -> Option<usize> {
        let jobs = crate::lock::lock(&self.jobs);
        let project_id = jobs.iter().find(|job| job.id == id)?.project_id.clone();
        jobs.iter()
            .filter(|job| job.project_id == project_id && job.state == JobState::Pending)
            .position(|job| job.id == id)
            .map(|index| index + 1)
    }

    /// Queued jobs: `projectId -> [{jobId, kind, title}]` in FIFO order.
    pub fn queued_json(&self) -> serde_json::Value {
        let jobs = crate::lock::lock(&self.jobs);
        let mut map = serde_json::Map::new();
        for job in jobs.iter().filter(|job| job.state == JobState::Pending) {
            let entry = map
                .entry(job.project_id.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(list) = entry.as_array_mut() {
                list.push(serde_json::json!({
                    "jobId": job.id,
                    "kind": job.kind,
                    "title": job.title,
                }));
            }
        }
        serde_json::Value::Object(map)
    }

    /// Pre-register the batch steps so a new job already shows the whole plan.
    ///
    /// Each tuple is `(batch step index, taskId, display name, kind)`. A mid-batch retry
    /// registers only the remaining steps, but keeps their batch indices aligned.
    pub fn plan_steps(&self, id: &str, steps: &[(usize, String, String, String)]) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.steps = steps
            .iter()
            .map(|(index, task_id, name, kind)| JobStep {
                index: *index,
                task_id: task_id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                state: "pending",
                started_at: 0,
                finished_at: 0,
                message: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                exit_code: -1,
                code: String::new(),
            })
            .collect();
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Mark one step as started.
    pub fn begin_step(&self, id: &str, index: usize) {
        let mut jobs = crate::lock::lock(&self.jobs);
        if let Some(job) = jobs.iter_mut().find(|job| job.id == id) {
            if let Some(step) = job.steps.iter_mut().find(|step| step.index == index) {
                step.state = "running";
                step.started_at = now_ms();
            }
        }
    }

    /// Mark one step finished. `code` is the structured failure reason (design doc §10.1),
    /// empty on success.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_step(
        &self,
        id: &str,
        index: usize,
        ok: bool,
        cancelled: bool,
        message: &str,
        inputs: Vec<String>,
        outputs: Vec<String>,
        exit_code: i32,
        code: &str,
    ) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        if let Some(step) = job.steps.iter_mut().find(|step| step.index == index) {
            step.state = step_state(ok, cancelled);
            step.finished_at = now_ms();
            step.message = message.to_string();
            step.inputs = inputs;
            step.outputs = outputs;
            step.exit_code = exit_code;
            step.code = code.to_string();
        }
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Record the **input snapshot** of this run (design doc §7.6.7); called once per job.
    pub fn set_inputs(&self, id: &str, inputs: BTreeMap<String, String>) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.inputs = inputs;
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Read back the input snapshot of a job; `retry` replays it (design doc §7.6.7).
    pub fn inputs(&self, id: &str) -> Option<BTreeMap<String, String>> {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.inputs.clone())
    }

    /// Step outputs a job has verified (design doc §7.6.5); a mid-batch retry relies on them.
    pub fn produced(&self, id: &str) -> BTreeMap<String, String> {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.produced.clone())
            .unwrap_or_default()
    }

    /// Publish one step output: `"<step id>.outputs.<name>" -> real path`.
    pub fn publish_output(&self, id: &str, key: String, value: String) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.produced.insert(key, value);
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Publish the whole set (a queued batch seeds it with what already exists).
    pub fn set_produced(&self, id: &str, produced: BTreeMap<String, String>) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.produced = produced;
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Mark all not-yet-run steps as skipped (an earlier step failed).
    pub fn skip_steps_from(&self, id: &str, index: usize) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        let mut changed = false;
        for step in job.steps.iter_mut().filter(|step| step.index >= index) {
            if step.state == "pending" {
                step.state = "skipped";
                step.message = "前序步骤未成功,已跳过".to_string();
                changed = true;
            }
        }
        if !changed {
            return;
        }
        // Skip transitions a terminal state, so it must be durable too (S6):
        // snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// Finish up. With `cancelled` set the state becomes cancelled, even on a non-zero command.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        id: &str,
        ok: bool,
        cancelled: bool,
        message: &str,
        exit_code: i32,
        artifacts: &[String],
        files: u64,
        bytes: u64,
    ) {
        let mut jobs = crate::lock::lock(&self.jobs);
        let Some(job) = jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        job.state = if cancelled {
            JobState::Cancelled
        } else if ok {
            JobState::Ok
        } else {
            JobState::Failed
        };
        job.finished_at = now_ms();
        job.message = message.to_string();
        job.exit_code = exit_code;
        job.artifacts = artifacts.to_vec();
        job.files = files;
        job.bytes = bytes;
        // Snapshot inside the lock, flush to disk outside it (S4).
        let snapshot = Self::snapshot_locked(&jobs);
        drop(jobs);
        self.persist_snapshot(&snapshot);
    }

    /// For retry: which project / batch this job belongs to.
    pub fn meta(&self, id: &str) -> Option<(String, Option<String>, String, usize)> {
        let jobs = crate::lock::lock(&self.jobs);
        let job = jobs.iter().find(|job| job.id == id)?;
        Some((
            job.project_id.clone(),
            job.batch_id.clone(),
            job.title.clone(),
            job.first_unfinished_step().unwrap_or(0),
        ))
    }

    /// The step list of one job (separate method for the "only this step is left" case).
    pub fn steps(&self, id: &str) -> Option<Vec<serde_json::Value>> {
        let jobs = crate::lock::lock(&self.jobs);
        jobs.iter()
            .find(|job| job.id == id)
            .map(|job| job.steps.iter().map(JobStep::to_json).collect())
    }

    pub fn get(&self, id: &str) -> Option<serde_json::Value> {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(Job::to_json)
    }

    pub fn list(&self) -> Vec<serde_json::Value> {
        let jobs = crate::lock::lock(&self.jobs);
        jobs.iter().rev().map(Job::to_json).collect()
    }

    /// Prune finished (terminal) jobs. Keep running and pending jobs untouched.
    /// If `project_id` is provided, only prunes for that project; otherwise prunes all.
    /// Returns the number of pruned jobs.
    pub fn prune_finished(&self, project_id: Option<&str>) -> usize {
        let mut jobs = crate::lock::lock(&self.jobs);
        let initial_len = jobs.len();
        jobs.retain(|job| {
            if !job.state.is_terminal() {
                return true; // Keep active / pending
            }
            if let Some(pid) = project_id {
                job.project_id != pid // Keep if different project
            } else {
                false // Prune all finished
            }
        });
        let pruned = initial_len - jobs.len();
        if pruned > 0 {
            // Snapshot inside the lock, flush to disk outside it (S4).
            let snapshot = Self::snapshot_locked(&jobs);
            drop(jobs);
            self.persist_snapshot(&snapshot);
        }
        pruned
    }

    /// Cancel a job that has not finished; the executor cooperates to actually stop it.
    pub fn request_cancel(&self, id: &str) -> Result<serde_json::Value, CancelError> {
        let mut jobs = crate::lock::lock(&self.jobs);
        let job = jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or(CancelError::NotFound)?;
        if job.state.is_terminal() {
            return Err(CancelError::Finished);
        }
        job.cancel.store(true, Ordering::Relaxed);
        job.message = "cancel requested".to_string();
        Ok(job.to_json())
    }

    /// Return the cancel flag for an existing job (needed when restoring queued runs).
    pub fn cancel_flag(&self, id: &str) -> Option<Arc<AtomicBool>> {
        crate::lock::lock(&self.jobs)
            .iter()
            .find(|job| job.id == id)
            .map(|job| job.cancel.clone())
    }

    /// Running project-level jobs: `projectId -> {jobId, kind, runningSeconds}`.
    pub fn running_json(&self) -> serde_json::Value {
        let jobs = crate::lock::lock(&self.jobs);
        let mut map = serde_json::Map::new();
        for job in jobs.iter().filter(|job| job.state == JobState::Running) {
            map.insert(
                job.project_id.clone(),
                serde_json::json!({
                    "jobId": job.id,
                    "kind": job.kind,
                    "runningSeconds": now_ms().saturating_sub(job.started_at) / 1000,
                }),
            );
        }
        serde_json::Value::Object(map)
    }

    pub fn is_running(&self, project_id: &str) -> bool {
        crate::lock::lock(&self.jobs)
            .iter()
            .any(|job| job.project_id == project_id && job.state == JobState::Running)
    }
}

    fn step_state(ok: bool, cancelled: bool) -> &'static str {
    if cancelled {
        "cancelled"
    } else if ok {
        "ok"
    } else {
        "failed"
    }
}

fn new_job_id() -> String {    use rand_core::{OsRng, RngCore};
    let mut buf = [0u8; 6];
    OsRng.fill_bytes(&mut buf);
    format!("j-{}", buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design doc §7.6.6: queued jobs do not steal the lock; once a project frees up they
    /// advance one by one in FIFO order.
    #[test]
    fn queued_jobs_wait_for_their_turn_in_fifo_order() {
        let registry = Registry::default();
        let running = registry.start("p-1", "c-1", "build", "t").unwrap();

        // Queueing skips the mutex check: queue even while something runs (§7.6.6 item 2)
        let first = registry.start_queued("p-1", "c-1", "copy", "first", None, "webui");
        let second = registry.start_queued("p-1", "c-1", "copy", "second", None, "webui");
        assert_eq!(registry.state_of(&first.id), Some(JobState::Pending));
        assert_eq!(registry.queue_position(&first.id), Some(1));
        assert_eq!(registry.queue_position(&second.id), Some(2));
        // A queued job holds no execution right
        assert_eq!(registry.promote_next("p-1", false), None, "no promotion while another job runs");
        assert_eq!(registry.state_of(&running.id), Some(JobState::Running));

        // Another project keeps its own queue
        let other = registry.start_queued("p-2", "c-1", "copy", "other", None, "webui");
        assert_eq!(registry.queue_position(&other.id), Some(1));
        assert_eq!(registry.promote_next("p-2", false), Some(other.id.clone()));

        // Only after the previous one ends can the queue head advance, and in FIFO order
        registry.finish(&running.id, true, false, "done", 0, &[], 0, 0);
        assert_eq!(registry.promote_next("p-1", false), Some(first.id.clone()));
        assert_eq!(registry.state_of(&first.id), Some(JobState::Running));
        assert_eq!(registry.queue_position(&first.id), None, "not queued after promotion");
        assert_eq!(registry.promote_next("p-1", false), None, "only one promotion at a time");

        registry.finish(&first.id, true, false, "done", 0, &[], 0, 0);
        assert_eq!(registry.promote_next("p-1", false), Some(second.id));
    }

    /// Design doc §7.6.6 item 3: a queued job can be abandoned, and only while still queued.
    #[test]
    fn pending_jobs_can_be_abandoned() {
        let registry = Registry::default();
        let running = registry.start("p-1", "c-1", "build", "t").unwrap();
        let queued = registry.start_queued("p-1", "c-1", "copy", "q", None, "cli");

        assert!(registry.abandon(&queued.id, "排队中被取消"));
        let job = registry.get(&queued.id).unwrap();
        assert_eq!(job["state"], "cancelled");
        assert_eq!(job["message"], "排队中被取消");
        // Once abandoned it is gone from the queue and will not be promoted
        assert_eq!(registry.queue_position(&queued.id), None);
        assert_eq!(registry.promote_next("p-1", false), None);
        assert!(registry.get(&queued.id).unwrap()["finishedAt"].as_u64().unwrap() > 0);

        // A running job is not handled by abandon (it has its own cancel path)
        assert!(!registry.abandon(&running.id, "试试"));
        assert_eq!(registry.state_of(&running.id), Some(JobState::Running));
        assert!(!registry.abandon("j-nope", "不存在"));
    }

    /// A queued job must not report an absurd elapsed time (it has not started yet).
    #[test]
    fn pending_job_reports_zero_elapsed() {
        let registry = Registry::default();
        let queued = registry.start_queued("p-1", "c-1", "copy", "q", None, "cli");
        let job = registry.get(&queued.id).unwrap();
        assert_eq!(job["state"], "pending");
        assert_eq!(job["elapsedSeconds"], 0);
        assert_eq!(job["startedAt"], 0);
        // While queued it is not running, so no running-time entry is reported
        assert!(registry.running_json().get("p-1").is_none());
        assert_eq!(registry.queued_json()["p-1"][0]["jobId"], queued.id.as_str());
    }

    /// Design doc §7.6.6 item 5: after a restart queued jobs do not resume on their own,
    /// but they must be **explicitly marked**.
    #[test]
    fn restore_marks_pending_jobs_as_cancelled() {
        let dir = std::env::temp_dir().join("cgw-jobs-queue-restore");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.json");
        let _ = std::fs::remove_file(&path);

        {
            let registry = Registry::default();
            registry.attach_store(path.clone());
            let running = registry.start("p-1", "c-1", "build", "t").unwrap();
            registry.start_queued("p-2", "c-1", "copy", "q", None, "webui");
            registry.finish(&running.id, true, false, "done", 0, &[], 0, 0);
        }

        let registry = Registry::default();
        registry.attach_store(path.clone());
        registry.restore();
        let list = registry.list();
        let pending = list
            .iter()
            .find(|job| job["title"] == "q")
            .expect("the queued job should be restored");
        assert_eq!(pending["state"], "cancelled");
        assert!(
            pending["message"]
                .as_str()
                .unwrap()
                .contains("重新提交"),
            "{pending}"
        );
        assert!(registry.promote_next("p-2", false).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_with_active_queue_preserves_pending_jobs() {
        let dir = std::env::temp_dir().join("cgw-jobs-queue-restore-active");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.json");
        let _ = std::fs::remove_file(&path);

        let queued_id = {
            let registry = Registry::default();
            registry.attach_store(path.clone());
            let handle = registry.start_queued("p-2", "c-1", "copy", "q", None, "webui");
            handle.id
        };

        let registry = Registry::default();
        registry.attach_store(path.clone());
        registry.restore_with_active_queue(&[queued_id.clone()]);
        let list = registry.list();
        let pending = list
            .iter()
            .find(|job| job["id"] == queued_id.as_str())
            .expect("the queued job should be restored");
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["message"], "排队中");
        assert_eq!(registry.promote_next("p-2", false), Some(queued_id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Design doc §7.6.6 last item: **host-level mutex** -- projects on the same client
    /// cannot run at the same time either.
    ///
    /// What is shared is the Xcode / JDK / devices on that machine, not "which project", so it is
    /// **another lock** (the `host_lock` switch), not part of the project lock.
    #[test]
    fn host_lock_serialises_projects_on_one_client() {
        let registry = Registry::default();
        let running = registry
            .start_sourced("p-1", "c-1", "build", "t", StartOptions { source: "webui", ..StartOptions::default() })
            .unwrap();

        // Off (default): another project on the same machine runs as before, nothing changed
        let other = registry
            .start_sourced("p-2", "c-1", "build", "t", StartOptions { source: "webui", ..StartOptions::default() })
            .unwrap();
        registry.finish(&other.id, true, false, "done", 0, &[], 0, 0);

        // On: the same client is busy -> host-level conflict, naming the holder
        match registry.start_sourced("p-3", "c-1", "build", "t", StartOptions { source: "webui", host_lock: true, ..StartOptions::default() }) {
            Ok(_) => panic!("must not allow this while the host mutex is on"),
            Err(busy) => {
                assert!(busy.host, "must be flagged as host-level, not project-level");
                assert_eq!(busy.job_id, running.id);
                assert_eq!(busy.holder_project_id, "p-1");
            }
        }
        // A different client is unaffected (the machine is locked, not the project)
        let elsewhere = registry
            .start_sourced("p-4", "c-2", "build", "t", StartOptions { source: "webui", host_lock: true, ..StartOptions::default() })
            .unwrap();

        // A queued job also waits for that machine: promotion checks the host lock too
        let queued = registry.start_queued("p-5", "c-1", "copy", "q", None, "webui");
        assert_eq!(
            registry.promote_next("p-5", true),
            None,
            "the same machine is still busy, the queued job cannot be promoted"
        );
        assert_eq!(registry.projects_with_pending(), vec!["p-5".to_string()]);
        assert_eq!(
            registry.promote_next("p-5", false),
            Some(queued.id.clone()),
            "with the host lock off promotion works (the switch is read live)"
        );

        registry.finish(&elsewhere.id, true, false, "done", 0, &[], 0, 0);
        registry.finish(&running.id, true, false, "done", 0, &[], 0, 0);
        registry.finish(&queued.id, true, false, "done", 0, &[], 0, 0);
    }

    #[test]
    fn one_running_job_per_project() {
        let registry = Registry::default();

        let first = registry.start("p-1", "c-1", "build", "t").unwrap();
        // A second job on the same project is blocked, reporting the holder jobId
        match registry.start("p-1", "c-1", "push", "t") {
            Ok(_) => panic!("a second job on the same project must not start"),
            Err(busy) => {
                assert_eq!(busy.job_id, first.id);
                assert_eq!(busy.kind, "build");
            }
        }
        // Other projects are unaffected
        assert!(registry.start("p-2", "c-1", "build", "t").is_ok());

        // After it ends the project can start again
        registry.finish(&first.id, true, false, "done", 0, &[], 0, 0);
        assert!(registry.start("p-1", "c-1", "pull", "t").is_ok());
    }

    #[test]
    fn finish_records_terminal_state() {
        let registry = Registry::default();
        let handle = registry.start("p-1", "c-1", "build", "iOS Release").unwrap();

        let running = registry.get(&handle.id).unwrap();
        assert_eq!(running["state"], "running");
        assert_eq!(running["title"], "iOS Release");
        assert_eq!(running["finishedAt"], 0);

        let artifacts = vec!["out/app.app".to_string(), "out/app.dSYM".to_string()];
        registry.finish(&handle.id, true, false, "构建完成", 0, &artifacts, 3, 1024);
        let done = registry.get(&handle.id).unwrap();
        assert_eq!(done["state"], "ok");
        assert_eq!(done["exitCode"], 0);
        assert_eq!(done["artifactPath"], "out/app.app");
        assert_eq!(done["artifacts"][1], "out/app.dSYM");
        assert_eq!(done["files"], 3);
        assert_eq!(done["bytes"], 1024);
        assert!(done["finishedAt"].as_u64().unwrap() > 0);

        // Cancelling after finishing must be rejected
        assert!(matches!(
            registry.request_cancel(&handle.id),
            Err(CancelError::Finished)
        ));
    }

    #[test]
    fn cancel_marks_flag_and_reports_unknown_ids() {
        let registry = Registry::default();
        let handle = registry.start("p-1", "c-1", "build", "t").unwrap();

        let job = registry.request_cancel(&handle.id).unwrap();
        assert_eq!(job["cancelRequested"], true);
        assert!(handle.cancel.load(Ordering::Relaxed));

        // Cancelling does not change state by itself; the executor confirms the stop
        assert_eq!(registry.get(&handle.id).unwrap()["state"], "running");

        registry.finish(&handle.id, false, true, "已取消", -1, &[], 0, 0);
        assert_eq!(registry.get(&handle.id).unwrap()["state"], "cancelled");

        assert!(matches!(
            registry.request_cancel("j-nope"),
            Err(CancelError::NotFound)
        ));
        assert!(registry.get("j-nope").is_none());
    }

    #[test]
    fn history_is_bounded() {
        let registry = Registry::default();
        for index in 0..(MAX_JOBS + 5) {
            let handle = registry
                .start(&format!("p-{index}"), "c-1", "build", "t")
                .unwrap();
            registry.finish(&handle.id, true, false, "done", 0, &[], 0, 0);
        }
        assert_eq!(registry.list().len(), MAX_JOBS);
    }

    fn plan() -> Vec<(usize, String, String, String)> {
        vec![
            (0, "t-a".to_string(), "构建 A".to_string(), "build".to_string()),
            (1, "t-b".to_string(), "复制 B".to_string(), "copy".to_string()),
            (2, "t-c".to_string(), "导出 C".to_string(), "export".to_string()),
        ]
    }

    /// Design doc §7.3: each step records taskId / times / inputs / outputs / result.
    #[test]
    fn batch_steps_are_recorded_in_order() {
        let registry = Registry::default();
        let handle = registry
            .start_with_batch("p-1", "c-1", "batch", "Release pipeline", Some("b-1".to_string()))
            .unwrap();
        registry.plan_steps(&handle.id, &plan());

        let planned = registry.steps(&handle.id).unwrap();
        assert_eq!(planned.len(), 3);
        assert_eq!(planned[0]["taskId"], "t-a");
        assert_eq!(planned[0]["state"], "pending");

        registry.begin_step(&handle.id, 0);
        assert_eq!(registry.steps(&handle.id).unwrap()[0]["state"], "running");
        registry.finish_step(
            &handle.id,
            0,
            true,
            false,
            "构建完成",
            vec!["remote:src".to_string()],
            vec!["remote:out.app".to_string()],
            0,
            "",
        );

        // Second step fails -> third step is skipped
        registry.begin_step(&handle.id, 1);
        registry.finish_step(
            &handle.id,
            1,
            false,
            false,
            "目标已存在",
            vec![],
            vec![],
            17,
            "artifactMissing",
        );
        registry.skip_steps_from(&handle.id, 2);

        let steps = registry.steps(&handle.id).unwrap();
        assert_eq!(steps[0]["state"], "ok");
        assert_eq!(steps[0]["inputs"][0], "remote:src");
        assert_eq!(steps[0]["outputs"][0], "remote:out.app");
        assert_eq!(steps[1]["state"], "failed");
        assert_eq!(steps[1]["exitCode"], 17);
        // The structured error code must reach the job JSON (design doc §10.1)
        assert_eq!(steps[1]["code"], "artifactMissing");
        assert_eq!(steps[2]["state"], "skipped");
        assert!(steps[0]["finishedAt"].as_u64().unwrap() > 0);

        // The retry start point is the first unsuccessful step
        let (project, batch, _title, from) = registry.meta(&handle.id).unwrap();
        assert_eq!(project, "p-1");
        assert_eq!(batch.as_deref(), Some("b-1"));
        assert_eq!(from, 1);
    }

    /// A fully successful batch: no step to retry.
    #[test]
    fn fully_successful_batch_has_no_retry_point() {
        let registry = Registry::default();
        let handle = registry
            .start_with_batch("p-1", "c-1", "batch", "t", Some("b-1".to_string()))
            .unwrap();
        registry.plan_steps(&handle.id, &plan());
        for index in 0..3 {
            registry.begin_step(&handle.id, index);
            registry.finish_step(
                &handle.id, index, true, false, "ok", vec![], vec![], 0, "",
            );
        }
        registry.finish(&handle.id, true, false, "全部完成", 0, &[], 0, 0);

        let (_, _, _, from) = registry.meta(&handle.id).unwrap();
        assert_eq!(from, 0);
        assert_eq!(
            registry.get(&handle.id).unwrap()["retryFromStep"],
            serde_json::Value::Null
        );
    }

    /// Retry from step 2: the new job registers only the remaining steps, keeping
    /// the original batch indices.
    #[test]
    fn retry_plan_keeps_batch_indices() {
        let registry = Registry::default();
        let handle = registry
            .start_with_batch("p-1", "c-1", "batch", "t", Some("b-1".to_string()))
            .unwrap();
        registry.plan_steps(&handle.id, &plan()[2..]);

        let steps = registry.steps(&handle.id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0]["index"], 2);
        assert_eq!(steps[0]["taskId"], "t-c");

        registry.begin_step(&handle.id, 2);
        registry
            .finish_step(&handle.id, 2, false, false, "失败", vec![], vec![], 1, "")
            ;
        let (_, _, _, from) = registry.meta(&handle.id).unwrap();
        assert_eq!(from, 2);
    }

    /// Design doc §12.1: recent job state survives a daemon restart.
    ///
    /// A job that was still running must become failed: its execution thread died with the
    /// process, and restoring it as running would make someone wait forever.
    #[test]
    fn history_survives_a_restart() {
        let dir = std::env::temp_dir().join("cgw-jobs-restore");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.json");
        let _ = std::fs::remove_file(&path);

        // First "process": finish one batch (success), then leave one job still running
        {
            let registry = Registry::default();
            registry.attach_store(path.clone());

            let done = registry
                .start_with_batch("p-1", "c-1", "batch", "release", Some("b-1".to_string()))
                .unwrap();
            registry.plan_steps(&done.id, &plan());
            registry.begin_step(&done.id, 0);
            registry.finish_step(
                &done.id,
                0,
                true,
                false,
                "ok",
                vec!["in".into()],
                vec!["out".into()],
                0,
                "",
            );
            registry.begin_step(&done.id, 1);
            registry.finish_step(
                &done.id,
                1,
                false,
                false,
                "目标已存在",
                vec![],
                vec![],
                17,
                "artifactMissing",
            );
            registry.skip_steps_from(&done.id, 2);
            registry.finish(&done.id, false, false, "第 2 步失败", 17, &["a.app".to_string()], 3, 4096);

            // Deliberately not finished, to simulate "still running at restart"
            let _running = registry.start_sourced("p-2", "c-1", "build", "ios", StartOptions { source: "webui", ..StartOptions::default() }).unwrap();
        }

        // Second "process": restore from disk
        let registry = Registry::default();
        registry.attach_store(path.clone());
        registry.restore();

        let list = registry.list();
        assert_eq!(list.len(), 2, "both history entries should come back");

        let batch = list.iter().find(|job| job["kind"] == "batch").expect("the batch record should be restored");
        assert_eq!(batch["state"], "failed");
        assert_eq!(batch["exitCode"], 17);
        assert_eq!(batch["artifacts"][0], "a.app");
        assert_eq!(batch["files"], 3);
        // Every step result must survive too: the retry point is still step 2
        assert_eq!(batch["retryFromStep"], 1);
        let steps = batch["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["state"], "ok");
        assert_eq!(steps[0]["outputs"][0], "out");
        assert_eq!(steps[1]["state"], "failed");
        assert_eq!(steps[2]["state"], "skipped");

        let interrupted = list.iter().find(|job| job["kind"] == "build").expect("the build record should be restored");
        assert_eq!(interrupted["state"], "failed", "it cannot still be running across processes");
        assert!(interrupted["message"].as_str().unwrap().contains("中断"), "{}", interrupted["message"]);
        assert_eq!(interrupted["source"], "webui", "the source marker must survive too");

        // Restored history must not block new work: that "running" project can start again
        assert!(registry.start("p-2", "c-1", "build", "again").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt or missing file on disk must not block startup.
    #[test]
    fn restore_tolerates_broken_or_missing_files() {
        let dir = std::env::temp_dir().join("cgw-jobs-broken");
        std::fs::create_dir_all(&dir).unwrap();

        let registry = Registry::default();
        registry.attach_store(dir.join("nope.json"));
        registry.restore();
        assert!(registry.list().is_empty());

        let path = dir.join("broken.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let registry = Registry::default();
        registry.attach_store(path.clone());
        registry.restore();
        assert!(registry.list().is_empty());

        // With no store path (unit-test default) everything behaves as before
        let registry = Registry::default();
        registry.restore();
        let handle = registry.start("p-1", "c-1", "build", "t").unwrap();
        registry.finish(&handle.id, true, false, "ok", 0, &[], 0, 0);
        assert_eq!(registry.list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_finished_removes_only_terminal_jobs() {
        let registry = Registry::default();
        let h1 = registry.start("p-1", "c-1", "build", "Job 1").unwrap();
        let h2 = registry.start("p-2", "c-1", "build", "Job 2").unwrap();
        registry.finish(&h1.id, true, false, "ok", 0, &[], 0, 0);
        // h1 is finished, h2 is running
        assert_eq!(registry.list().len(), 2);

        // Pruning for p-2 (which is running) removes nothing
        assert_eq!(registry.prune_finished(Some("p-2")), 0);
        assert_eq!(registry.list().len(), 2);

        // Pruning for p-1 removes h1
        assert_eq!(registry.prune_finished(Some("p-1")), 1);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0]["id"], h2.id);

        // Finish h2, then prune all
        registry.finish(&h2.id, false, false, "err", 1, &[], 0, 0);
        assert_eq!(registry.prune_finished(None), 1);
        assert!(registry.list().is_empty());
    }
}