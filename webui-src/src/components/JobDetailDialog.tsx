import type { JSX } from "react";
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Spinner,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  ArrowClockwise20Regular,
  Copy20Regular,
} from "@fluentui/react-icons";
import { useEffect, useRef, useState } from "react";
import {
  apiJobLogs,
  type Job,
  type JobKind,
  type JobState,
  type Language,
  type LogEntry,
} from "../api";
import { makeT } from "../i18n/strings";
import { TERMINAL } from "../terminalTheme";
import { formatClock, formatDateTime } from "../format";

const useStyles = makeStyles({
  surface: {
    maxWidth: "920px",
    width: "92vw",
    maxHeight: "90vh",
  },
  header: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: "12px",
  },
  badges: {
    display: "flex",
    gap: "8px",
    alignItems: "center",
    flexWrap: "wrap",
    marginTop: "4px",
    marginBottom: "12px",
  },
  metaGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(140px, 1fr))",
    gap: "10px",
    padding: "10px 14px",
    backgroundColor: tokens.colorNeutralBackground3,
    borderRadius: tokens.borderRadiusMedium,
    fontSize: tokens.fontSizeBase200,
    marginBottom: "14px",
  },
  metaItem: {
    display: "flex",
    flexDirection: "column",
    gap: "2px",
  },
  metaLabel: {
    color: tokens.colorNeutralForeground3,
    fontSize: "11px",
  },
  metaValue: {
    fontWeight: tokens.fontWeightSemibold,
    fontFamily: tokens.fontFamilyMonospace,
  },
  sectionTitle: {
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
    marginTop: "16px",
    marginBottom: "8px",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
  },
  stepsList: {
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    marginBottom: "16px",
  },
  stepRow: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: "8px",
    padding: "8px 12px",
    backgroundColor: tokens.colorNeutralBackground2,
    borderRadius: tokens.borderRadiusSmall,
    fontSize: tokens.fontSizeBase200,
  },
  stepMain: {
    display: "flex",
    alignItems: "center",
    gap: "8px",
    flex: 1,
    overflow: "hidden",
  },
  stepName: {
    fontWeight: tokens.fontWeightMedium,
    whiteSpace: "nowrap",
    overflow: "hidden",
    textOverflow: "ellipsis",
  },
  logBox: {
    backgroundColor: TERMINAL.background,
    color: TERMINAL.foreground,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12px",
    lineHeight: "1.55",
    padding: "10px 12px",
    borderRadius: tokens.borderRadiusMedium,
    height: "260px",
    overflowY: "auto",
    border: `1px solid ${TERMINAL.border}`,
  },
  logLine: {
    display: "flex",
    gap: "8px",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    padding: "1px 0",
  },
  logTime: {
    color: TERMINAL.dim,
    flexShrink: 0,
    fontSize: "11px",
  },
  logLevel: {
    flexShrink: 0,
    fontSize: "10px",
    textTransform: "uppercase",
    width: "44px",
  },
  empty: {
    color: TERMINAL.dimmer,
    padding: "16px 0",
    textAlign: "center",
  },
});

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

export function JobDetailDialog({
  open,
  job,
  language,
  onClose,
  onRetry,
}: {
  open: boolean;
  job: Job | null;
  language: Language;
  onClose: () => void;
  onRetry?: (job: Job) => void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(language);

  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [copied, setCopied] = useState(false);
  const logBoxRef = useRef<HTMLDivElement>(null);
  const aliveRef = useRef(true);

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

  useEffect(() => {
    aliveRef.current = true;
    if (!open || !job) {
      setLogs([]);
      return;
    }

    setLoading(true);
    let lastSeq = 0;

    async function fetchLogs(): Promise<void> {
      if (!job || !aliveRef.current) return;
      try {
        const result = await apiJobLogs(job.id, lastSeq);
        if (!aliveRef.current) return;
        if (result.entries && result.entries.length > 0) {
          lastSeq = result.entries[result.entries.length - 1].seq;
          setLogs((prev) => [...prev, ...result.entries]);
          // auto scroll
          setTimeout(() => {
            if (logBoxRef.current) {
              logBoxRef.current.scrollTop = logBoxRef.current.scrollHeight;
            }
          }, 50);
        }
      } catch {
        // logs fetch failure is non-fatal
      } finally {
        if (aliveRef.current) {
          setLoading(false);
        }
      }
    }

    void fetchLogs();

    // If job is still running/pending, poll for logs every 1.5s
    let intervalId: number | null = null;
    if (job.state === "running" || job.state === "pending") {
      intervalId = window.setInterval(() => {
        void fetchLogs();
      }, 1500);
    }

    return () => {
      aliveRef.current = false;
      if (intervalId !== null) {
        clearInterval(intervalId);
      }
    };
  }, [open, job?.id, job?.state]);

  if (!job) {
    return <></>;
  }

  const allArtifacts = [
    job.artifactPath,
    ...(job.artifacts || []),
  ].filter((p): p is string => Boolean(p && p.trim()));
  const uniqueArtifacts = Array.from(new Set(allArtifacts));

  return (
    <Dialog open={open} onOpenChange={(_, data) => !data.open && onClose()}>
      <DialogSurface className={styles.surface}>
        <DialogTitle className={styles.header}>
          <span>{job.title || job.id}</span>
        </DialogTitle>
        <DialogBody>
          <div className={styles.badges}>
            <Badge appearance="tint" color="informative" size="small">
              {kindLabel(job.kind)}
            </Badge>
            <Badge appearance="tint" color={stateColor(job.state)} size="small">
              {stateLabel(job.state)}
            </Badge>
            <span style={{ fontSize: "11px", color: tokens.colorNeutralForeground4 }}>
              ID: {job.id}
            </span>
          </div>

          <DialogContent>
            <div className={styles.metaGrid}>
              <div className={styles.metaItem}>
                <span className={styles.metaLabel}>{t("jobStartedAt")}</span>
                <span className={styles.metaValue}>
                  {job.startedAt > 0 ? formatDateTime(job.startedAt) : "-"}
                </span>
              </div>
              <div className={styles.metaItem}>
                <span className={styles.metaLabel}>{t("jobDuration")}</span>
                <span className={styles.metaValue}>
                  {job.elapsedSeconds > 0 ? `${job.elapsedSeconds}s` : "-"}
                </span>
              </div>
              <div className={styles.metaItem}>
                <span className={styles.metaLabel}>{t("jobExitCode")}</span>
                <span className={styles.metaValue}>
                  {job.exitCode >= 0 ? job.exitCode : "-"}
                </span>
              </div>
              {job.files > 0 ? (
                <div className={styles.metaItem}>
                  <span className={styles.metaLabel}>{t("jobFiles")}</span>
                  <span className={styles.metaValue}>{job.files}</span>
                </div>
              ) : null}
            </div>

            {job.message ? (
              <div
                style={{
                  padding: "8px 12px",
                  backgroundColor: tokens.colorNeutralBackground2,
                  borderRadius: tokens.borderRadiusSmall,
                  fontSize: tokens.fontSizeBase200,
                  marginBottom: "12px",
                }}
              >
                {job.message}
              </div>
            ) : null}

            {job.steps && job.steps.length > 0 ? (
              <>
                <div className={styles.sectionTitle}>
                  <span>{t("jobSteps")} ({job.steps.length})</span>
                </div>
                <div className={styles.stepsList}>
                  {job.steps.map((step) => (
                    <div key={step.index} className={styles.stepRow}>
                      <div className={styles.stepMain}>
                        <span style={{ color: tokens.colorNeutralForeground4, minWidth: "20px" }}>
                          #{step.index + 1}
                        </span>
                        <span className={styles.stepName}>{step.name || step.taskId}</span>
                        <Badge appearance="tint" color="informative" size="small">
                          {step.kind}
                        </Badge>
                        <Badge
                          appearance="tint"
                          color={step.state === "ok" ? "success" : step.state === "failed" ? "danger" : "informative"}
                          size="small"
                        >
                          {step.state}
                        </Badge>
                      </div>
                      <div style={{ display: "flex", gap: "10px", alignItems: "center" }}>
                        {step.elapsedSeconds > 0 ? (
                          <span style={{ fontSize: "11px", color: tokens.colorNeutralForeground3 }}>
                            {step.elapsedSeconds}s
                          </span>
                        ) : null}
                        {step.message ? (
                          <span style={{ fontSize: "11px", color: tokens.colorNeutralForeground3, maxWidth: "200px" }}>
                            {step.message}
                          </span>
                        ) : null}
                      </div>
                    </div>
                  ))}
                </div>
              </>
            ) : null}

            {uniqueArtifacts.length > 0 ? (
              <>
                <div className={styles.sectionTitle}>
                  <span>{t("jobArtifacts")}</span>
                </div>
                <div
                  style={{
                    display: "flex",
                    flexDirection: "column",
                    gap: "4px",
                    marginBottom: "14px",
                  }}
                >
                  {uniqueArtifacts.map((art, idx) => (
                    <code
                      key={idx}
                      style={{
                        padding: "4px 8px",
                        backgroundColor: tokens.colorNeutralBackground3,
                        borderRadius: tokens.borderRadiusSmall,
                        fontSize: "11px",
                        wordBreak: "break-all",
                      }}
                    >
                      {art}
                    </code>
                  ))}
                </div>
              </>
            ) : null}

            <div className={styles.sectionTitle}>
              <span>{t("jobLogs")}</span>
              <Tooltip content={t("copyLogs")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  size="small"
                  icon={<Copy20Regular />}
                  disabled={logs.length === 0}
                  onClick={() => {
                    const fullText = logs.map((l) => `${formatClock(l.ts_ms)} [${l.level}] ${l.text}`).join("\n");
                    void navigator.clipboard.writeText(fullText);
                    setCopied(true);
                    setTimeout(() => setCopied(false), 2000);
                  }}
                >
                  {copied ? t("copied") : t("copyLogs")}
                </Button>
              </Tooltip>
            </div>

            <div className={styles.logBox} ref={logBoxRef}>
              {loading && logs.length === 0 ? (
                <div className={styles.empty}>
                  <Spinner size="small" />
                </div>
              ) : logs.length === 0 ? (
                <div className={styles.empty}>{t("jobNoLogs")}</div>
              ) : (
                logs.map((entry, idx) => (
                  <div key={entry.seq ?? idx} className={styles.logLine}>
                    <span className={styles.logTime}>{formatClock(entry.ts_ms)}</span>
                    <span
                      className={styles.logLevel}
                      style={{
                        color:
                          entry.level === "error"
                            ? tokens.colorPaletteRedForeground1
                            : entry.level === "warn"
                            ? tokens.colorPaletteYellowForeground1
                            : TERMINAL.dim,
                      }}
                    >
                      [{entry.level}]
                    </span>
                    <span>{entry.text}</span>
                  </div>
                ))
              )}
            </div>
          </DialogContent>

          <DialogActions>
            {job.retryFromStep !== null &&
            job.retryFromStep !== undefined &&
            job.state !== "running" &&
            onRetry ? (
              <Button
                appearance="primary"
                icon={<ArrowClockwise20Regular />}
                onClick={() => {
                  onRetry(job);
                  onClose();
                }}
              >
                {t("jobRetryFrom", { n: job.retryFromStep + 1 })}
              </Button>
            ) : null}
            <Button appearance="secondary" onClick={onClose}>
              {t("close")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
