import type { JSX } from "react";
import {
  Button,
  MessageBar,
  MessageBarBody,
  Tab,
  TabList,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  ArrowLeft20Regular,
  PlugConnected20Regular,
} from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiSetHostKey,
  apiTestClient,
  type ClientInfo,
  type Project,
  type RunningJob,
  type Status,
  type TestResult,
} from "../api";
import { makeT } from "../i18n/strings";
import { PageHead } from "../components/Common";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { InstanceHome } from "./instance/InstanceHome";
import { ProjectsTab } from "./instance/ProjectsTab";
import { EnvTab } from "./instance/EnvTab";
import { LogsTab } from "./instance/LogsTab";

export type InstanceTab = "status" | "projects" | "env" | "logs";

const useStyles = makeStyles({
  screen: {
    maxWidth: "1080px",
  },
  strip: {
    display: "flex",
    flexWrap: "wrap",
    columnGap: "6px",
    rowGap: "6px",
    margin: "0 0 16px",
    paddingBottom: "12px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  stripButton: {
    whiteSpace: "nowrap",
  },
  subnav: {
    marginBottom: "16px",
  },
  meta: {
    margin: "2px 0 0",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  emptyClients: {
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground3,
  },
});

/**
 * Instance view: a client strip on top (switch machines without going back to the list),
 * below it the status / projects / logs tabs.
 */
export function InstancePage({
  client,
  clients,
  projects,
  running,
  status,
  tab,
  onSelectTab,
  onSelectClient,
  onBack,
  onReload,
}: {
  client: ClientInfo;
  clients: ClientInfo[];
  projects: Project[];
  /** projectId -> running job. */
  running: Record<string, RunningJob>;
  status: Status;
  tab: InstanceTab;
  onSelectTab: (tab: InstanceTab) => void;
  onSelectClient: (clientId: string) => void;
  onBack: () => void;
  onReload: () => Promise<void>;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);
  const [testing, setTesting] = useState(false);
  /** On a fingerprint mismatch during test connection, show both fingerprints and let the user decide. */
  const [mismatch, setMismatch] = useState<{ presented: string; stored: string } | null>(null);

  const [testBanner, setTestBanner] = useState<{ ok: boolean; message: string } | null>(null);

  // The last used tab is kept when switching instances (legacy behavior).
  useEffect(() => {
    setTesting(false);
    setTestBanner(null);
  }, [client.id]);

  const method = client.auth.method === "key" ? t("authKey") : t("authPassword");
  const authText = client.auth.username ? `${method} · ${client.auth.username}` : method;

  async function runTest(): Promise<void> {
    setTesting(true);
    setTestBanner(null);
    const startMs = Date.now();
    let result: TestResult | null = null;
    try {
      result = await apiTestClient(client.id);
      const elapsedMs = Date.now() - startMs;
      if (result.mismatch && result.presentedKey) {
        setMismatch({ presented: result.presentedKey, stored: client.hostKey ?? "" });
        return;
      }
      if (result.ok) {
        setTestBanner({
          ok: true,
          message: t("testConnSuccess", { ms: String(elapsedMs), msg: result.message || "OK" }),
        });
      } else {
        setTestBanner({
          ok: false,
          message: t("testConnFailed", { msg: result.message || "Failed" }),
        });
      }
    } catch (err) {
      const msg = err instanceof ApiError ? err.message : String(err);
      setTestBanner({
        ok: false,
        message: t("testConnFailed", { msg }),
      });
    } finally {
      setTesting(false);
    }
  }

  async function trustPresented(): Promise<void> {
    if (!mismatch) {
      return;
    }
    setTesting(true);
    try {
      await apiSetHostKey(client.id, mismatch.presented);
      await onReload();
    } catch {
      // Failures also go to the instance log
    } finally {
      setTesting(false);
      setMismatch(null);
    }
    onSelectTab("logs");
  }

  return (
    <div className={styles.screen}>
      <div className={styles.strip} aria-label={t("clientsTitle")}>
        {clients.map((item) => (
          <Button
            key={item.id}
            className={styles.stripButton}
            appearance={item.id === client.id ? "primary" : "subtle"}
            size="small"
            shape="circular"
            aria-current={item.id === client.id}
            onClick={() => onSelectClient(item.id)}
          >
            {item.name || item.host}
          </Button>
        ))}
      </div>

      <PageHead
        title={client.name}
        subtitle={
          <span className={styles.meta}>
            {client.host}:{client.port} · {authText}
          </span>
        }
        actions={
          <>
            <Tooltip content={testing ? t("testing") : t("test")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<PlugConnected20Regular />}
                aria-label={t("test")}
                disabled={testing}
                onClick={() => void runTest()}
              />
            </Tooltip>
            <Tooltip content={t("back")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<ArrowLeft20Regular />}
                aria-label={t("back")}
                onClick={onBack}
              />
            </Tooltip>
          </>
        }
      />

      <TabList
        className={styles.subnav}
        selectedValue={tab}
        onTabSelect={(_, data) => onSelectTab(data.value as InstanceTab)}
      >
        <Tab value="status">{t("tabStatus")}</Tab>
        <Tab value="projects">{t("tabProjects")}</Tab>
        <Tab value="env">{t("tabEnv")}</Tab>
        <Tab value="logs">{t("tabLogs")}</Tab>
      </TabList>

      {testBanner ? (
        <MessageBar
          intent={testBanner.ok ? "success" : "error"}
          style={{ marginBottom: "14px", marginTop: "10px" }}
        >
          <MessageBarBody style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "12px", width: "100%" }}>
            <span>{testBanner.message}</span>
            <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
              <Button appearance="subtle" size="small" onClick={() => onSelectTab("logs")}>
                {t("viewLogs")}
              </Button>
              <Button appearance="subtle" size="small" onClick={() => setTestBanner(null)}>
                {t("close")}
              </Button>
            </div>
          </MessageBarBody>
        </MessageBar>
      ) : null}

      {tab === "status" ? (
        <InstanceHome client={client} status={status} onReload={onReload} />
      ) : null}
      {tab === "projects" ? (
        <ProjectsTab
          key={client.id}
          clientId={client.id}
          projects={projects}
          running={running}
          status={status}
          onReload={onReload}
        />
      ) : null}
      {tab === "env" ? <EnvTab key={client.id} clientId={client.id} status={status} /> : null}
      {tab === "logs" ? <LogsTab client={client} status={status} /> : null}

      <ConfirmDialog
        open={mismatch !== null}
        title={t("hostKeyMismatchTitle")}
        message={t("hostKeyMismatchBody", {
          stored: mismatch?.stored ?? "",
          presented: mismatch?.presented ?? "",
        })}
        confirmLabel={t("hostKeyTrust")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setMismatch(null)}
        onConfirm={() => void trustPresented()}
      />
    </div>
  );
}
