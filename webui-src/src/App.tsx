import type { JSX } from "react";
import { FluentProvider, webDarkTheme } from "@fluentui/react-components";
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronRight,
  Copy,
  FileCode2,
  Globe,
  LayoutDashboard,
  LogOut,
  RotateCw,
  Server,
  Settings,
  Terminal,
  Workflow,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  ApiError,
  apiGetConfig,
  apiGetStatus,
  apiListClients,
  apiListProjects,
  apiLogout,
  apiSetConfig,
  type ClientInfo,
  type Config,
  type Project,
  type RunningJob,
  type Status,
} from "./api";
import { makeT, normalizeLanguage } from "./i18n/strings";
import { CenteredSpinner } from "./components/Common";
import { LoginPage } from "./screens/LoginPage";
import { StatusPage } from "./screens/StatusPage";
import { SettingsPage } from "./screens/SettingsPage";
import { ClientsPage } from "./screens/ClientsPage";
import { ManifestsPage } from "./screens/ManifestsPage";
import { WorkflowsPage } from "./screens/WorkflowsPage";
import { InstancePage, type InstanceTab } from "./screens/InstancePage";

type Page = "status" | "config" | "clients" | "manifests" | "workflows" | "instance";
type Phase = "booting" | "login" | "ready" | "error";

export function App(): JSX.Element {
  const [phase, setPhase] = useState<Phase>("booting");
  const [fatal, setFatal] = useState<string | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [config, setConfig] = useState<Config | null>(null);
  const [clients, setClients] = useState<ClientInfo[]>([]);
  const [projects, setProjects] = useState<Project[]>([]);
  /** projectId -> running job (from `running` in /api/projects). */
  const [runningJobs, setRunningJobs] = useState<Record<string, RunningJob>>({});
  const [page, setPage] = useState<Page>("status");
  const [instanceId, setInstanceId] = useState<string | null>(null);
  const [instanceTab, setInstanceTab] = useState<InstanceTab>("status");
  const [copiedMcp, setCopiedMcp] = useState(false);
  const [reloading, setReloading] = useState(false);

  /** Load everything. status is required; other failures degrade (as before). */
  const loadAll = useCallback(async (): Promise<{ status: Status; config: Config | null }> => {
    const [statusResult, configResult, clientsResult, projectsResult] = await Promise.allSettled([
      apiGetStatus(),
      apiGetConfig(),
      apiListClients(),
      apiListProjects(),
    ]);

    if (statusResult.status === "rejected") {
      throw statusResult.reason;
    }
    const nextStatus = statusResult.value;
    const nextConfig = configResult.status === "fulfilled" ? configResult.value : null;
    const nextClients = clientsResult.status === "fulfilled" ? clientsResult.value.clients : [];
    const nextProjects = projectsResult.status === "fulfilled" ? projectsResult.value.projects : [];
    const nextRunning =
      projectsResult.status === "fulfilled" ? projectsResult.value.running ?? {} : {};

    setStatus(nextStatus);
    setConfig(nextConfig);
    setClients(nextClients ?? []);
    setProjects(nextProjects ?? []);
    setRunningJobs(nextRunning);
    return { status: nextStatus, config: nextConfig };
  }, []);

  // Boot: first check whether login is required.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const nextStatus = await apiGetStatus();
        if (cancelled) {
          return;
        }
        setStatus(nextStatus);
        if (nextStatus.auth.required && !nextStatus.client.authenticated) {
          setPhase("login");
          return;
        }
        const loaded = await loadAll();
        if (cancelled) {
          return;
        }
        if (!loaded.config) {
          setPage("status");
        }
        setPhase("ready");
      } catch (err) {
        if (cancelled) {
          return;
        }
        setFatal(errMessage(err));
        setPhase("error");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [loadAll]);

  // Language follows the server.
  useEffect(() => {
    if (status) {
      document.documentElement.lang = status.language;
    }
  }, [status]);

  const reload = useCallback(async (): Promise<void> => {
    setReloading(true);
    try {
      await loadAll();
    } catch (err) {
      setFatal(errMessage(err));
      setPhase("error");
    } finally {
      setTimeout(() => setReloading(false), 300);
    }
  }, [loadAll]);

  // Fall back to the list page when the instance is deleted.
  useEffect(() => {
    if (page !== "instance") {
      return;
    }
    if (instanceId && clients.some((client) => client.id === instanceId)) {
      return;
    }
    if (instanceId && clients.length === 0) {
      return;
    }
    setInstanceId(null);
    setPage("clients");
  }, [page, instanceId, clients]);

  if (phase === "booting") {
    return (
      <div className="min-h-screen bg-background flex flex-col items-center justify-center text-text">
        <div className="flex flex-col items-center gap-4">
          <div className="w-10 h-10 rounded-xl bg-primary/10 border border-primary/20 flex items-center justify-center text-primary animate-pulse">
            <Terminal className="w-5 h-5" />
          </div>
          <CenteredSpinner label="CompileGateway" />
        </div>
      </div>
    );
  }

  const language = normalizeLanguage(status?.language);

  if (phase === "error" || !status) {
    const t = makeT(language);
    return (
      <div className="min-h-screen bg-background flex flex-col items-center justify-center p-6 text-text">
        <div className="max-w-md w-full bg-card border border-danger/30 rounded-xl p-6 text-center shadow-lg">
          <AlertTriangle className="w-10 h-10 text-danger mx-auto mb-3" />
          <h2 className="text-lg font-semibold text-text mb-2">{t("loadFailed", { msg: fatal ?? "" })}</h2>
          <button
            onClick={() => void reload()}
            className="mt-4 px-4 py-2 bg-primary hover:bg-primary-hover text-white rounded-lg text-sm font-medium transition-colors"
          >
            {t("refresh")}
          </button>
        </div>
      </div>
    );
  }

  if (phase === "login") {
    return (
      <FluentProvider theme={webDarkTheme}>
        <LoginPage
          status={status}
          onAuthenticated={async () => {
            const loaded = await loadAll();
            if (!loaded.config) {
              setPage("status");
            }
            setPhase("ready");
          }}
        />
      </FluentProvider>
    );
  }

  const t = makeT(language);
  const instanceClient = clients.find((client) => client.id === instanceId) ?? null;
  const activeNav = page === "instance" ? "clients" : page;
  const runningJobsCount = Object.keys(runningJobs).length;

  async function signOut(): Promise<void> {
    try {
      await apiLogout();
    } catch {
      // Go back to the login gate even if the request failed.
    }
    setPhase("booting");
    try {
      const nextStatus = await apiGetStatus();
      setStatus(nextStatus);
      setPhase(
        nextStatus.auth.required && !nextStatus.client.authenticated ? "login" : "ready",
      );
      if (!(nextStatus.auth.required && !nextStatus.client.authenticated)) {
        await reload();
      }
    } catch (err) {
      setFatal(errMessage(err));
      setPhase("error");
    }
  }

  async function toggleLanguage(): Promise<void> {
    if (!status) return;
    const nextLang = status.language === "zh-Hans" ? "en-US" : "zh-Hans";
    try {
      await apiSetConfig({ language: nextLang });
      await reload();
    } catch (err) {
      console.error("Failed to switch language:", err);
    }
  }

  const navItems = [
    {
      id: "status" as const,
      label: t("navStatus"),
      icon: LayoutDashboard,
      badge: null,
    },
    {
      id: "clients" as const,
      label: t("navClients"),
      icon: Server,
      badge: clients.length > 0 ? clients.length : null,
    },
    {
      id: "workflows" as const,
      label: t("navWorkflows"),
      icon: Workflow,
      badge: null,
    },
    {
      id: "manifests" as const,
      label: t("navManifests"),
      icon: FileCode2,
      badge: null,
    },
    {
      id: "config" as const,
      label: t("navConfig"),
      icon: Settings,
      badge: null,
    },
  ];

  return (
    <div className="min-h-screen bg-background text-text flex flex-col md:flex-row antialiased selection:bg-primary/20 selection:text-primary">
      {/* ------------------------------------------------------------- Desktop Sidebar (Option A) */}
      <aside className="hidden md:flex flex-col w-64 border-r border-border bg-surface shrink-0 h-screen sticky top-0 select-none">
        {/* Brand Header */}
        <div className="p-4 border-b border-border/80 flex items-center justify-between">
          <div className="flex items-center gap-2.5">
            <div className="w-8 h-8 rounded-lg bg-primary/10 border border-primary/20 flex items-center justify-center text-primary shadow-inner">
              <Terminal className="w-4 h-4" />
            </div>
            <div>
              <span className="font-bold text-sm text-text tracking-tight block">CompileGateway</span>
              <span className="text-[10px] font-mono text-text-muted leading-none">v{status.version}</span>
            </div>
          </div>
          <div className="flex items-center gap-1.5 px-2 py-0.5 rounded-full bg-emerald-500/10 border border-emerald-500/20 text-emerald-400 text-[11px] font-mono font-medium">
            <span className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse" />
            <span>:{status.port}</span>
          </div>
        </div>

        {/* Daemon Connection Mini-Bar */}
        <div className="px-4 py-2 bg-background/50 border-b border-border/60 flex items-center justify-between text-[11px] text-text-muted">
          <span className="truncate">{status.url}</span>
          <span className="uppercase font-mono text-[10px] px-1 bg-card border border-border rounded">
            {status.listen}
          </span>
        </div>

        {/* Navigation Items */}
        <nav className="flex-1 px-3 py-4 space-y-1 overflow-y-auto">
          {navItems.map((item) => {
            const Icon = item.icon;
            const active = activeNav === item.id;
            return (
              <button
                key={item.id}
                onClick={() => {
                  setInstanceId(null);
                  setPage(item.id);
                }}
                className={`w-full flex items-center justify-between px-3 py-2 rounded-lg text-sm font-medium transition-all group ${
                  active
                    ? "bg-primary/10 text-primary border border-primary/20 shadow-sm"
                    : "text-text-secondary hover:text-text hover:bg-card/70 border border-transparent"
                }`}
              >
                <div className="flex items-center gap-2.5">
                  <Icon
                    className={`w-4 h-4 transition-colors ${
                      active ? "text-primary" : "text-text-muted group-hover:text-text-secondary"
                    }`}
                  />
                  <span>{item.label}</span>
                </div>
                {item.badge !== null ? (
                  <span
                    className={`text-[10px] font-mono px-1.5 py-0.5 rounded-md ${
                      active
                        ? "bg-primary/20 text-primary font-semibold"
                        : "bg-card border border-border text-text-muted"
                    }`}
                  >
                    {item.badge}
                  </span>
                ) : null}
              </button>
            );
          })}
        </nav>

        {/* Active Jobs Monitor Widget in Sidebar */}
        {runningJobsCount > 0 ? (
          <div className="mx-3 mb-3 p-2.5 rounded-lg bg-card border border-primary/30 shadow-sm">
            <div className="flex items-center justify-between text-xs text-primary font-medium mb-1">
              <span className="flex items-center gap-1.5">
                <span className="w-2 h-2 rounded-full bg-primary animate-ping" />
                <span>{t("jobRunning")}</span>
              </span>
              <span className="font-mono text-[11px]">{runningJobsCount}</span>
            </div>
            <p className="text-[11px] text-text-muted truncate">
              {Object.values(runningJobs)[0]?.jobId}
            </p>
          </div>
        ) : null}

        {/* Sidebar Footer */}
        <div className="p-3 border-t border-border/80 space-y-2 bg-surface/50">
          {/* MCP Server Pill */}
          <div className="flex items-center justify-between px-2.5 py-1.5 rounded-lg bg-card/80 border border-border text-xs text-text-muted">
            <div className="flex items-center gap-2 min-w-0">
              <Bot className="w-3.5 h-3.5 text-primary shrink-0" />
              <span className="truncate">MCP: {status.mcp.running ? "On" : "Off"}</span>
              <span className="text-[10px] font-mono text-text-muted">({status.mcp.tools})</span>
            </div>
            {status.mcp.running ? (
              <button
                title={t("mcpCopy")}
                onClick={() => {
                  void navigator.clipboard.writeText(status.mcp.endpoint);
                  setCopiedMcp(true);
                  setTimeout(() => setCopiedMcp(false), 2000);
                }}
                className="text-text-muted hover:text-text p-1 transition-colors"
              >
                {copiedMcp ? <Check className="w-3.5 h-3.5 text-emerald-400" /> : <Copy className="w-3.5 h-3.5" />}
              </button>
            ) : null}
          </div>

          {/* Quick Actions (Language + Signout) */}
          <div className="flex items-center justify-between pt-1">
            <button
              onClick={() => void toggleLanguage()}
              className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-text-secondary hover:text-text hover:bg-card rounded-md transition-colors"
            >
              <Globe className="w-3.5 h-3.5" />
              <span>{status.language === "zh-Hans" ? "中文" : "English"}</span>
            </button>

            {!status.client.loopback && status.client.authenticated ? (
              <button
                onClick={() => void signOut()}
                className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-danger hover:bg-danger/10 rounded-md transition-colors"
              >
                <LogOut className="w-3.5 h-3.5" />
                <span>{t("logout")}</span>
              </button>
            ) : null}
          </div>
        </div>
      </aside>

      {/* ------------------------------------------------------------- Mobile Top Bar (< md) */}
      <header className="md:hidden sticky top-0 z-30 h-14 bg-surface/90 backdrop-blur border-b border-border flex items-center justify-between px-4">
        <div className="flex items-center gap-2">
          <div className="w-7 h-7 rounded-lg bg-primary/10 border border-primary/20 flex items-center justify-center text-primary">
            <Terminal className="w-4 h-4" />
          </div>
          <span className="font-bold text-sm tracking-tight text-text">CompileGateway</span>
          <span className="w-2 h-2 rounded-full bg-emerald-500 animate-pulse ml-1" />
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void toggleLanguage()}
            className="px-2 py-1 text-xs text-text-secondary border border-border rounded-md bg-card"
          >
            {status.language === "zh-Hans" ? "中" : "EN"}
          </button>
          {!status.client.loopback && status.client.authenticated ? (
            <button
              onClick={() => void signOut()}
              className="p-1.5 text-danger border border-border rounded-md bg-card"
            >
              <LogOut className="w-4 h-4" />
            </button>
          ) : null}
        </div>
      </header>

      {/* ------------------------------------------------------------- Main Content Viewport */}
      <div className="flex-1 flex flex-col min-w-0 min-h-[100dvh] pb-24 md:pb-6">
        {/* Warning Banner if WAN without Auth */}
        {status.warning === "wanWithoutAuth" ? (
          <div className="bg-amber-500/10 border-b border-amber-500/20 px-4 py-2.5 text-xs text-amber-300 flex items-center gap-2">
            <AlertTriangle className="w-4 h-4 shrink-0 text-amber-400" />
            <span>{t("warningWan")}</span>
          </div>
        ) : null}

        {/* Top Header & Breadcrumbs Bar */}
        <header className="h-14 border-b border-border/80 px-4 lg:px-8 flex items-center justify-between shrink-0 bg-surface/30 backdrop-blur">
          {/* Breadcrumbs */}
          <div className="flex items-center gap-2 text-xs md:text-sm text-text-secondary min-w-0 overflow-hidden">
            {page === "instance" && instanceClient ? (
              <>
                <button
                  onClick={() => {
                    setInstanceId(null);
                    setPage("clients");
                  }}
                  className="hover:text-text transition-colors truncate"
                >
                  {t("navClients")}
                </button>
                <ChevronRight className="w-3.5 h-3.5 text-text-muted shrink-0" />
                <span className="font-medium text-text truncate">
                  {instanceClient.name || instanceClient.host}
                </span>
                <ChevronRight className="w-3.5 h-3.5 text-text-muted shrink-0" />
                <span className="text-text-muted uppercase text-xs font-mono">
                  {instanceTab}
                </span>
              </>
            ) : (
              <>
                <span className="text-text-muted hidden sm:inline">CompileGateway</span>
                <ChevronRight className="w-3.5 h-3.5 text-text-muted shrink-0 hidden sm:inline" />
                <span className="font-medium text-text">
                  {navItems.find((n) => n.id === page)?.label ?? page}
                </span>
              </>
            )}
          </div>

          {/* Quick Header Actions */}
          <div className="flex items-center gap-3 shrink-0">
            <div className="hidden lg:flex items-center gap-2 text-xs text-text-muted font-mono">
              <span className="px-2 py-0.5 rounded bg-card border border-border">
                {status.client.loopback ? "Local" : "Remote"}
              </span>
            </div>
            <button
              title={t("refresh")}
              onClick={() => void reload()}
              disabled={reloading}
              className="p-1.5 text-text-secondary hover:text-text hover:bg-card border border-border/60 rounded-lg transition-colors"
            >
              <RotateCw className={`w-3.5 h-3.5 ${reloading ? "animate-spin" : ""}`} />
            </button>
          </div>
        </header>

        {/* Page Content with Scoped FluentProvider */}
        <main className="flex-1 w-full max-w-7xl mx-auto p-4 sm:p-6 lg:p-8">
          <FluentProvider theme={webDarkTheme} className="bg-transparent text-text w-full">
            {page === "status" ? <StatusPage status={status} /> : null}
            {page === "config" && config ? (
              <SettingsPage status={status} config={config} onSaved={reload} />
            ) : null}
            {page === "config" && !config ? (
              <div className="p-4 bg-danger/10 border border-danger/30 rounded-xl text-danger text-sm">
                {t("loadFailed", { msg: "/api/config" })}
              </div>
            ) : null}
            {page === "clients" ? (
              <ClientsPage
                status={status}
                clients={clients}
                onReload={reload}
                onOpenInstance={(clientId) => {
                  setInstanceId(clientId);
                  setInstanceTab("status");
                  setPage("instance");
                }}
              />
            ) : null}
            {page === "manifests" ? <ManifestsPage status={status} /> : null}
            {page === "workflows" ? (
              <WorkflowsPage
                status={status}
                projects={projects}
                onOpenProject={(projectId) => {
                  const targetProject = projects.find((p) => p.id === projectId);
                  if (targetProject) {
                    setInstanceId(targetProject.clientId);
                    setInstanceTab("projects");
                    setPage("instance");
                  }
                }}
              />
            ) : null}
            {page === "instance" && instanceClient ? (
              <InstancePage
                client={instanceClient}
                clients={clients}
                projects={projects}
                running={runningJobs}
                status={status}
                tab={instanceTab}
                onSelectTab={setInstanceTab}
                onSelectClient={(clientId) => {
                  setInstanceId(clientId);
                  setInstanceTab("status");
                }}
                onBack={() => {
                  setInstanceId(null);
                  setPage("clients");
                }}
                onReload={reload}
              />
            ) : null}
          </FluentProvider>
        </main>
      </div>

      {/* ------------------------------------------------------------- Mobile Bottom Navigation Bar (< md) */}
      <nav className="md:hidden fixed bottom-0 left-0 right-0 z-40 bg-surface/95 backdrop-blur-md border-t border-border flex items-center justify-around h-16 pb-[env(safe-area-inset-bottom)] px-1 select-none">
        {navItems.map((item) => {
          const Icon = item.icon;
          const active = activeNav === item.id;
          return (
            <button
              key={item.id}
              onClick={() => {
                setInstanceId(null);
                setPage(item.id);
              }}
              className={`flex-1 flex flex-col items-center justify-center py-1 transition-all ${
                active ? "text-primary font-medium" : "text-text-muted hover:text-text-secondary"
              }`}
            >
              <div className="relative">
                <Icon className={`w-5 h-5 ${active ? "text-primary" : "text-text-muted"}`} />
                {item.badge !== null ? (
                  <span className="absolute -top-1 -right-2 w-3.5 h-3.5 rounded-full bg-primary text-white text-[9px] font-mono flex items-center justify-center">
                    {item.badge}
                  </span>
                ) : null}
              </div>
              <span className="text-[10px] mt-1 tracking-tight">{item.label}</span>
            </button>
          );
        })}
      </nav>
    </div>
  );
}

function errMessage(err: unknown): string {
  if (err instanceof ApiError) {
    return err.message;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}
