import type { JSX } from "react";
import {
  Button,
  FluentProvider,
  MessageBar,
  MessageBarBody,
  MessageBarTitle,
  Tab,
  TabList,
  Tooltip,
  makeStyles,
  tokens,
  webDarkTheme,
} from "@fluentui/react-components";
import {
  BookOpen20Regular,
  BranchFork20Regular,
  Desktop20Regular,
  Server20Regular,
  Settings20Regular,
  SignOut20Regular,
  Warning20Regular,
} from "@fluentui/react-icons";
import { useCallback, useEffect, useState } from "react";
import {
  ApiError,
  apiGetConfig,
  apiGetStatus,
  apiListClients,
  apiListProjects,
  apiLogout,
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

const useStyles = makeStyles({
  root: {
    minHeight: "100vh",
    color: tokens.colorNeutralForeground1,
  },
  banner: {
    borderRadius: tokens.borderRadiusNone,
  },
  shell: {
    display: "flex",
    minHeight: "100vh",
  },
  sidebar: {
    flex: "0 0 232px",
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
    padding: "16px 12px",
    borderRight: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  brand: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    padding: "0 8px 10px",
    fontWeight: tokens.fontWeightSemibold,
  },
  logo: {
    fontFamily: tokens.fontFamilyMonospace,
    fontWeight: tokens.fontWeightBold,
    color: tokens.colorBrandForeground1,
  },
  nav: {
    display: "flex",
    flexDirection: "column",
    flex: "1 1 auto",
  },
  foot: {
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
    paddingTop: "10px",
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
  },
  meta: {
    fontSize: "11px",
    lineHeight: "1.6",
    color: tokens.colorNeutralForeground3,
    overflowWrap: "anywhere",
    padding: "0 8px",
  },
  content: {
    flex: "1 1 auto",
    minWidth: 0,
    overflowY: "auto",
    padding: "28px 32px 40px",
  },
});

export function App(): JSX.Element {
  const styles = useStyles();

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

  // Pseudo-Mica base color: system accent, dark red when unavailable
  useEffect(() => {
    const accent =
      status?.accent && /^#[0-9a-f]{6}$/i.test(status.accent) ? status.accent : "#7f1d1d";
    document.documentElement.style.setProperty("--cgw-accent", accent);
  }, [status?.accent]);

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
    try {
      await loadAll();
    } catch (err) {
      setFatal(errMessage(err));
      setPhase("error");
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
      <FluentProvider theme={webDarkTheme} className={styles.root}>
      <div className="cgw-backdrop" />
        <CenteredSpinner label="CompileGateway" />
      </FluentProvider>
    );
  }

  const language = normalizeLanguage(status?.language);

  if (phase === "error" || !status) {
    const t = makeT(language);
    return (
      <FluentProvider theme={webDarkTheme} className={styles.root}>
      <div className="cgw-backdrop" />
        <div style={{ padding: 24 }}>
          <MessageBar intent="error">
            <MessageBarBody>
              <MessageBarTitle>{t("loadFailed", { msg: fatal ?? "" })}</MessageBarTitle>
            </MessageBarBody>
          </MessageBar>
        </div>
      </FluentProvider>
    );
  }

  if (phase === "login") {
    return (
      <FluentProvider theme={webDarkTheme} className={styles.root}>
      <div className="cgw-backdrop" />
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
  const navValue = page === "instance" ? "clients" : page;

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

  return (
    <FluentProvider theme={webDarkTheme} className={styles.root}>
      <div className="cgw-backdrop" />
      {status.warning === "wanWithoutAuth" ? (
        <MessageBar className={styles.banner} intent="warning" layout="multiline">
          <MessageBarBody>
            <MessageBarTitle>
              <Warning20Regular style={{ verticalAlign: "middle", marginRight: 6 }} />
              {t("warningTitle")}
            </MessageBarTitle>
            {t("warningWan")}
          </MessageBarBody>
        </MessageBar>
      ) : null}

      <div className={`${styles.shell} cgw-layer`}>
        <aside className={`${styles.sidebar} cgw-shell-surface`}>
          <div className={styles.brand}>
            <span className={styles.logo}>&gt;_</span>
            <span>{status.app}</span>
          </div>

          <TabList
            className={styles.nav}
            vertical
            selectedValue={navValue}
            onTabSelect={(_, data) => {
              const value = data.value as Page;
              setInstanceId(null);
              setPage(value);
            }}
          >
            <Tab value="status" icon={<Desktop20Regular />}>
              {t("navStatus")}
            </Tab>
            <Tab value="config" icon={<Settings20Regular />}>
              {t("navConfig")}
            </Tab>
            <Tab value="clients" icon={<Server20Regular />}>
              {t("navClients")}
            </Tab>
            <Tab value="manifests" icon={<BookOpen20Regular />}>
              {t("navManifests")}
            </Tab>
            <Tab value="workflows" icon={<BranchFork20Regular />}>
              {t("navWorkflows")}
            </Tab>
          </TabList>

          <div className={styles.foot}>
            <div className={styles.meta}>
              {t("version")} {status.version}
              <br />
              {status.url}
            </div>
            {!status.client.loopback && status.client.authenticated ? (
              <Tooltip content={t("logout")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  icon={<SignOut20Regular />}
                  aria-label={t("logout")}
                  onClick={() => void signOut()}
                >
                  {t("logout")}
                </Button>
              </Tooltip>
            ) : null}
          </div>
        </aside>

        <main className={`${styles.content} cgw-content-surface`}>
          {page === "status" ? <StatusPage status={status} /> : null}
          {page === "config" && config ? (
            <SettingsPage status={status} config={config} onSaved={reload} />
          ) : null}
          {page === "config" && !config ? (
            <MessageBar intent="error">
              <MessageBarBody>{t("loadFailed", { msg: "/api/config" })}</MessageBarBody>
            </MessageBar>
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
        </main>
      </div>
    </FluentProvider>
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
