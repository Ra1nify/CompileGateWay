import type { JSX } from "react";
import {
  Badge,
  Button,
  Card,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  MessageBar,
  MessageBarBody,
  Spinner,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  Add20Regular,
  ArrowClockwise20Regular,
  ArrowDownload20Regular,
  ArrowUpload20Regular,
  Archive20Regular,
  Braces20Regular,
  Clock20Regular,
  Code20Regular,
  Copy20Regular,
  Delete20Regular,
  DismissCircle20Regular,
  DocumentText20Regular,
  Edit20Regular,
  FolderAdd20Regular,
  FolderOpen20Regular,
  Layer20Regular,
  Play20Regular,
} from "@fluentui/react-icons";
import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  apiCancelJob,
  apiClearJobs,
  apiExportProject,
  apiCreateProject,
  apiDeleteBatch,
  apiDeleteProject,
  apiDeleteTask,
  apiEnqueueBatch,
  apiEnqueueTask,
  apiGetJob,
  apiListJobs,
  apiListManifests,
  apiPreviewTask,
  apiRetryJob,
  apiRunBatch,
  apiStartPull,
  apiStartPush,
  apiStartTask,
  type Batch,
  type Job,
  type JobKind,
  type JobStartResponse,
  type JobState,
  type Location,
  type ManifestSummary,
  type Project,
  type RunningJob,
  type Status,
  type Task,
} from "../../api";
import { makeT } from "../../i18n/strings";
import { CleanCheckbox, ConfirmDialog } from "../../components/ConfirmDialog";
import { AddToBatchDialog, BatchDialog } from "../../components/BatchDialog";
import { FolderPickerDialog } from "../../components/FolderPickerDialog";
import { BatchJsonDialog } from "../../components/BatchJsonDialog";
import { ExportDialog } from "../../components/ExportDialog";
import { JobDetailDialog } from "../../components/JobDetailDialog";
import { TaskDialog } from "../../components/TaskDialog";
import { TaskJsonDialog } from "../../components/TaskJsonDialog";
import { TreeDialog } from "../../components/TreeDialog";
import { formatBytes, formatDateTime } from "../../format";

/** Poll interval for job status, in ms. */
const POLL_INTERVAL_MS = 1000;
/** Max rows shown in the "recent jobs" panel. */
const RECENT_LIMIT = 8;

const useStyles = makeStyles({
  hint: {
    margin: "0 0 12px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  list: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
    maxWidth: "1080px",
  },
  card: {
    width: "100%",
    borderRadius: tokens.borderRadiusLarge,
    padding: "14px 16px",
    display: "flex",
    flexDirection: "column",
    rowGap: "10px",
  },
  row: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    justifyContent: "space-between",
    columnGap: "20px",
    rowGap: "12px",
  },
  fields: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    columnGap: "22px",
    rowGap: "12px",
    minWidth: 0,
  },
  group: {
    display: "flex",
    flexDirection: "column",
    rowGap: "5px",
    flex: "0 0 auto",
    maxWidth: "100%",
  },
  label: {
    fontSize: "11px",
    color: tokens.colorNeutralForeground3,
    letterSpacing: "0.4px",
  },
  value: {
    fontSize: tokens.fontSizeBase300,
    overflowWrap: "anywhere",
  },
  actions: {
    display: "flex",
    columnGap: "8px",
    flex: "0 0 auto",
    marginLeft: "auto",
  },
  meta: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    lineHeight: "1.6",
    overflowWrap: "anywhere",
  },
  empty: {
    padding: "12px 2px",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground3,
  },
  jobBar: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    columnGap: "8px",
    rowGap: "6px",
    padding: "6px 10px",
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground2,
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
  },
  jobTitle: {
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground1,
    overflowWrap: "anywhere",
  },
  buildSection: {
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    paddingTop: "10px",
  },
  buildHead: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    columnGap: "12px",
    rowGap: "6px",
    flexWrap: "wrap",
  },
  sectionLabel: {
    fontSize: "11px",
    letterSpacing: "0.4px",
    color: tokens.colorNeutralForeground3,
  },
  task: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    justifyContent: "space-between",
    columnGap: "16px",
    rowGap: "8px",
    padding: "8px 10px",
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  taskFields: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    columnGap: "14px",
    rowGap: "6px",
    minWidth: 0,
  },
  taskName: {
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase300,
    overflowWrap: "anywhere",
  },
  taskBits: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    flexWrap: "wrap",
    minWidth: 0,
  },
  taskText: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    overflowWrap: "anywhere",
  },
  taskMono: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    overflowWrap: "anywhere",
  },
  taskActions: {
    display: "flex",
    columnGap: "4px",
    flex: "0 0 auto",
    marginLeft: "auto",
  },
  recent: {
    display: "flex",
    flexDirection: "column",
    rowGap: "6px",
    maxWidth: "1080px",
    marginTop: "12px",
    padding: "12px 16px",
    borderRadius: tokens.borderRadiusLarge,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  recentRow: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    columnGap: "10px",
    rowGap: "4px",
    padding: "4px 0",
    borderTop: `1px solid ${tokens.colorNeutralStroke3}`,
  },
  recentTitle: {
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase300,
    overflowWrap: "anywhere",
  },
  recentText: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    overflowWrap: "anywhere",
  },
  command: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12.5px",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
    margin: 0,
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    backgroundColor: tokens.colorNeutralBackgroundInverted,
    color: tokens.colorNeutralForegroundInverted,
  },
  dialogBody: {
    display: "flex",
    flexDirection: "column",
    rowGap: "10px",
  },
  dialogSurface: {
    maxWidth: "680px",
    width: "680px",
  },
});

/** One notice on a card: server text, or a local error with an i18n prefix. */
type CardNotice =
  | { kind: "server"; intent: "warning" | "error"; message: string }
  | { kind: "startFailed"; message: string }
  | { kind: "pollFailed"; message: string };

/** Placeholder after click until the first apiGetJob returns; buttons stay disabled. */
interface StartingJob {
  /** "" until a jobId is known. */
  jobId: string;
  kind: JobKind;
  title: string;
}

interface PendingCancel {
  projectId: string;
  jobId: string;
}

interface PreviewState {
  title: string;
  command: string;
}

function errorText(err: unknown): string {
  return err instanceof ApiError ? err.message : String(err);
}

function sortJobs(list: readonly Job[]): Job[] {
  return [...list].sort((a, b) => b.startedAt - a.startedAt);
}

/**
 * Seconds elapsed. Prefer computing from startedAt so it ticks on each poll;
 * fall back to the server's elapsedSeconds when timestamps are unusable.
 */
function elapsedOf(job: Job): number {
  if (job.startedAt > 0) {
    const end = job.finishedAt > 0 ? job.finishedAt : Date.now();
    const seconds = Math.floor((end - job.startedAt) / 1000);
    if (Number.isFinite(seconds) && seconds >= 0) {
      return seconds;
    }
  }
  return Math.max(0, Math.floor(job.elapsedSeconds || 0));
}

function resultIntent(state: JobState): "success" | "warning" | "error" {
  switch (state) {
    case "ok":
      return "success";
    case "cancelled":
    case "pending":
      return "warning";
    default:
      return "error";
  }
}

function stateColor(state: JobState): "success" | "warning" | "danger" | "informative" {
  switch (state) {
    case "ok":
      return "success";
    case "cancelled":
    case "pending":
      return "warning";
    case "failed":
      return "danger";
    default:
      return "informative";
  }
}

/**
 * Substitute `{{var}}` in an artifact path with the task's own values before display.
 * Real templating belongs to the backend; this only avoids showing raw `{{scheme}}`.
 */
export function renderArtifact(
  template: string,
  task: Task,
  manifest: ManifestSummary | null | undefined,
): string {
  if (!template) {
    return "";
  }
  return template.replace(/\{\{\s*([^}]+?)\s*\}\}/g, (whole, rawName: string) => {
    const name = rawName.trim();
    const fromTask = task.variables?.[name];
    if (fromTask !== undefined && fromTask !== "") {
      return fromTask;
    }
    const declared = manifest?.variables?.find((v) => v.name === name);
    return declared?.default ?? whole;
  });
}

/**
 * Projects tab: import host folders plus push / pull / build jobs.
 *
 * All three exec endpoints are async: they return only a jobId, then we poll
 * `/api/jobs/{id}` every 1000ms until state is no longer running. One setInterval
 * per running job, all cleared in the effect cleanup on unmount or client switch.
 */
export function ProjectsTab({
  clientId,
  projects,
  running,
  status,
  onReload,
}: {
  clientId: string;
  projects: Project[];
  /** projectId -> running job (from the running map of /api/projects). */
  running: Record<string, RunningJob>;
  status: Status;
  onReload: () => Promise<void>;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<Project | null>(null);
  const [pendingPush, setPendingPush] = useState<Project | null>(null);
  const [pendingPull, setPendingPull] = useState<Project | null>(null);
  const [exportProject, setExportProject] = useState<Project | null>(null);
  const [batchJson, setBatchJson] = useState<{ project: Project; batch: Batch | null } | null>(
    null,
  );
  const [taskJson, setTaskJson] = useState<{ project: Project; task: Task | null } | null>(
    null,
  );

  // Jobs: the map keeps running and just-finished jobs; cards look them up by projectId.
  const [jobs, setJobs] = useState<Record<string, Job>>({});
  const [notices, setNotices] = useState<Record<string, CardNotice>>({});
  const [starting, setStarting] = useState<Record<string, StartingJob>>({});
  const [cancelling, setCancelling] = useState<string | null>(null);
  const [pendingCancel, setPendingCancel] = useState<PendingCancel | null>(null);
  const [recent, setRecent] = useState<Job[]>([]);
  const [selectedJob, setSelectedJob] = useState<Job | null>(null);

  // One timer per jobId. resumedRef tracks auto-resumed jobIds to avoid retry loops.
  const timersRef = useRef<Map<string, number>>(new Map());
  const resumedRef = useRef<Set<string>>(new Set());
  const aliveRef = useRef(true);
  // jobIds with a request currently in flight (S17): the interval fires on a fixed cadence,
  // so a slow `apiGetJob` would otherwise stack overlapping requests for the same job.
  const pollingRef = useRef<Set<string>>(new Set());

  // Tasks and batch workflows
  const [manifests, setManifests] = useState<ManifestSummary[]>([]);
  const [taskDialog, setTaskDialog] = useState<{ project: Project; task: Task | null } | null>(
    null,
  );
  const [pendingTaskDelete, setPendingTaskDelete] = useState<{
    project: Project;
    task: Task;
  } | null>(null);
  const [batchDialog, setBatchDialog] = useState<{ project: Project; batch: Batch | null } | null>(
    null,
  );
  const [pendingBatchDelete, setPendingBatchDelete] = useState<{
    project: Project;
    batch: Batch;
  } | null>(null);
  const [treeProject, setTreeProject] = useState<Project | null>(null);
  /** Second confirmation for destructive jobs (design doc §9). */
  const [pendingRun, setPendingRun] = useState<{ project: Project; task: Task } | null>(null);
  /** Entry point for "add to batch workflow". */
  const [addToBatch, setAddToBatch] = useState<{ project: Project; task: Task } | null>(null);
  /** Directory picker: chooses the source folder (design doc §6.1). */
  const [browseFor, setBrowseFor] = useState<{ kind: "newProject" } | null>(null);
  const [preview, setPreview] = useState<PreviewState | null>(null);
  const [copied, setCopied] = useState(false);
  const [clearJobsConfirm, setClearJobsConfirm] = useState(false);

  async function handleClearJobs(): Promise<void> {
    try {
      await apiClearJobs();
      await refreshRecent();
    } catch {
      // Ignore
    } finally {
      setClearJobsConfirm(false);
    }
  }

  // Manifests only supply display names and default artifact paths; fall back to id.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const result = await apiListManifests();
        if (!cancelled) {
          setManifests(result.manifests ?? []);
        }
      } catch {
        // Degraded: task rows show the manifest id.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // ------------------------------------------------------------------ Jobs

  function stopPoll(jobId: string): void {
    const timer = timersRef.current.get(jobId);
    if (timer !== undefined) {
      window.clearInterval(timer);
      timersRef.current.delete(jobId);
    }
    pollingRef.current.delete(jobId);
  }

  function stopAllPolls(): void {
    for (const timer of timersRef.current.values()) {
      window.clearInterval(timer);
    }
    timersRef.current.clear();
    // Any in-flight request will still settle, but a later start must not be blocked by
    // its guard entry, so drop the markers too (S17).
    pollingRef.current.clear();
  }

  /** Start polling a job; no-op if it is already being polled. */
  function startPoll(jobId: string, projectId: string): void {
    if (!jobId || timersRef.current.has(jobId)) {
      return;
    }
    const timer = window.setInterval(() => void pollOnce(jobId, projectId), POLL_INTERVAL_MS);
    timersRef.current.set(jobId, timer);
    // Fetch immediately instead of waiting for the first 1s tick.
    void pollOnce(jobId, projectId);
  }

  /** For auto-resume (on mount / discovered in list): each jobId is tried once. */
  function resumePoll(jobId: string, projectId: string): void {
    if (!jobId || timersRef.current.has(jobId) || resumedRef.current.has(jobId)) {
      return;
    }
    resumedRef.current.add(jobId);
    startPoll(jobId, projectId);
  }

  async function pollOnce(jobId: string, projectId: string): Promise<void> {
    // Skip if a previous poll for this job has not returned yet (S17).
    if (pollingRef.current.has(jobId)) {
      return;
    }
    pollingRef.current.add(jobId);
    try {
      const job = await apiGetJob(jobId);
      if (!aliveRef.current) {
        return;
      }
      setJobs((current) => ({ ...current, [job.id]: job }));
      clearStarting(projectId);
      if (job.state === "running" || job.state === "pending") {
        return;
      }
      // Finished: stop the timer and refresh recent jobs / project registry.
      stopPoll(job.id);
      void refreshRecent();
      void onReload();
    } catch (err) {
      if (!aliveRef.current) {
        return;
      }
      stopPoll(jobId);
      clearStarting(projectId);
      setNotice(projectId, { kind: "pollFailed", message: errorText(err) });
    } finally {
      pollingRef.current.delete(jobId);
    }
  }

  /** Last 50 jobs, newest first. On failure keep as-is; this panel is auxiliary. */
  async function refreshRecent(): Promise<void> {
    try {
      const result = await apiListJobs();
      if (!aliveRef.current) {
        return;
      }
      const list = sortJobs(result.jobs ?? []);
      setRecent(list);
      // Still running in the list (started before this tab opened): keep polling.
      for (const job of list) {
        if ((job.state === "running" || job.state === "pending") && job.clientId === clientId) {
          resumePoll(job.id, job.projectId);
        }
      }
    } catch {
      // Ignore: the recent-jobs panel is not part of the main flow.
    }
  }

  function setNotice(projectId: string, notice: CardNotice): void {
    setNotices((current) => ({ ...current, [projectId]: notice }));
  }

  function clearNotice(projectId: string): void {
    setNotices((current) => {
      if (!(projectId in current)) {
        return current;
      }
      const next = { ...current };
      delete next[projectId];
      return next;
    });
  }

  function clearStarting(projectId: string): void {
    setStarting((current) => {
      if (!(projectId in current)) {
        return current;
      }
      const next = { ...current };
      delete next[projectId];
      return next;
    });
  }

  /**
   * Start one execution. The endpoint returns a jobId right away; polling follows.
   * If the project already has a running job the server returns 409 projectBusy;
   * show its message verbatim.
   */
  async function startJob(
    project: Project,
    kind: JobKind,
    title: string,
    start: () => Promise<JobStartResponse>,
  ): Promise<void> {
    clearNotice(project.id);
    setStarting((current) => ({ ...current, [project.id]: { jobId: "", kind, title } }));
    try {
      const result = await start();
      if (!aliveRef.current) {
        return;
      }
      // Record it so a stale running map cannot restart this finished job.
      resumedRef.current.add(result.jobId);
      setStarting((current) => ({
        ...current,
        [project.id]: { jobId: result.jobId, kind, title },
      }));
      startPoll(result.jobId, project.id);
      void onReload();
    } catch (err) {
      if (!aliveRef.current) {
        return;
      }
      clearStarting(project.id);
      if (err instanceof ApiError && err.status === 409 && (err.code === "projectBusy" || err.code === "hostBusy")) {
        // The server already phrases what runs on this machine in the current language.
        setNotice(project.id, { kind: "server", intent: "warning", message: err.message });
        void onReload();
        return;
      }
      setNotice(project.id, { kind: "startFailed", message: errorText(err) });
    }
  }

  async function cancelJob(projectId: string, jobId: string): Promise<void> {
    setPendingCancel(null);
    setCancelling(jobId);
    try {
      const result = await apiCancelJob(jobId);
      if (!aliveRef.current) {
        return;
      }
      setJobs((current) => ({ ...current, [result.job.id]: result.job }));
      if (result.job.state === "running") {
        // Cancel accepted (remote connection still closing); keep polling until it ends.
        startPoll(jobId, projectId);
      } else {
        stopPoll(jobId);
        void refreshRecent();
        void onReload();
      }
    } catch (err) {
      if (!aliveRef.current) {
        return;
      }
      stopPoll(jobId);
      setNotice(projectId, { kind: "server", intent: "error", message: errorText(err) });
      void onReload();
    } finally {
      if (aliveRef.current) {
        setCancelling(null);
      }
    }
  }

  // Fetch recent jobs once on mount; clear all timers on unmount (tab or page switch).
  // The outer component carries key={client.id}, so a client switch remounts.
  useEffect(() => {
    aliveRef.current = true;
    void refreshRecent();
    return () => {
      aliveRef.current = false;
      stopAllPolls();
    };
    // Run once: only refs and stable setState are used here.
  }, []);

  // Jobs present in the running map of /api/projects but not tracked locally
  // (started in another browser): take them over.
  useEffect(() => {
    const projectIds = new Set(
      projects.filter((project) => project.clientId === clientId).map((project) => project.id),
    );
    for (const [projectId, info] of Object.entries(running)) {
      if (!projectIds.has(projectId)) {
        continue;
      }
      resumePoll(info?.jobId ?? "", projectId);
    }
    // Only react to running changes: projects / clientId are fixed per mount.
  }, [running]);

  const mine = projects.filter((project) => project.clientId === clientId);

  function runningJobOf(projectId: string): Job | null {
    let found: Job | null = null;
    for (const job of Object.values(jobs)) {
      if (job.projectId !== projectId || (job.state !== "running" && job.state !== "pending")) {
        continue;
      }
      if (!found || job.startedAt >= found.startedAt) {
        found = job;
      }
    }
    return found;
  }

  function finishedJobOf(projectId: string): Job | null {
    let found: Job | null = null;
    for (const job of Object.values(jobs)) {
      if (job.projectId !== projectId || job.state === "running" || job.state === "pending") {
        continue;
      }
      if (!found || job.startedAt >= found.startedAt) {
        found = job;
      }
    }
    return found;
  }

  function manifestOf(id: string): ManifestSummary | null {
    return manifests.find((item) => item.id === id) ?? null;
  }

  function manifestLabel(id: string): string {
    return manifestOf(id)?.displayName ?? id;
  }

  function kindOf(task: Task): JobKind {
    return task.kind;
  }

  /** Default UI title for a task: manifest name for build, kind name otherwise. */
  function taskTitle(task: Task): string {
    return task.kind === "build" ? manifestLabel(task.manifest ?? "") : kindLabel(task.kind);
  }

  function kindLabel(kind: JobKind): string {
    switch (kind) {
      case "build":
        return t("jobKindBuild");
      case "push":
        return t("jobKindPush");
      case "pull":
        return t("jobKindPull");
      case "copy":
        return t("jobKindCopy");
      case "move":
        return t("jobKindMove");
      case "delete":
        return t("jobKindDelete");
      case "export":
        return t("jobKindExport");
      default:
        return t("jobKindBatch");
    }
  }

  /** Source -> target summary for file tasks (design doc §9: show source, target, overwrite). */
  function locationText(location: Location | undefined): string {
    if (!location) {
      return "-";
    }
    const side = location.side === "remote" ? t("sideRemote") : t("sideHost");
    return `${side}:${location.path}`;
  }

  function taskDetail(task: Task): string {
    switch (task.kind) {
      case "build":
        return [manifestLabel(task.manifest ?? ""), task.variables
          ? Object.entries(task.variables)
              .slice(0, 2)
              .map(([key, value]) => `${key}=${value}`)
              .join(" · ")
          : ""]
          .filter(Boolean)
          .join(" · ");
      case "copy":
      case "move":
        return `${locationText(task.from)} -> ${locationText(task.to)}${
          task.overwrite ? ` · ${t("overwrite")}` : ""
        }`;
      case "delete":
        return locationText(task.target);
      case "pull":
        return `${(task.sources ?? []).length} -> ${locationText(task.to)}`;
      case "export":
        return `${(task.sources ?? []).length} -> ${task.outputName ?? "export.zip"}`;
      default:
        return task.clean ? t("pushClean") : "";
    }
  }

  function stateLabel(state: JobState): string {
    switch (state) {
      case "pending":
        return t("jobPending");
      case "running":
        return t("jobRunning");
      case "ok":
        return t("jobOk");
      case "failed":
        return t("jobFailed");
      default:
        return t("jobCancelled");
    }
  }

  function noticeIntent(notice: CardNotice): "warning" | "error" {
    return notice.kind === "server" ? notice.intent : "error";
  }

  function noticeText(notice: CardNotice): string {
    switch (notice.kind) {
      case "server":
        return notice.message;
      case "startFailed":
        return t("jobStartFailed", { msg: notice.message });
      default:
        return t("jobPollFailed", { msg: notice.message });
    }
  }

  /** Sentence after finishing: message + file count / bytes / artifact path / exit code, joined as needed. */
  function resultText(job: Job): string {
    const head =
      job.state === "ok"
        ? t("jobOk")
        : job.state === "cancelled"
          ? t("jobCancelled")
          : t("jobFailed");
    const parts: string[] = [];
    if (job.message) {
      parts.push(job.message);
    }
    if (job.state === "ok") {
      if (job.files > 0) {
        parts.push(t("filesCount", { n: job.files }));
      }
      if (job.bytes > 0) {
        parts.push(formatBytes(job.bytes));
      }
      if (job.artifactPath) {
        parts.push(job.artifactPath);
      }
    }
    if (job.state === "failed" && job.exitCode >= 0) {
      parts.push(t("buildExitCode", { code: job.exitCode }));
    }
    return parts.length ? `${head} ${parts.join(" · ")}` : head;
  }

  async function importProject(): Promise<void> {
    // Design doc §6.1: directory browsing is served by the daemon, because the WebUI
    // may be open on another device and the native picker would target the wrong host.
    // So open the daemon-side browser first, then register the returned path.
    setBrowseFor({ kind: "newProject" });
  }

  async function createProjectAt(path: string): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiCreateProject(clientId, path);
      await onReload();
    } catch (err) {
      setError(t("importFailed", { msg: errorText(err) }));
    } finally {
      setBusy(false);
    }
  }

  async function removeProject(project: Project): Promise<void> {
    setPendingDelete(null);
    setError(null);
    try {
      await apiDeleteProject(project.id);
      await onReload();
    } catch (err) {
      setError(t("importFailed", { msg: errorText(err) }));
    }
  }

  async function pushProject(project: Project, clean: boolean): Promise<void> {
    setPendingPush(null);
    setError(null);
    await startJob(project, "push", t("push"), () => apiStartPush(project.id, clean));
  }

  async function pullProject(project: Project): Promise<void> {
    setPendingPull(null);
    setError(null);
    await startJob(project, "pull", t("pull"), () => apiStartPull(project.id));
  }

  /** Run a task. The server writes output to this client's log stream; watch polling results and the log tab. */
  async function runTask(project: Project, task: Task): Promise<void> {
    // Design doc §9: delete and move need confirmation; copy overwriting a target too.
    if (needsConfirm(task)) {
      setPendingRun({ project, task });
      return;
    }
    await startTaskNow(project, task);
  }

  /** Actually start after the confirmation passed. */
  async function startTaskNow(project: Project, task: Task): Promise<void> {
    setPendingRun(null);
    await startJob(project, kindOf(task), task.name || taskTitle(task), () =>
      apiStartTask(project.id, task.id),
    );
  }

  /** Which tasks deserve an extra prompt (the destructive kinds). */
  function needsConfirm(task: Task): boolean {
    if (task.kind === "delete" || task.kind === "move") {
      return true;
    }
    return task.kind === "copy" && task.overwrite === true;
  }

  /** The confirm dialog must spell out exactly what will be touched. */
  function confirmRunMessage(task: Task): string {
    if (task.kind === "delete") {
      return t("confirmRunDelete", { path: locationText(task.target) });
    }
    if (task.kind === "move") {
      return t("confirmRunMove", {
        from: locationText(task.from),
        to: locationText(task.to),
      });
    }
    return t("confirmRunCopy", { to: locationText(task.to) });
  }

  async function previewTask(project: Project, task: Task): Promise<void> {
    setError(null);
    try {
      const result = await apiPreviewTask(project.id, task.id);
      setCopied(false);
      setPreview({
        title: task.name || taskTitle(task),
        command: result.command || taskDetail(task),
      });
    } catch (err) {
      setError(t("previewFailed", { msg: errorText(err) }));
    }
  }

  async function removeTask(project: Project, task: Task): Promise<void> {
    setPendingTaskDelete(null);
    setError(null);
    try {
      await apiDeleteTask(project.id, task.id);
      await onReload();
    } catch (err) {
      setError(t("saveFailed", { msg: errorText(err) }));
    }
  }

  async function runBatch(project: Project, batch: Batch, fromStep?: number): Promise<void> {
    await startJob(project, "batch", batch.name || batch.id, () =>
      apiRunBatch(project.id, batch.id, fromStep),
    );
  }

  async function enqueueTask(project: Project, task: Task): Promise<void> {
    if (needsConfirm(task)) {
      setPendingRun({ project, task });
      return;
    }
    clearNotice(project.id);
    await startJob(project, kindOf(task), task.name || taskTitle(task), () =>
      apiEnqueueTask(project.id, task.id),
    );
  }

  async function enqueueBatch(project: Project, batch: Batch, fromStep?: number): Promise<void> {
    clearNotice(project.id);
    await startJob(project, "batch", batch.name || batch.id, () =>
      apiEnqueueBatch(project.id, batch.id, fromStep),
    );
  }

  async function removeBatch(project: Project, batch: Batch): Promise<void> {
    setPendingBatchDelete(null);
    setError(null);
    try {
      await apiDeleteBatch(project.id, batch.id);
      await onReload();
    } catch (err) {
      setError(t("saveFailed", { msg: errorText(err) }));
    }
  }

  /** Retry from the failed step (design doc §7.3). */
  async function retryJob(job: Job): Promise<void> {
    const project = projects.find((item) => item.id === job.projectId);
    if (!project) {
      return;
    }
    await startJob(project, "batch", job.title || job.id, async () => {
      const result = await apiRetryJob(job.id);
      return { ok: true as const, jobId: result.jobId, project };
    });
  }

  async function copyCommand(): Promise<void> {
    if (!preview?.command) {
      return;
    }
    try {
      await navigator.clipboard.writeText(preview.command);
      setCopied(true);
    } catch {
      // Clipboard unavailable (insecure context etc.): the command is already shown, so skip.
    }
  }

  const recentMine = recent
    .filter((job) => job.clientId === clientId)
    .slice(0, RECENT_LIMIT);

  return (
    <>
      <div className={styles.row} style={{ marginBottom: 12 }}>
        <p className={styles.hint} style={{ margin: 0 }}>
          {t("projectsHint")}
        </p>
        <div className={styles.actions}>
          <Tooltip content={t("importProject")} relationship="description" withArrow>
            <Button
              appearance="subtle"
              icon={<FolderAdd20Regular />}
              aria-label={t("importProject")}
              disabled={busy}
              onClick={() => void importProject()}
            />
          </Tooltip>
        </div>
      </div>

      {error ? (
        <MessageBar intent="error" style={{ marginBottom: 12 }}>
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      ) : null}

      <div className={styles.list}>
        {mine.length === 0 ? <p className={styles.empty}>{t("emptyProjects")}</p> : null}
        {mine.map((project) => {
          const lastSync = formatDateTime(project.lastSyncAt);
          const taskList = project.tasks ?? [];
          const runningJob = runningJobOf(project.id);
          const startingJob = starting[project.id];
          const serverJob = running[project.id];
          const busyJobId = runningJob?.id ?? startingJob?.jobId ?? serverJob?.jobId ?? "";
          const projectBusy = busyJobId !== "" || startingJob !== undefined;
          const busyKind: JobKind | null =
            runningJob?.kind ?? startingJob?.kind ?? serverJob?.kind ?? null;
          const busyTitle = runningJob?.title || startingJob?.title || t("jobRunning");
          const busyElapsed = runningJob ? elapsedOf(runningJob) : serverJob?.runningSeconds ?? 0;
          const notice = notices[project.id];
          const finished = projectBusy || notice ? null : finishedJobOf(project.id);
          // §9: artifact remote path (may be multi-line; the card shows only the first)
          const artifact =
            (project.lastArtifact ?? "")
              .split("\n")
              .map((line) => line.trim())
              .find((line) => line.length > 0) ?? "";
          return (
            <Card key={project.id} className={styles.card}>
              <div className={styles.row}>
                <div className={styles.fields}>
                  <div className={styles.group}>
                    <div className={styles.label}>{t("fName")}</div>
                    <div className={styles.value}>{project.name}</div>
                  </div>
                  <div className={styles.group}>
                    <div className={styles.label}>{t("projectSource")}</div>
                    <div className={styles.value}>{project.sourcePath}</div>
                  </div>
                  <div className={styles.group}>
                    <div className={styles.label}>{t("projectRemote")}</div>
                    <div className={styles.value}>{project.remotePath}</div>
                  </div>
                </div>
                <div className={styles.actions}>
                  <Tooltip content={t("buildAdd")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Add20Regular />}
                      aria-label={t("buildAdd")}
                      disabled={busy}
                      onClick={() => setTaskDialog({ project, task: null })}
                    />
                  </Tooltip>
                  <Tooltip content={t("treeTitle")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<FolderOpen20Regular />}
                      aria-label={t("treeTitle")}
                      onClick={() => setTreeProject(project)}
                    />
                  </Tooltip>
                  <Tooltip content={t("push")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<ArrowUpload20Regular />}
                      aria-label={t("push")}
                      disabled={projectBusy}
                      onClick={() => setPendingPush(project)}
                    />
                  </Tooltip>
                  <Tooltip content={t("pull")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<ArrowDownload20Regular />}
                      aria-label={t("pull")}
                      disabled={projectBusy}
                      onClick={() => setPendingPull(project)}
                    />
                  </Tooltip>
                  <Tooltip content={t("exportTitle")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Archive20Regular />}
                      aria-label={t("exportTitle")}
                      disabled={projectBusy}
                      onClick={() => setExportProject(project)}
                    />
                  </Tooltip>
                  <Tooltip content={t("del")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Delete20Regular />}
                      aria-label={t("del")}
                      style={{ color: tokens.colorPaletteRedForeground1 }}
                      onClick={() => setPendingDelete(project)}
                    />
                  </Tooltip>
                </div>
              </div>

              <div className={styles.meta}>
                {t("projectExcludes")}: {project.excludes?.length ? project.excludes.join(", ") : "-"}
                {" · "}
                {lastSync
                  ? `${t("lastSync")} ${lastSync} ${
                      project.lastSyncOk ? t("syncOk") : t("syncFailed")
                    }`
                  : t("neverSynced")}
                {project.lastSyncMessage ? ` · ${project.lastSyncMessage}` : ""}
              </div>

              {/* Design doc §9: the callback/Export row shows "artifact remote path + build finish time" */}
              <div className={styles.meta}>
                {`${t("artifactPathLabel")}: ${artifact || "-"}`}
                {" · "}
                {project.lastBuildAt
                  ? `${t("buildFinishedAt")} ${formatDateTime(project.lastBuildAt) ?? "-"} ${
                      project.lastBuildOk ? t("jobOk") : t("jobFailed")
                    }`
                  : t("neverBuilt")}
              </div>

              {projectBusy ? (
                <div className={styles.jobBar}>
                  <Spinner size="tiny" />
                  {busyKind ? (
                    <Badge appearance="tint" color="informative" size="small">
                      {kindLabel(busyKind)}
                    </Badge>
                  ) : null}
                  <span className={styles.jobTitle}>{busyTitle}</span>
                  <span>{t("jobElapsed", { s: busyElapsed })}</span>
                  {busyJobId ? (
                    <Tooltip content={t("jobCancel")} relationship="description" withArrow>
                      <Button
                        appearance="subtle"
                        size="small"
                        icon={<DismissCircle20Regular />}
                        aria-label={t("jobCancel")}
                        disabled={cancelling === busyJobId}
                        onClick={() => setPendingCancel({ projectId: project.id, jobId: busyJobId })}
                      />
                    </Tooltip>
                  ) : null}
                </div>
              ) : null}

              {notice ? (
                <MessageBar intent={noticeIntent(notice)}>
                  <MessageBarBody>{noticeText(notice)}</MessageBarBody>
                </MessageBar>
              ) : null}

              {finished ? (
                <MessageBar intent={resultIntent(finished.state)}>
                  <MessageBarBody>
                    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: "8px" }}>
                      <div>{resultText(finished)}</div>
                      <Button
                        appearance="subtle"
                        size="small"
                        icon={<DocumentText20Regular />}
                        onClick={() => setSelectedJob(finished)}
                      >
                        {t("jobDetails")}
                      </Button>
                    </div>
                    <div className={styles.taskText}>{t("buildLogsHint")}</div>
                    {finished.steps && finished.steps.length > 1 ? (
                      <div className={styles.taskFields} style={{ marginTop: "6px" }}>
                        {finished.steps.map((step) => (
                          <span key={step.index} className={styles.taskText}>
                            {`${step.index + 1}. ${step.name || step.taskId} — ${
                              step.state
                            }${step.message ? `: ${step.message}` : ""}`}
                          </span>
                        ))}
                        {finished.retryFromStep !== null &&
                        finished.retryFromStep !== undefined &&
                        finished.state !== "running" ? (
                          <Button
                            appearance="secondary"
                            size="small"
                            icon={<ArrowClockwise20Regular />}
                            onClick={() => void retryJob(finished)}
                          >
                            {t("jobRetryFrom", { n: finished.retryFromStep + 1 })}
                          </Button>
                        ) : null}
                      </div>
                    ) : null}
                  </MessageBarBody>
                </MessageBar>
              ) : null}

              <div className={styles.buildSection}>
                <div className={styles.sectionLabel}>{t("buildTasks")}</div>

                {taskList.length === 0 ? (
                  <>
                    <p className={styles.empty}>{t("emptyBuildTasks")}</p>
                    <Button
                      appearance="primary"
                      size="small"
                      icon={<Add20Regular />}
                      onClick={() => setTaskDialog({ project, task: null })}
                    >
                      {t("buildAdd")}
                    </Button>
                  </>
                ) : (
                  taskList.map((task) => {
                    const artifact =
                      task.kind === "build"
                        ? renderArtifact(
                            task.artifactPath ||
                              manifestOf(task.manifest ?? "")?.build?.artifactPath ||
                              "",
                            task,
                            manifestOf(task.manifest ?? ""),
                          )
                        : "";
                    return (
                      <div key={task.id} className={styles.task}>
                        <div className={styles.taskFields}>
                          <span className={styles.taskName}>{task.name || task.id}</span>
                          <Badge appearance="tint" color="informative" size="small">
                            {kindLabel(task.kind)}
                          </Badge>
                          {task.kind === "build" ? (
                            <Badge appearance="tint" color="informative" size="small">
                              {task.signingMode === "unsigned"
                                ? t("signingUnsigned")
                                : t("signingSigned")}
                            </Badge>
                          ) : null}
                          <span className={styles.taskMono}>{taskDetail(task)}</span>
                          {artifact ? (
                            <span className={styles.taskText}>{artifact}</span>
                          ) : null}
                        </div>
                        <div className={styles.taskActions}>
                          <Tooltip
                            content={projectBusy ? t("jobEnqueue") : t("buildRun")}
                            relationship="description"
                            withArrow
                          >
                            <Button
                              appearance={projectBusy ? "secondary" : "primary"}
                              size="small"
                              icon={projectBusy ? <Clock20Regular /> : <Play20Regular />}
                              aria-label={projectBusy ? t("jobEnqueue") : t("buildRun")}
                              onClick={() => void (projectBusy ? enqueueTask(project, task) : runTask(project, task))}
                            />
                          </Tooltip>
                          <Tooltip content={t("buildPreview")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Code20Regular />}
                              aria-label={t("buildPreview")}
                              onClick={() => void previewTask(project, task)}
                            />
                          </Tooltip>
                          <Tooltip content={t("batchAddTask")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Layer20Regular />}
                              aria-label={t("batchAddTask")}
                              onClick={() => setAddToBatch({ project, task })}
                            />
                          </Tooltip>
                          <Tooltip content={t("buildEditJson")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Braces20Regular />}
                              aria-label={t("buildEditJson")}
                              onClick={() => setTaskJson({ project, task })}
                            />
                          </Tooltip>
                          <Tooltip content={t("buildEdit")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Edit20Regular />}
                              aria-label={t("buildEdit")}
                              onClick={() => setTaskDialog({ project, task })}
                            />
                          </Tooltip>
                          <Tooltip content={t("del")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Delete20Regular />}
                              aria-label={t("del")}
                              style={{ color: tokens.colorPaletteRedForeground1 }}
                              onClick={() => setPendingTaskDelete({ project, task })}
                            />
                          </Tooltip>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>

              <div className={styles.buildSection}>
                <div className={styles.sectionLabel}>{t("batches")}</div>
                {(project.batches ?? []).length === 0 ? (
                  <>
                    <p className={styles.empty}>{t("emptyBatches")}</p>
                    <Button
                      appearance="secondary"
                      size="small"
                      icon={<Add20Regular />}
                      onClick={() => setBatchDialog({ project, batch: null })}
                    >
                      {t("batchAdd")}
                    </Button>
                  </>
                ) : (
                  <>
                    {(project.batches ?? []).map((batch) => (
                      <div key={batch.id} className={styles.task}>
                        <div className={styles.taskFields}>
                          <span className={styles.taskName}>{batch.name || batch.id}</span>
                          <Badge appearance="tint" color="informative" size="small">
                            {t("stepCount", { n: batch.steps.length })}
                          </Badge>
                          <span className={styles.taskMono}>
                            {batch.steps
                              .map((step) => {
                                const target = taskList.find((task) => task.id === step.taskId);
                                return target ? target.name || taskTitle(target) : step.taskId;
                              })
                              .join(" → ")}
                          </span>
                        </div>
                        <div className={styles.taskActions}>
                          <Tooltip
                            content={projectBusy ? t("jobEnqueue") : t("batchRun")}
                            relationship="description"
                            withArrow
                          >
                            <Button
                              appearance={projectBusy ? "secondary" : "primary"}
                              size="small"
                              icon={projectBusy ? <Clock20Regular /> : <Play20Regular />}
                              aria-label={projectBusy ? t("jobEnqueue") : t("batchRun")}
                              onClick={() => void (projectBusy ? enqueueBatch(project, batch) : runBatch(project, batch))}
                            />
                          </Tooltip>
                          <Tooltip content={t("batchEdit")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Edit20Regular />}
                              aria-label={t("batchEdit")}
                              onClick={() => setBatchDialog({ project, batch })}
                            />
                          </Tooltip>
                          <Tooltip content={t("batchEditJson")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Braces20Regular />}
                              aria-label={t("batchEditJson")}
                              onClick={() => setBatchJson({ project, batch })}
                            />
                          </Tooltip>
                          <Tooltip content={t("del")} relationship="description" withArrow>
                            <Button
                              appearance="subtle"
                              size="small"
                              icon={<Delete20Regular />}
                              aria-label={t("del")}
                              style={{ color: tokens.colorPaletteRedForeground1 }}
                              onClick={() => setPendingBatchDelete({ project, batch })}
                            />
                          </Tooltip>
                        </div>
                      </div>
                    ))}
                    <Button
                      appearance="secondary"
                      size="small"
                      icon={<Add20Regular />}
                      onClick={() => setBatchDialog({ project, batch: null })}
                    >
                      {t("batchAdd")}
                    </Button>
                  </>
                )}
              </div>
            </Card>
          );
        })}
      </div>

      <div className={styles.recent}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}>
          <div className={styles.sectionLabel}>{t("jobRecent")}</div>
          {recentMine.some((j) => j.state !== "running") ? (
            <Tooltip content={t("jobsClear")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                size="small"
                icon={<Delete20Regular />}
                onClick={() => setClearJobsConfirm(true)}
              >
                {t("jobsClear")}
              </Button>
            </Tooltip>
          ) : null}
        </div>
        {recentMine.length === 0 ? (
          <p className={styles.empty} style={{ padding: "4px 2px" }}>
            {t("jobEmpty")}
          </p>
        ) : (
          recentMine.map((job) => (
            <div
              key={job.id}
              className={styles.recentRow}
              style={{ cursor: "pointer" }}
              onClick={() => setSelectedJob(job)}
            >
              <span className={styles.recentTitle}>{job.title || job.id}</span>
              <Badge appearance="tint" color="informative" size="small">
                {kindLabel(job.kind)}
              </Badge>
              <Badge appearance="tint" color={stateColor(job.state)} size="small">
                {stateLabel(job.state)}
              </Badge>
              <span className={styles.recentText}>{t("jobElapsed", { s: elapsedOf(job) })}</span>
              {job.message ? <span className={styles.recentText}>{job.message}</span> : null}
              {job.retryFromStep !== null && job.retryFromStep !== undefined ? (
                <Tooltip
                  content={t("jobRetryFrom", { n: job.retryFromStep + 1 })}
                  relationship="description"
                  withArrow
                >
                  <Button
                    appearance="subtle"
                    size="small"
                    icon={<ArrowClockwise20Regular />}
                    aria-label={t("jobRetry")}
                    disabled={job.state === "running"}
                    onClick={(e) => {
                      e.stopPropagation();
                      void retryJob(job);
                    }}
                  />
                </Tooltip>
              ) : null}
              <Tooltip content={t("jobDetails")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  size="small"
                  icon={<DocumentText20Regular />}
                  aria-label={t("jobDetails")}
                  onClick={(e) => {
                    e.stopPropagation();
                    setSelectedJob(job);
                  }}
                />
              </Tooltip>
            </div>
          ))
        )}
      </div>

      <ConfirmDialog
        open={clearJobsConfirm}
        title={t("jobsClear")}
        message={t("jobsClearConfirm")}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setClearJobsConfirm(false)}
        onConfirm={() => void handleClearJobs()}
      />

      <ConfirmDialog
        open={pendingBatchDelete !== null}
        title={t("batchDeleteConfirm")}
        message={pendingBatchDelete?.batch.name}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingBatchDelete(null)}
        onConfirm={() => {
          if (pendingBatchDelete) {
            void removeBatch(pendingBatchDelete.project, pendingBatchDelete.batch);
          }
        }}
      />

      {batchDialog ? (
        <BatchDialog
          open
          status={status}
          projectId={batchDialog.project.id}
          tasks={batchDialog.project.tasks ?? []}
          batch={batchDialog.batch}
          onClose={() => setBatchDialog(null)}
          onSaved={onReload}
        />
      ) : null}

      <ConfirmDialog
        open={pendingRun !== null}
        title={t("confirmRunTitle")}
        message={pendingRun ? confirmRunMessage(pendingRun.task) : undefined}
        confirmLabel={t("buildRun")}
        cancelLabel={t("cancel")}
        danger={pendingRun?.task.kind === "delete" || pendingRun?.task.kind === "move"}
        onCancel={() => setPendingRun(null)}
        onConfirm={() => {
          if (pendingRun) {
            void startTaskNow(pendingRun.project, pendingRun.task);
          }
        }}
      />

      {addToBatch ? (
        <AddToBatchDialog
          open
          status={status}
          project={addToBatch.project}
          task={addToBatch.task}
          onClose={() => setAddToBatch(null)}
          onSaved={onReload}
        />
      ) : null}

      {treeProject ? (
        <TreeDialog
          open
          status={status}
          projectId={treeProject.id}
          projectName={treeProject.name}
          onClose={() => setTreeProject(null)}
          onCreateTask={(side, path) => {
            // Design doc §9: a path selected in the file tree becomes the file task's starting path
            setTaskDialog({
              project: treeProject,
              task: {
                id: "",
                kind: "copy",
                name: "",
                from: { side, path },
                to: { side, path: "" },
                overwrite: false,
              },
            });
          }}
        />
      ) : null}

      <FolderPickerDialog
        open={browseFor !== null}
        status={status}
        onPick={(path) => void createProjectAt(path)}
        onClose={() => setBrowseFor(null)}
      />

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t("delProjectConfirm")}
        message={pendingDelete?.name}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingDelete(null)}
        onConfirm={() => {
          if (pendingDelete) {
            void removeProject(pendingDelete);
          }
        }}
      />

      <ConfirmDialog
        open={pendingPush !== null}
        title={t("pushTitle")}
        message={t("pushConfirm")}
        confirmLabel={t("push")}
        cancelLabel={t("cancel")}
        extra={(state) => <CleanCheckbox label={t("pushClean")} state={state} />}
        onCancel={() => setPendingPush(null)}
        onConfirm={(state) => {
          if (pendingPush) {
            void pushProject(pendingPush, state.clean);
          }
        }}
      />

      <ConfirmDialog
        open={pendingPull !== null}
        title={t("pull")}
        message={t("pullConfirm")}
        confirmLabel={t("pull")}
        cancelLabel={t("cancel")}
        onCancel={() => setPendingPull(null)}
        onConfirm={() => {
          if (pendingPull) {
            void pullProject(pendingPull);
          }
        }}
      />

      {/* Design doc §9: the "edit JSON params" entry for tasks / batch workflows */}
      <BatchJsonDialog
        open={batchJson !== null}
        project={batchJson?.project ?? null}
        batch={batchJson?.batch ?? null}
        status={status}
        onClose={() => setBatchJson(null)}
        onSaved={() => void onReload()}
      />

      <TaskJsonDialog
        open={taskJson !== null}
        project={taskJson?.project ?? null}
        task={taskJson?.task ?? null}
        status={status}
        onClose={() => setTaskJson(null)}
        onSaved={() => void onReload()}
      />

      {/* Design doc §9: Export shortcut task + archive download */}
      <ExportDialog
        open={exportProject !== null}
        project={exportProject}
        status={status}
        onClose={() => setExportProject(null)}
        onConfirm={(params) => {
          const project = exportProject;
          setExportProject(null);
          if (project) {
            void startJob(project, "export", t("exportTitle"), () =>
              apiExportProject(project.id, params),
            );
          }
        }}
      />

      <ConfirmDialog
        open={pendingCancel !== null}
        title={t("jobCancel")}
        message={t("jobCancelConfirm")}
        confirmLabel={t("jobCancel")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingCancel(null)}
        onConfirm={() => {
          if (pendingCancel) {
            void cancelJob(pendingCancel.projectId, pendingCancel.jobId);
          }
        }}
      />

      <ConfirmDialog
        open={pendingTaskDelete !== null}
        title={t("buildDeleteConfirm")}
        message={pendingTaskDelete?.task.name}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingTaskDelete(null)}
        onConfirm={() => {
          if (pendingTaskDelete) {
            void removeTask(pendingTaskDelete.project, pendingTaskDelete.task);
          }
        }}
      />

      {/* Add / edit task */}
      {taskDialog ? (
        <TaskDialog
          open
          status={status}
          clientId={clientId}
          projectId={taskDialog.project.id}
          projectName={taskDialog.project.name}
          task={taskDialog.task}
          onClose={() => setTaskDialog(null)}
          onSaved={onReload}
        />
      ) : null}

      {/* Preview command */}
      <Dialog
        open={preview !== null}
        modalType="modal"
        onOpenChange={(_, data) => {
          if (!data.open) {
            setPreview(null);
          }
        }}
      >
        <DialogSurface className={styles.dialogSurface}>
          <DialogBody>
            <DialogTitle>{t("buildPreviewTitle")}</DialogTitle>
            <DialogContent className={styles.dialogBody}>
              <div className={styles.taskText}>{preview?.title ?? ""}</div>
              <pre className={styles.command}>{preview?.command ?? ""}</pre>
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                icon={<Copy20Regular />}
                onClick={() => void copyCommand()}
              >
                {copied ? t("copied") : t("copy")}
              </Button>
              <Button appearance="primary" onClick={() => setPreview(null)}>
                {t("ok")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      <JobDetailDialog
        open={selectedJob !== null}
        job={selectedJob}
        language={status.language}
        onClose={() => setSelectedJob(null)}
        onRetry={(j) => void retryJob(j)}
      />
    </>
  );
}





