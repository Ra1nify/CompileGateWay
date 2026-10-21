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
  ArrowClockwise20Regular,
  CheckmarkCircle20Regular,
  Code20Regular,
  Copy20Regular,
  DismissCircle20Regular,
} from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiDetectEnv,
  apiGetEnv,
  apiGetEnvScript,
  type EnvReport,
  type Status,
} from "../../api";
import { makeT } from "../../i18n/strings";
import { formatBytes, formatDateTime } from "../../format";
import { TERMINAL } from "../../terminalTheme";

const useStyles = makeStyles({
  screen: {
    display: "flex",
    flexDirection: "column",
    rowGap: "14px",
    maxWidth: "1080px",
  },
  toolbar: {
    display: "flex",
    alignItems: "center",
    columnGap: "10px",
    flexWrap: "wrap",
  },
  meta: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  card: {
    padding: "14px 18px",
    borderRadius: tokens.borderRadiusXLarge,
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
  },
  groupHead: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
  },
  row: {
    display: "flex",
    alignItems: "baseline",
    columnGap: "10px",
    flexWrap: "wrap",
  },
  label: {
    minWidth: "120px",
    fontSize: tokens.fontSizeBase300,
  },
  version: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    minWidth: "96px",
  },
  command: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  xcode: {
    display: "flex",
    flexDirection: "column",
    rowGap: "4px",
  },
  path: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
  },
  empty: {
    margin: 0,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
});

/**
 * Environment page (design doc §8): which tools and versions exist on the target, free disk.
 *
 * Detection really opens an SSH connection and runs a dozen `--version` calls, so it is
 * two-step: read the cache (`GET`) first, run (`POST`) only when the user clicks "detect".
 * Results stay in the server cache, so the task editor can read them (`apiGetEnv`) when
 * Xcode is selected without reconnecting.
 */
export function EnvTab({
  clientId,
  status,
}: {
  clientId: string;
  status: Status;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [report, setReport] = useState<EnvReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    (async () => {
      try {
        const result = await apiGetEnv(clientId);
        if (!cancelled) {
          setReport(result.report);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof ApiError ? err.message : String(err));
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [clientId]);

  async function detect(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const result = await apiDetectEnv(clientId);
      setReport(result.report);
    } catch (err) {
      setError(t("envFailed", { msg: err instanceof ApiError ? err.message : String(err) }));
    } finally {
      setBusy(false);
    }
  }

  const [scriptDialog, setScriptDialog] = useState(false);
  const [scriptContent, setScriptContent] = useState<string | null>(null);
  const [loadingScript, setLoadingScript] = useState(false);
  const [copiedScript, setCopiedScript] = useState(false);

  async function openScript(): Promise<void> {
    setScriptDialog(true);
    if (!scriptContent) {
      setLoadingScript(true);
      try {
        const res = await apiGetEnvScript();
        setScriptContent(res.script);
      } catch (err) {
        setScriptContent(err instanceof ApiError ? err.message : String(err));
      } finally {
        setLoadingScript(false);
      }
    }
  }

  return (
    <div className={styles.screen}>
      <div className={styles.toolbar}>
        <Tooltip content={t("envDetect")} relationship="description" withArrow>
          <Button
            appearance="primary"
            icon={<ArrowClockwise20Regular />}
            aria-label={t("envDetect")}
            disabled={busy}
            onClick={() => void detect()}
          />
        </Tooltip>
        <Tooltip content={t("envScriptTitle")} relationship="description" withArrow>
          <Button
            appearance="subtle"
            icon={<Code20Regular />}
            aria-label={t("envScript")}
            onClick={() => void openScript()}
          >
            {t("envScript")}
          </Button>
        </Tooltip>
        <span className={styles.meta}>
          {report
            ? `${t("envCheckedAt")} ${formatDateTime(report.checkedAt)} · ${report.elapsedMs} ms`
            : t("envNeverChecked")}
        </span>
        {busy ? <Spinner size="tiny" /> : null}
      </div>

      {error ? (
        <MessageBar intent="error">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      ) : null}

      {loading ? (
        <Spinner size="tiny" label={t("envLoading")} />
      ) : report === null ? (
        <p className={styles.empty}>{t("envNeverChecked")}</p>
      ) : (
        <>
          {!report.ok ? (
            <MessageBar intent="warning">
              <MessageBarBody>{report.message}</MessageBarBody>
            </MessageBar>
          ) : (
            <div className={styles.toolbar}>
              <Badge appearance="tint" color="informative" size="small">
                {report.os || "-"}
              </Badge>
              <span className={styles.meta}>{report.message}</span>
            </div>
          )}

          {report.disk ? (
            <Card className={styles.card}>
              <div className={styles.groupHead}>{t("envDisk")}</div>
              <div className={styles.row}>
                <span className={styles.version}>
                  {formatBytes(report.disk.availableBytes)}
                </span>
                <span className={styles.meta}>
                  {t("envDiskDetail", {
                    total: formatBytes(report.disk.totalBytes),
                    mount: report.disk.mount,
                    percent: Math.round(report.disk.usedRatio * 100),
                  })}
                </span>
              </div>
            </Card>
          ) : null}

          {report.xcodes.length > 0 ? (
            <Card className={styles.card}>
              <div className={styles.groupHead}>
                {t("envXcodeInstances")}
                <Badge
                  appearance="tint"
                  color={report.licenseOk ? "success" : "warning"}
                  size="small"
                  icon={
                    report.licenseOk ? <CheckmarkCircle20Regular /> : <DismissCircle20Regular />
                  }
                >
                  {report.licenseOk ? t("envLicenseOk") : t("envLicenseMissing")}
                </Badge>
              </div>
              {report.xcodes.map((instance) => (
                <div key={instance.path} className={styles.xcode}>
                  <div className={styles.row}>
                    <span className={styles.version}>{instance.version || "-"}</span>
                    <span className={styles.path}>{instance.path}</span>
                    {instance.build ? (
                      <span className={styles.meta}>
                        {t("envBuildVersion", { build: instance.build })}
                      </span>
                    ) : null}
                  </div>
                  <span className={styles.meta}>
                    {t("envDeveloperDir", { dir: instance.developerDir })}
                  </span>
                </div>
              ))}
              {!report.licenseOk && report.xcodeLicense ? (
                <span className={styles.command}>{report.xcodeLicense}</span>
              ) : null}
            </Card>
          ) : null}

          {report.groups.map((group) => (
            <Card key={group.platform} className={styles.card}>
              <div className={styles.groupHead}>
                {group.platform}
                {group.available ? null : (
                  <Badge appearance="tint" color="warning" size="small">
                    {t("envAllMissing")}
                  </Badge>
                )}
              </div>
              {group.items.map((item) => (
                <div key={item.id} className={styles.row}>
                  <span className={styles.label}>{item.label}</span>
                  {item.found ? (
                    <>
                      <span className={styles.version}>{item.version || t("envNoVersion")}</span>
                      <span className={styles.command}>{firstLine(item.output)}</span>
                    </>
                  ) : (
                    <>
                      <Badge appearance="tint" color="informative" size="small">
                        {t("envMissing")}
                      </Badge>
                      <span className={styles.command}>{item.command}</span>
                    </>
                  )}
                </div>
              ))}
            </Card>
          ))}
        </>
      )}

      <Dialog open={scriptDialog} onOpenChange={(_, data) => setScriptDialog(data.open)}>
        <DialogSurface style={{ maxWidth: "800px", width: "90vw" }}>
          <DialogTitle>{t("envScriptTitle")}</DialogTitle>
          <DialogBody>
            <DialogContent>
              {loadingScript ? (
                <Spinner label={t("envLoading")} style={{ padding: "30px 0" }} />
              ) : (
                <div
                  style={{
                    backgroundColor: TERMINAL.background,
                    color: TERMINAL.foreground,
                    fontFamily: tokens.fontFamilyMonospace,
                    fontSize: "12px",
                    lineHeight: "1.5",
                    padding: "12px 14px",
                    borderRadius: tokens.borderRadiusMedium,
                    maxHeight: "60vh",
                    overflowY: "auto",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-all",
                    border: `1px solid ${TERMINAL.border}`,
                  }}
                >
                  {scriptContent}
                </div>
              )}
            </DialogContent>
            <DialogActions>
              <Button
                appearance="subtle"
                icon={<Copy20Regular />}
                disabled={!scriptContent || loadingScript}
                onClick={() => {
                  if (scriptContent) {
                    void navigator.clipboard.writeText(scriptContent);
                    setCopiedScript(true);
                    setTimeout(() => setCopiedScript(false), 2000);
                  }
                }}
              >
                {copiedScript ? t("copied") : t("copyScript")}
              </Button>
              <Button appearance="secondary" onClick={() => setScriptDialog(false)}>
                {t("close")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </div>
  );
}

function firstLine(text: string): string {
  const line = text.split("\n").find((item) => item.trim() !== "") ?? "";
  return line.length > 96 ? `${line.slice(0, 96)}…` : line;
}
