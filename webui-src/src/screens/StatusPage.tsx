import type { JSX } from "react";
import { Button, Card, Tooltip, makeStyles, tokens } from "@fluentui/react-components";
import { Copy16Regular } from "@fluentui/react-icons";
import { useState } from "react";
import type { Status } from "../api";
import { makeT } from "../i18n/strings";
import { DescriptionList, PageHead } from "../components/Common";

const useStyles = makeStyles({
  screen: {
    maxWidth: "1080px",
  },
  note: {
    margin: "0 0 18px",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground3,
  },
  card: {
    maxWidth: "720px",
    borderRadius: tokens.borderRadiusXLarge,
    padding: "20px 22px",
  },
});

export function StatusPage({ status }: { status: Status }): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);
  const [copiedMcp, setCopiedMcp] = useState(false);

  const credentials = status.auth.username
    ? `${status.auth.username} (${t("credentialsSet")})`
    : t("credentialsNone");

  return (
    <div className={styles.screen}>
      <PageHead title={t("statusTitle")} />
      {status.pageNote ? <p className={styles.note}>{status.pageNote}</p> : null}

      <Card className={styles.card}>
        <DescriptionList
          items={[
            { term: t("version"), detail: `${status.app} ${status.version}` },
            { term: t("url"), detail: status.url },
            { term: t("port"), detail: String(status.port) },
            {
              term: t("listen"),
              detail: t(status.listen === "wan" ? "listenWan" : "listenLan"),
            },
            { term: t("language"), detail: status.language },
            { term: t("credentials"), detail: credentials },
            {
              term: t("clientRemote"),
              detail: status.client.loopback ? t("no") : t("yes"),
            },
            // MCP is a **separate port**, so it gets its own status row -- finding the
            // address an agent should use needs no trip to the settings page
            {
              term: t("mcpTitle"),
              detail: status.mcp.running ? (
                <span style={{ display: "inline-flex", alignItems: "center", gap: "8px" }}>
                  <span>{`${status.mcp.endpoint} · ${t("mcpTools", { n: String(status.mcp.tools) })}`}</span>
                  <Tooltip content={copiedMcp ? t("copied") : t("mcpCopy")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<Copy16Regular />}
                      aria-label={t("mcpCopy")}
                      onClick={() => {
                        void navigator.clipboard.writeText(status.mcp.endpoint);
                        setCopiedMcp(true);
                        setTimeout(() => setCopiedMcp(false), 2000);
                      }}
                    />
                  </Tooltip>
                </span>
              ) : status.mcp.enabled ? (
                t("mcpNotRunning")
              ) : (
                t("mcpOff")
              ),
            },
            { term: t("configPath"), detail: status.configPath },
          ]}
        />
      </Card>
    </div>
  );
}
