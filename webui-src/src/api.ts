/**
 * Typed wrapper over the daemon HTTP API: every call is same-origin `/api/*` with cookie
 * credentials (§4.1), so fetch always sends `credentials: "same-origin"`. Server error bodies
 * (`{ error: { code, message, ... } }`) are folded into [`ApiError`] here; pages read `err.message`.
 */

export type Language = "zh-Hans" | "en-US";
export type LanguageSetting = "auto" | Language;
export type Listen = "lan" | "wan";
export type AuthMethod = "password" | "key";
export type LogLevel = "info" | "warn" | "error" | "input" | "out";

export interface StatusAuth {
  /** Whether a username + password have already been set. */
  required: boolean;
  username: string;
}

export interface StatusClient {
  /** Whether the request came from 127.0.0.1 (credentials change locally only, §4.4). */
  loopback: boolean;
  authenticated: boolean;
}

export interface Status {
  app: string;
  version: string;
  hello: string;
  pageNote: string;
  port: number;
  listen: Listen;
  url: string;
  language: Language;
  /** Windows accent color, used by the UI as the pseudo-Mica base. */
  accent: string;
  configPath: string;
  auth: StatusAuth;
  client: StatusClient;
  /** `"wanWithoutAuth"` or null. */
  warning: string | null;
  /** Built-in MCP adapter status (§12 Phase 6). */
  mcp: McpStatus;
  /** Warning when MCP listens on wan without credentials: `"mcpWanWithoutAuth"` or null. */
  mcpWarning: string | null;
  restartNote: {
    port: number;
    listen: Listen;
  };
}

/**
 * Status + config of the MCP adapter: it runs **inside the daemon process on a separate port**,
 * because the WebUI port is for humans while the MCP port is for AI agents (often on another
 * machine), so the two listen scopes may differ.
 */
export interface McpStatus {
  /** Enabled in config. */
  enabled: boolean;
  /** Actually listening. */
  running: boolean;
  /** Listening port (the one actually picked when the port is auto-selected). */
  port: number;
  listen: Listen;
  /** Like `http://127.0.0.1:3994`; empty string when not running. */
  url: string;
  /** Address clients should use: `http://127.0.0.1:3994/mcp`. */
  endpoint: string;
  /** Reason the last start failed (port already in use, etc.). */
  error: string | null;
  /** Not an error, but worth mentioning (e.g. the default port was busy and gave way). */
  note?: string | null;
  /** Port tried first when `mcp.port` is empty (separate constant, unrelated to the daemon port). */
  defaultPort?: number;
  tools: number;
  protocolVersions: string[];
  /** `"mcpWanWithoutAuth"` or null. */
  warning?: string | null;
}

export interface Config {
  /** 0 = auto-select port. */
  port: number;
  language: LanguageSetting;
  listen: Listen;
  username: string;
  hasPassword: boolean;
  /** Minimum seconds between two login attempts from the same source IP; 0 = no throttling. */
  loginIntervalSeconds: number;
  /** Log retention in days (§10). */
  logRetainDays: number;
  /** Built-in MCP adapter (for AI agents, on a separate port). */
  mcp: McpStatus;
  /** Extra host dirs allowed in config (shared by browse / project registration / detection). */
  allowedRoots: string[];
  /** Host-level exclusion: one job at a time per client (§7.6.6 end), effective immediately. */
  hostLock: boolean;
  /** Effective host dirs = user home + allowedRoots. */
  hostRoots: string[];
  /** Log directory (read-only display). */
  logDir: string;
  configPath: string;
}

export interface ConfigPatch {
  /** All optional: **only the given fields change**, the rest keep their values (same rule in the daemon). */
  port?: number;
  language?: LanguageSetting;
  listen?: Listen;
  username?: string;
  /** Omitted = leave unchanged; "" = clear. */
  password?: string;
  loginIntervalSeconds?: number;
  logRetainDays?: number;
  /** Extra host dirs allowed for operations; effective immediately. */
  allowedRoots?: string[];
  /** Host-level exclusion (§7.6.6 end); effective immediately. */
  hostLock?: boolean;
  /** Only the fields to change; they take effect **immediately**, no restart (unlike the daemon port). */
  mcp?: { enabled?: boolean; listen?: Listen; port?: number };
}

export interface ConfigResult {
  ok: true;
  restartRequired: boolean;
  config: Config;
}

export interface ClientAuthInfo {
  method: AuthMethod;
  username: string;
  keyPath: string;
  hasPassword: boolean;
  /** Whether the private key has a passphrase (the passphrase itself is never sent back). */
  hasPassphrase: boolean;
}

export interface ClientInfo {
  id: string;
  name: string;
  host: string;
  port: number;
  auth: ClientAuthInfo;
  tags: string[];
  baseDir: string;
  baseDirResolved: string;
  hostKey: string;
  /** Whether the raw public key line is kept — a fingerprint alone cannot export standard known_hosts. */
  hasKeyLine?: boolean;
  /** Host-level mutual exclusion lock switch for this specific client machine. */
  hostLock?: boolean;
}

export interface ClientAuthInput {
  method: AuthMethod;
  username: string;
  keyPath: string;
  /** Omitted = keep the saved password; "" = clear. */
  password?: string;
  /** Private key passphrase; same semantics as `password`. */
  passphrase?: string;
}

export interface ClientInput {
  /** "" = create new. */
  id: string;
  name: string;
  host: string;
  port: number;
  auth: ClientAuthInput;
  tags: string[];
  baseDir: string;
  hostLock?: boolean;
}

export type SigningMode = "signed" | "unsigned";

/** Task kind (§7.1). */
export type TaskKind = "build" | "push" | "pull" | "copy" | "move" | "delete" | "export";

/** `side` permits only controlled locations: `remote` = project remote root, `host` = daemon host dirs. */
export type PathSide = "remote" | "host";

export interface Location {
  side: PathSide;
  path: string;
}

/**
 * One trigger task in a project. All kinds share this structure and use only some of the fields;
 * the server returns only the fields relevant to the kind, so the defaults may be omitted.
 */
export interface Task {
  /** "" = create new (the server assigns the id on submit). */
  id: string;
  kind: TaskKind;
  name: string;
  // build
  manifest?: string;
  variables?: Record<string, string>;
  signingMode?: SigningMode;
  /** A single manual artifact path; when `artifacts` is non-empty, `artifacts` wins. */
  artifactPath?: string;
  /** Manual artifact path list (§7.1). */
  artifacts?: string[];
  /** Which Xcode this build uses (`.app` path); empty = the target's global `xcode-select` (§8). */
  xcodePath?: string;
  // push
  clean?: boolean;
  // copy / move
  from?: Location;
  to?: Location;
  overwrite?: boolean;
  // delete
  target?: Location;
  // pull / export
  sources?: Location[];
  outputName?: string;
}

/** Legacy alias: a build task is a Task with kind === "build". */
export type BuildTask = Task & { kind: "build" };

/** One step of a batch workflow: references a project task by id (§7.3). */
export interface BatchStep {
  taskId: string;
  variables?: Record<string, string>;
}

export interface Batch {
  /** "" = create new. */
  id: string;
  name: string;
  steps: BatchStep[];
}

export interface Project {
  id: string;
  name: string;
  clientId: string;
  sourcePath: string;
  remotePath: string;
  excludes: string[];
  addedAt: number;
  lastSyncAt: number;
  lastSyncOk: boolean;
  lastSyncMessage: string;
  tasks: Task[];
  batches: Batch[];
  lastBuildAt: number;
  lastBuildOk: boolean;
  lastBuildMessage: string;
  lastArtifact: string;
}

export type JobKind = TaskKind | "batch";
export type JobState = "pending" | "running" | "ok" | "failed" | "cancelled";
export type JobStepState = "pending" | "running" | "ok" | "failed" | "cancelled" | "skipped";

/** Execution record of one batch step (§7.3). */
export interface JobStep {
  /** Step index within the batch (0-based; stays aligned with the original batch on a mid-retry). */
  index: number;
  taskId: string;
  name: string;
  kind: string;
  state: JobStepState;
  startedAt: number;
  finishedAt: number;
  elapsedSeconds: number;
  message: string;
  inputs?: string[];
  outputs?: string[];
  exitCode?: number;
  code?: string;
}

/**
 * One asynchronous run (build / push / pull / file operation / batch). The start endpoints return
 * a jobId at once; the actual progress and result come from polling /api/jobs/{id}.
 */
export interface Job {
  id: string;
  projectId: string;
  clientId: string;
  kind: JobKind;
  /** Only present on batch jobs. */
  batchId: string | null;
  /** Task name / batch name. */
  title: string;
  state: JobState;
  /** epoch ms. */
  startedAt: number;
  /** 0 = not finished yet. */
  finishedAt: number;
  elapsedSeconds: number;
  message: string;
  /** -1 = not applicable. */
  exitCode: number;
  artifactPath: string;
  artifacts: string[];
  files: number;
  bytes: number;
  steps: JobStep[];
  inputs?: string[];
  stepOutputs?: Record<string, string>;
  /** Index of the first step that did not succeed; null = no retryable step. */
  retryFromStep: number | null;
  cancelRequested: boolean;
}

/** Start-endpoint response: the job is queued, poll for the result yourself. */
export interface JobStartResponse {
  ok: true;
  jobId: string;
  project: Project;
  queued?: boolean;
  queuePosition?: number;
}

export interface JobsResponse {
  jobs: Job[];
}

/** Value of the `running` map in /api/projects (source of the busy info in §7.5). */
export interface RunningJob {
  jobId: string;
  kind: JobKind;
  runningSeconds: number;
}

export interface ManifestVariable {
  name: string;
  label: string;
  required: boolean;
  default: string | null;
}

export interface ManifestSummary {
  /** File name without the extension. */
  id: string;
  displayName: string;
  platform: string;
  include: {
    paths: string[];
  };
  variables: ManifestVariable[];
  build: {
    stepCount: number;
    artifactPath: string;
    artifacts?: string[];
  };
  cache: {
    paths: string[];
  };
  /** Whether this file is one of the templates shipped with the app (restorable if deleted). */
  template: boolean;
  /** `shipped` = bundled with the app, `yours` = created by you. */
  source?: "shipped" | "yours";
  valid: boolean;
  errors: string[];
}

export interface ManifestsResponse {
  manifests: ManifestSummary[];
  dir: string;
}

export interface ManifestPreset {
  id: string;
  displayName: string;
}

export interface ManifestPresetsResponse {
  missing: ManifestPreset[];
  dir?: string;
}

/** Response of `POST /api/manifests/reload`: a **reload event**. */
export interface ManifestReloadResponse {
  ok: true;
  dir: string;
  /** The directory was regenerated this time (you deleted it, or this is the first run). */
  created: boolean;
  seeded: string[];
  added: string[];
  changed: string[];
  removed: string[];
  /**
   * Human-readable one-liner, e.g. `added a; changed b` or `no changes`.
   *
   * It follows the daemon's `language` setting, so **never** branch on its value --
   * use `added` / `changed` / `removed` (or their lengths) for logic.
   */
  summary: string;
  total: number;
  invalid: number;
  manifests: ManifestSummary[];
}

export interface ManifestRawResponse {
  id: string;
  raw: string;
  template: boolean;
}

export interface ManifestDetectMatch {
  id: string;
  displayName: string;
  matched: string[];
  missing: string[];
}

export interface ManifestDetectResponse {
  matches: ManifestDetectMatch[];
}

export interface LogEntry {
  seq: number;
  level: LogLevel;
  text: string;
  /** The server `LogEntry` has no rename_all, so this field is snake_case. */
  ts_ms: number;
  /** Source tag (§10): `webui` / `cli` / `mcp` / `daemon`, an open enum. */
  source: string;
}

/** One log record persisted to disk (§10). */
export interface StoredLogEntry {
  tsMs: number;
  level: LogLevel;
  source: string;
  scope: string;
  text: string;
}

export interface LogDays {
  ok: true;
  days: string[];
  dir: string | null;
  retainDays: number;
  /** A write to disk failed (directory not writable); the UI should tell the user. */
  failed: boolean;
}

export interface TestResult {
  ok: boolean;
  message: string;
  output: string;
  hostKey: string;
  /** Fingerprint the peer **actually presented** when it differs from the record (UI offers "trust new"). */
  presentedKey: string | null;
  /** Whether this failure was caused by a fingerprint mismatch. */
  mismatch: boolean;
}

export interface BaseDirResult {
  ok: boolean;
  message: string;
  client: ClientInfo;
}

export interface PickResult {
  path: string;
}

// ------------------------------------------------------------- Directory browse (§6.1)

export interface BrowseEntry {
  name: string;
  /** Absolute path */
  path: string;
}

export interface BrowseResult {
  ok: true;
  /** Browsable root (the daemon user's home directory) */
  root: string;
  /** Absolute path of the current directory */
  path: string;
  /** Parent directory; null when already at the root */
  parent: string | null;
  entries: BrowseEntry[];
}

export interface ClientsResponse {
  clients: ClientInfo[];
  path: string;
}

export interface ProjectsResponse {
  projects: Project[];
  path: string;
  running: Record<string, RunningJob>;
}

export interface LogsResponse {
  entries: LogEntry[];
}

// ------------------------------------------------------------- Project file tree (§7.4)

export type EntryKind = "dir" | "file" | "symlink" | "other";

export interface TreeEntry {
  name: string;
  /** Path relative to the project root — exactly what a file task's `path` expects */
  path: string;
  kind: EntryKind;
  size: number;
  /** Unix seconds; 0 when unavailable */
  modifiedAt: number;
}

export interface TreeListing {
  side: PathSide;
  /** Current directory path relative to the root ("" = root) */
  path: string;
  /** Resolved root directory */
  root: string;
  /** Parent directory; null at the root */
  parent: string | null;
  entries: TreeEntry[];
  truncated: boolean;
  message: string;
}

// ------------------------------------------------------------- Environment items (§8)

/** One probe item. */
export interface EnvItem {
  id: string;
  label: string;
  /** Groups the display by this: xcode / gradle / flutter / dart / rust / common */
  platform: string;
  /** Command actually run, so the user can re-check it on the target machine */
  command: string;
  found: boolean;
  /** Parsed version; empty string when it cannot be parsed */
  version: string;
  output: string;
}

/** One Xcode installation. */
export interface XcodeInstance {
  path: string;
  version: string;
  build: string;
  /** Value to pass to DEVELOPER_DIR at build time */
  developerDir: string;
  output: string;
}

export interface EnvDisk {
  mount: string;
  totalBytes: number;
  usedBytes: number;
  availableBytes: number;
  usedRatio: number;
}

export interface EnvGroup {
  platform: string;
  items: EnvItem[];
  /** Whether any probe in this group matched */
  available: boolean;
}

export interface EnvReport {
  clientId: string;
  clientName: string;
  ok: boolean;
  message: string;
  /** uname -s */
  os: string;
  checkedAt: number;
  elapsedMs: number;
  items: EnvItem[];
  xcodes: XcodeInstance[];
  xcodeLicense: string;
  licenseOk: boolean;
  disk: EnvDisk | null;
  groups: EnvGroup[];
}

/** Server error body: `{ error: { code, message, details?, retryAfterSeconds? } }`. */
export interface ApiErrorBody {
  code: string;
  message: string;
  details?: string[];
  retryAfterSeconds?: number;
}

/**
 * Non-2xx responses are folded into this error. `code` is the server's stable identifier (e.g.
 * `projectBusy`) that pages branch on; `message` is already a sentence in the current language.
 */
export class ApiError extends Error {
  status: number;
  code: string;
  details: string[];
  retryAfterSeconds?: number;

  constructor(status: number, body: ApiErrorBody | null, statusText?: string) {
    super(body?.message || statusText || `HTTP ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.code = body?.code ?? "";
    this.details = Array.isArray(body?.details) ? body.details : [];
    if (typeof body?.retryAfterSeconds === "number") {
      this.retryAfterSeconds = body.retryAfterSeconds;
    }
  }
}

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: "same-origin",
    ...init,
    headers: {
      "Content-Type": "application/json",
      // §10: the daemon tags the log source by this header
      "X-Client": "webui",
      ...(init?.headers ?? {}),
    },
  });

  let body: unknown = null;
  const text = await response.text();
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = null;
    }
  }

  if (!response.ok) {
    const error = (body as { error?: ApiErrorBody } | null)?.error ?? null;
    throw new ApiError(response.status, error, response.statusText);
  }
  return body as T;
}

function jsonBody(value: unknown): RequestInit {
  return { body: JSON.stringify(value) };
}

// ------------------------------------------------------------------ Status and config

export const apiGetStatus = () => api<Status>("/api/status");
export const apiGetConfig = () => api<Config>("/api/config");
export const apiSetConfig = (patch: ConfigPatch) =>
  api<ConfigResult>("/api/config", { method: "POST", ...jsonBody(patch) });

// ------------------------------------------------------------------ Login

export const apiLogin = (username: string, password: string) =>
  api<{ ok: true }>("/api/login", { method: "POST", ...jsonBody({ username, password }) });
export const apiLogout = () => api<{ ok: true }>("/api/logout", { method: "POST" });

// ------------------------------------------------------------------ Clients

export const apiListClients = () => api<ClientsResponse>("/api/clients");
export const apiSaveClient = (input: ClientInput) =>
  api<{ ok: true; client: ClientInfo }>("/api/clients", { method: "POST", ...jsonBody(input) });
export const apiDeleteClient = (id: string) =>
  api<{ ok: true }>(`/api/clients/${encodeURIComponent(id)}`, { method: "DELETE" });
export const apiTestClient = (id: string) =>
  api<TestResult>(`/api/clients/${encodeURIComponent(id)}/test`, { method: "POST" });

/**
 * Manage recorded host fingerprints (TOFU). Refusing a changed fingerprint is right, but having
 * **no way out** is not — rebuilding a macOS VM or reinstalling the OS changes the host key.
 * Passing a fingerprint trusts it; passing "" clears the record (next time = first trust).
 */
export const apiSetHostKey = (id: string, fingerprint: string) =>
  api<{ ok: true; client: ClientInfo; previous: string }>(
    `/api/clients/${encodeURIComponent(id)}/hostkey`,
    { method: "POST", ...jsonBody({ fingerprint }) },
  );
export const apiSetBaseDir = (id: string, baseDir: string, verify: boolean) =>
  api<BaseDirResult>(`/api/clients/${encodeURIComponent(id)}/basedir`, {
    method: "POST",
    ...jsonBody({ baseDir, verify }),
  });

/** Pin the key for this machine from a known_hosts file (offline pre-trust).
 *
 * `path` is a path on the **daemon machine** (which is what the WebUI file picker opens);
 * the CLI uses `raw` instead (the file is on the user's machine, read there and uploaded).
 */
export const apiImportKnownHosts = (
  id: string,
  body: { path?: string; raw?: string; force?: boolean },
) =>
  api<{
    ok: true;
    alreadyTrusted: boolean;
    line: number;
    keyType: string;
    fingerprint: string;
    keyLine: string;
    notes: string[];
    problems: string[];
    client: ClientInfo;
  }>(`/api/clients/${encodeURIComponent(id)}/hostkey/import`, {
    method: "POST",
    ...jsonBody(body),
  });

/** Export the recorded trust as a single known_hosts line. */
export const apiExportKnownHosts = (id: string) =>
  api<{
    ok: true;
    hasKeyLine: boolean;
    keyType: string;
    fingerprint: string;
    line: string;
    warning: string | null;
    client: ClientInfo;
  }>(`/api/clients/${encodeURIComponent(id)}/known-hosts`);
export const apiClientLogs = (id: string, since: number) =>
  api<LogsResponse>(`/api/clients/${encodeURIComponent(id)}/logs?since=${since}`);

/** Read the env detection cache (null if it has never been detected). */
export const apiGetEnv = (id: string) =>
  api<{ ok: true; report: EnvReport | null }>(`/api/clients/${encodeURIComponent(id)}/env`);
/** Actually connect and run detection (slow, 90 second timeout). */
export const apiDetectEnv = (id: string) =>
  api<{ ok: true; report: EnvReport }>(`/api/clients/${encodeURIComponent(id)}/env`, {
    method: "POST",
  });
/** Return the probe script without connecting to any client (`cgw env --script`). */
export const apiGetEnvScript = () => api<{ ok: true; script: string }>("/api/env/script");

// ------------------------------------------------------------------ Projects

export const apiListProjects = () => api<ProjectsResponse>("/api/projects");
export const apiCreateProject = (clientId: string, sourcePath: string, name?: string) =>
  api<{ ok: true; project: Project }>("/api/projects", {
    method: "POST",
    ...jsonBody({ clientId, sourcePath, ...(name === undefined ? {} : { name }) }),
  });
export const apiDeleteProject = (id: string) =>
  api<{ ok: true }>(`/api/projects/${encodeURIComponent(id)}`, { method: "DELETE" });
export const apiStartPush = (id: string, clean: boolean) =>
  api<JobStartResponse>(`/api/projects/${encodeURIComponent(id)}/push`, {
    method: "POST",
    ...jsonBody({ clean }),
  });
export const apiStartPull = (id: string) =>
  api<JobStartResponse>(`/api/projects/${encodeURIComponent(id)}/pull`, { method: "POST" });

/** Export shortcut (§9): zip the project into `exports/` under the host dir. */
export const apiExportProject = (
  id: string,
  body: { source?: "remote" | "host"; path?: string; name?: string },
) =>
  api<JobStartResponse & { name: string; source: { side: string; path: string } }>(
    `/api/projects/${encodeURIComponent(id)}/export`,
    { method: "POST", ...jsonBody(body) },
  );

export interface ExportItem {
  name: string;
  bytes: number;
  modifiedAt: number;
}

/** Archives already exported (§9: "provide only the packaged zip download"). */
export const apiListExports = (id: string) =>
  api<{ ok: true; dir: string; exports: ExportItem[] }>(
    `/api/projects/${encodeURIComponent(id)}/exports`,
  );

/** Download URL. Same-origin browser requests carry the session cookie, so hand it to `<a download>`. */
export const exportDownloadUrl = (id: string, name: string) =>
  `/api/projects/${encodeURIComponent(id)}/exports/${encodeURIComponent(name)}`;
/** One level of the project file tree (§7.4). A missing `path` means the root directory. */
/** Controlled read of one file (§7.4). Binaries return metadata only; over the limit is truncated. */
export const apiProjectFile = (id: string, side: PathSide, path: string) =>
  api<{
    ok: true;
    side: string;
    path: string;
    bytes: number;
    size: number;
    truncated: boolean;
    binary: boolean;
    text: string;
  }>(
    `/api/projects/${encodeURIComponent(id)}/file?side=${side}&path=${encodeURIComponent(path)}`,
  );
export const apiProjectTree = (id: string, side: PathSide, path: string) =>
  api<{ ok: true; tree: TreeListing }>(
    `/api/projects/${encodeURIComponent(id)}/tree?side=${side}&path=${encodeURIComponent(path)}`,
  );

// ------------------------------------------------------------------ Jobs

export const apiListJobs = () => api<JobsResponse>("/api/jobs");
export const apiGetJob = (id: string) => api<Job>(`/api/jobs/${encodeURIComponent(id)}`);
export const apiJobLogs = (id: string, since: number) =>
  api<LogsResponse>(`/api/jobs/${encodeURIComponent(id)}/logs?since=${since}`);
export const apiCancelJob = (id: string) =>
  api<{ ok: true; job: Job }>(`/api/jobs/${encodeURIComponent(id)}/cancel`, { method: "POST" });
export const apiClearJobs = (projectId?: string) => {
  const query = projectId && projectId.trim() ? `?projectId=${encodeURIComponent(projectId.trim())}` : "";
  return api<{ ok: true; pruned: number }>(`/api/jobs/clear${query}`, { method: "POST" });
};

// ------------------------------------------------------------------ Persisted log history

export const apiLogDays = () => api<LogDays>("/api/logs/days");
/** Read one day; an empty `source` / `scope` means no filtering. */
export const apiLogDay = (
  day: string,
  options: { limit?: number; source?: string; scope?: string } = {},
) => {
  const query: string[] = [];
  if (options.limit) {
    query.push(`limit=${options.limit}`);
  }
  if (options.source) {
    query.push(`source=${encodeURIComponent(options.source)}`);
  }
  if (options.scope) {
    query.push(`scope=${encodeURIComponent(options.scope)}`);
  }
  const suffix = query.length ? `?${query.join("&")}` : "";
  return api<{
    ok: true;
    day: string;
    total: number;
    returned: number;
    entries: StoredLogEntry[];
  }>(`/api/logs/${encodeURIComponent(day)}${suffix}`);
};

// ------------------------------------------------------------------ Native dialogs

export const apiPickFile = (title: string) =>
  api<PickResult>("/api/dialog/pick-file", { method: "POST", ...jsonBody({ title }) });
export const apiPickFolder = (title?: string) =>
  api<PickResult>("/api/dialog/pick-folder", {
    method: "POST",
    ...jsonBody(title === undefined ? {} : { title }),
  });

/**
 * List the subfolders on the daemon machine (§6.1). Used instead of a native picker because the
 * WebUI may be opened from another device (a native picker selects files on the *viewing* device),
 * and a headless daemon has no native dialogs at all.
 */
export const apiBrowse = (path?: string) =>
  api<BrowseResult>(
    path && path.trim() ? `/api/browse?path=${encodeURIComponent(path)}` : "/api/browse",
  );

// ------------------------------------------------------------------ Build type manifests

export const apiListManifests = () => api<ManifestsResponse>("/api/manifests");
export const apiManifestPresets = () =>
  api<ManifestPresetsResponse>("/api/manifests/templates");

/** Reload event: re-read the build type directory from disk, returning the diff and current list. */
export const apiReloadManifests = () =>
  api<ManifestReloadResponse>("/api/manifests/reload", { method: "POST", body: "{}" });
export const apiManifestRaw = (id: string) =>
  api<ManifestRawResponse>(`/api/manifests/${encodeURIComponent(id)}/raw`);
export const apiSaveManifest = (id: string, raw: string) =>
  api<{ ok: true; id: string }>(`/api/manifests/${encodeURIComponent(id)}`, {
    method: "PUT",
    ...jsonBody({ raw }),
  });
export const apiDeleteManifest = (id: string) =>
  api<{ ok: true }>(`/api/manifests/${encodeURIComponent(id)}`, { method: "DELETE" });
export const apiResetManifests = (ids: string[]) =>
  api<{ ok: true; restored: string[] }>("/api/manifests/reset", {
    method: "POST",
    ...jsonBody({ ids }),
  });
export const apiDetectManifests = (sourcePath: string) =>
  api<ManifestDetectResponse>("/api/manifests/detect", {
    method: "POST",
    ...jsonBody({ sourcePath }),
  });

// ------------------------------------------------------------------ Tasks and batch workflows

export const apiSaveTask = (projectId: string, task: Task) =>
  api<{ ok: true; project: Project }>(`/api/projects/${encodeURIComponent(projectId)}/tasks`, {
    method: "POST",
    ...jsonBody(task),
  });
export const apiDeleteTask = (projectId: string, taskId: string) =>
  api<{ ok: true; project: Project }>(
    `/api/projects/${encodeURIComponent(projectId)}/tasks/${encodeURIComponent(taskId)}`,
    { method: "DELETE" },
  );
export const apiPreviewTask = (projectId: string, taskId: string) =>
  api<{ kind: string; command: string; artifactPath: string; artifacts?: string[] }>(
    `/api/projects/${encodeURIComponent(projectId)}/tasks/${encodeURIComponent(taskId)}/preview`,
    { method: "POST" },
  );
/** Start the task: a jobId comes back at once, and output goes into the server's log stream. */
export const apiStartTask = (projectId: string, taskId: string) =>
  api<JobStartResponse>(
    `/api/projects/${encodeURIComponent(projectId)}/tasks/${encodeURIComponent(taskId)}/run`,
    { method: "POST" },
  );
/** Queue task execution when project is busy (§7.6.6). */
export const apiEnqueueTask = (projectId: string, taskId: string) =>
  api<JobStartResponse>(
    `/api/projects/${encodeURIComponent(projectId)}/tasks/${encodeURIComponent(taskId)}/enqueue`,
    { method: "POST" },
  );

export const apiSaveBatch = (projectId: string, batch: Batch) =>
  api<{ ok: true; project: Project }>(`/api/projects/${encodeURIComponent(projectId)}/batches`, {
    method: "POST",
    ...jsonBody(batch),
  });
export const apiDeleteBatch = (projectId: string, batchId: string) =>
  api<{ ok: true; project: Project }>(
    `/api/projects/${encodeURIComponent(projectId)}/batches/${encodeURIComponent(batchId)}`,
    { method: "DELETE" },
  );
export const apiRunBatch = (projectId: string, batchId: string, fromStep?: number) =>
  api<JobStartResponse>(
    `/api/projects/${encodeURIComponent(projectId)}/batches/${encodeURIComponent(batchId)}/run`,
    {
      method: "POST",
      ...jsonBody(fromStep === undefined ? {} : { fromStep }),
    },
  );
/** Queue batch workflow execution when project is busy (§7.6.6). */
export const apiEnqueueBatch = (projectId: string, batchId: string, fromStep?: number) =>
  api<JobStartResponse>(
    `/api/projects/${encodeURIComponent(projectId)}/batches/${encodeURIComponent(batchId)}/enqueue`,
    {
      method: "POST",
      ...jsonBody(fromStep === undefined ? {} : { fromStep }),
    },
  );
/** Retry from the failed step (§7.3). `fromStart` = explicitly run from the beginning. */
export const apiRetryJob = (jobId: string, fromStart = false, fromStep?: number) =>
  api<{ ok: true; jobId: string; retriedFrom: number }>(
    `/api/jobs/${encodeURIComponent(jobId)}/retry`,
    {
      method: "POST",
      ...jsonBody({ fromStart, ...(fromStep === undefined ? {} : { fromStep }) }),
    },
  );

// Legacy names kept to reduce caller churn
export const apiSaveBuildTask = apiSaveTask;
export const apiDeleteBuildTask = apiDeleteTask;
export const apiPreviewBuildTask = apiPreviewTask;
export const apiStartBuildTask = apiStartTask;

// ------------------------------------------------------------------ Cross-project workflow templates (§14.3)

export interface WorkflowVariable {
  name: string;
  label?: string;
  default?: string;
  required?: boolean;
  description?: string;
}

export interface WorkflowStep {
  id: string;
  task: string;
  name?: string;
  continueOnError?: boolean;
}

export interface WorkflowSummary {
  id: string;
  name: string;
  description?: string;
  batchId: string;
  variables: WorkflowVariable[];
  tasks: Task[];
  steps: WorkflowStep[];
  valid: boolean;
  errors: string[];
  template: boolean;
}

export interface WorkflowTemplateItem {
  id: string;
  name: string;
  description?: string;
  steps: number;
  variables: number;
}

export interface WorkflowApplyInput {
  overrides?: Record<string, string>;
  batchName?: string;
  overwrite?: boolean;
  run?: boolean;
}

export interface WorkflowApplyResult {
  ok: true;
  jobId?: string;
  message: string;
  project: Project;
}

export const apiListWorkflows = () =>
  api<{ ok: true; workflows: WorkflowSummary[]; dir: string; hint: string }>("/api/workflows");

export const apiWorkflowRaw = (id: string) =>
  api<{ ok: true; id: string; template: boolean; raw: string; parsed: any; errors: string[] }>(
    `/api/workflows/${encodeURIComponent(id)}/raw`
  );

export const apiSaveWorkflow = (id: string, raw: string) =>
  api<{ ok: true; workflow?: WorkflowSummary }>(`/api/workflows/${encodeURIComponent(id)}`, {
    method: "PUT",
    ...jsonBody({ raw }),
  });

export const apiDeleteWorkflow = (id: string) =>
  api<{ ok: true; id: string }>(`/api/workflows/${encodeURIComponent(id)}`, { method: "DELETE" });

export const apiWorkflowTemplates = () =>
  api<{ ok: true; missing: WorkflowTemplateItem[]; dir: string }>("/api/workflows/templates");

export const apiResetWorkflows = (ids?: string[]) =>
  api<{ ok: true; restored: string[] }>("/api/workflows/reset", {
    method: "POST",
    ...jsonBody({ ids: ids ?? [] }),
  });

export const apiApplyWorkflow = (
  projectId: string,
  presetId: string,
  input: WorkflowApplyInput
) =>
  api<WorkflowApplyResult>(
    `/api/projects/${encodeURIComponent(projectId)}/workflows/${encodeURIComponent(presetId)}/apply`,
    {
      method: "POST",
      ...jsonBody(input),
    }
  );

