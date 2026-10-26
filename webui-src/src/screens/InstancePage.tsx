import type { JSX } from "react";
import {
  Button,
  MessageBar,
  MessageBarBody,
  Tooltip,
} from "@fluentui/react-components";
import { ArrowLeft, Zap } from "lucide-react";
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
    <div className="w-full max-w-5xl">
      {/* Horizontal Client Machine Switcher Strip */}
      <div className="flex items-center gap-1.5 overflow-x-auto pb-3 mb-4 border-b border-border/80 scrollbar-none" aria-label={t("clientsTitle")}>
        {clients.map((item) => {
          const active = item.id === client.id;
          return (
            <button
              key={item.id}
              onClick={() => onSelectClient(item.id)}
              className={`px-3 py-1 text-xs font-medium rounded-full transition-all shrink-0 ${
                active
                  ? "bg-primary text-white shadow-sm"
                  : "bg-card border border-border text-text-secondary hover:text-text hover:bg-cardHover"
              }`}
            >
              {item.name || item.host}
            </button>
          );
        })}
      </div>

      <PageHead
        title={client.name}
        subtitle={
          <span className="text-xs text-text-muted font-mono">
            {client.host}:{client.port} · {authText}
          </span>
        }
        actions={
          <>
            <Tooltip content={testing ? t("testing") : t("test")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<Zap className="w-4 h-4 text-amber-400" />}
                aria-label={t("test")}
                disabled={testing}
                onClick={() => void runTest()}
              />
            </Tooltip>
            <Tooltip content={t("back")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<ArrowLeft className="w-4 h-4" />}
                aria-label={t("back")}
                onClick={onBack}
              />
            </Tooltip>
          </>
        }
      />

      {/* Segmented Control Subnav */}
      <div className="inline-flex p-1 bg-surface border border-border rounded-lg gap-1 mb-6 max-w-full overflow-x-auto">
        {[
          { id: "status", label: t("tabStatus") },
          { id: "projects", label: t("tabProjects") },
          { id: "env", label: t("tabEnv") },
          { id: "logs", label: t("tabLogs") },
        ].map((tabItem) => {
          const active = tab === tabItem.id;
          return (
            <button
              key={tabItem.id}
              onClick={() => onSelectTab(tabItem.id as InstanceTab)}
              className={`px-3.5 py-1.5 text-xs font-medium rounded-md transition-all whitespace-nowrap ${
                active
                  ? "bg-card text-text border border-border/80 shadow-sm"
                  : "text-text-muted hover:text-text hover:bg-card/50"
              }`}
            >
              {tabItem.label}
            </button>
          );
        })}
      </div>

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
