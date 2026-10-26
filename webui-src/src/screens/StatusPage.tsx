import type { JSX } from "react";
import { Button, Tooltip } from "@fluentui/react-components";
import { Copy } from "lucide-react";
import { useState } from "react";
import type { Status } from "../api";
import { makeT } from "../i18n/strings";
import { DescriptionList, PageHead } from "../components/Common";

export function StatusPage({ status }: { status: Status }): JSX.Element {
  const t = makeT(status.language);
  const [copiedMcp, setCopiedMcp] = useState(false);

  const credentials = status.auth.username
    ? `${status.auth.username} (${t("credentialsSet")})`
    : t("credentialsNone");

  return (
    <div className="w-full max-w-4xl">
      <PageHead title={t("statusTitle")} />
      {status.pageNote ? (
        <div className="mb-5 p-3.5 rounded-lg bg-surface border border-border text-xs sm:text-sm text-text-secondary">
          {status.pageNote}
        </div>
      ) : null}

      <div className="bg-card border border-border rounded-xl p-5 sm:p-6 shadow-sm">
        <DescriptionList
          items={[
            { term: t("version"), detail: <span className="font-mono">{status.app} v{status.version}</span> },
            { term: t("url"), detail: <code className="font-mono text-xs px-1.5 py-0.5 rounded bg-surface border border-border text-primary">{status.url}</code> },
            { term: t("port"), detail: <span className="font-mono font-medium">{status.port}</span> },
            {
              term: t("listen"),
              detail: (
                <span className="inline-flex items-center gap-1.5">
                  <span className={`w-2 h-2 rounded-full ${status.listen === "wan" ? "bg-amber-400" : "bg-emerald-400"}`} />
                  <span>{t(status.listen === "wan" ? "listenWan" : "listenLan")}</span>
                </span>
              ),
            },
            { term: t("language"), detail: status.language },
            { term: t("credentials"), detail: credentials },
            {
              term: t("clientRemote"),
              detail: status.client.loopback ? t("no") : t("yes"),
            },
            // MCP is a separate port, so it gets its own status row
            {
              term: t("mcpTitle"),
              detail: status.mcp.running ? (
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="font-mono text-xs bg-surface border border-border px-2 py-0.5 rounded">
                    {status.mcp.endpoint}
                  </span>
                  <span className="text-xs text-text-muted">
                    · {t("mcpTools", { n: String(status.mcp.tools) })}
                  </span>
                  <Tooltip content={copiedMcp ? t("copied") : t("mcpCopy")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<Copy className="w-3.5 h-3.5" />}
                      aria-label={t("mcpCopy")}
                      onClick={() => {
                        void navigator.clipboard.writeText(status.mcp.endpoint);
                        setCopiedMcp(true);
                        setTimeout(() => setCopiedMcp(false), 2000);
                      }}
                    />
                  </Tooltip>
                </div>
              ) : status.mcp.enabled ? (
                t("mcpNotRunning")
              ) : (
                t("mcpOff")
              ),
            },
            { term: t("configPath"), detail: <code className="font-mono text-xs text-text-muted break-all">{status.configPath}</code> },
          ]}
        />
      </div>
    </div>
  );
}
