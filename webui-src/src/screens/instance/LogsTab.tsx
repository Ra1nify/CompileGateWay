import type { JSX } from "react";
import {
  Checkbox,
  Dropdown,
  MessageBar,
  MessageBarBody,
  Option,
  Spinner,
  Button,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Copy } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  ApiError,
  apiClientLogs,
  apiLogDay,
  apiLogDays,
  type ClientInfo,
  type LogEntry,
  type Status,
  type StoredLogEntry,
} from "../../api";
import { makeT } from "../../i18n/strings";
import { TERMINAL } from "../../terminalTheme";
import { formatClock } from "../../format";

const POLL_INTERVAL_MS = 1500;

/**
 * Hard cap on retained live lines (S18). The daemon keeps the ring buffer, but the UI
 * used to append forever: a long build would grow `entries` without bound and slowly
 * freeze the tab. Oldest lines are dropped first, matching the daemon's own policy.
 */
const MAX_LIVE_ENTRIES = 4000;

/**
 * Sources listed up front (design doc §10 calls this an **open enum**; `mcp` holds
 * a slot and sources seen in practice are added automatically).
 */
const SOURCE_SEED = ["webui", "cli", "mcp"] as const;

const useStyles = makeStyles({
  filters: {
    display: "flex",
    alignItems: "center",
    flexWrap: "wrap",
    columnGap: "14px",
    rowGap: "4px",
    marginBottom: "8px",
  },
  filterNote: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  source: {
    flex: "0 0 auto",
    minWidth: "56px",
    color: tokens.colorNeutralForeground4,
    fontSize: "11px",
    paddingTop: "2px",
  },
  scope: {
    flex: "0 0 auto",
    minWidth: "72px",
    color: tokens.colorNeutralForeground4,
    fontSize: "11px",
    paddingTop: "2px",
  },
  view: {
    maxWidth: "1080px",
    height: "calc(100vh - 330px)",
    minHeight: "220px",
    overflowY: "auto",
    padding: "12px 14px",
    // **Fixed dark**, ignores the theme -- this is a terminal, not a normal surface.
    // (colorNeutralBackgroundInverted used to flip to white on dark themes.)
    border: `1px solid ${TERMINAL.border}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: TERMINAL.background,
    color: TERMINAL.foreground,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12.5px",
    lineHeight: "1.6",
  },
  empty: {
    color: TERMINAL.dimmer,
  },
  line: {
    display: "flex",
    columnGap: "10px",
    alignItems: "flex-start",
    padding: "1px 0",
  },
  time: {
    flex: "0 0 auto",
    color: TERMINAL.dim,
    fontSize: "11px",
    paddingTop: "2px",
  },
  text: {
    flex: "1 1 auto",
    minWidth: 0,
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
  },
  info: { color: TERMINAL.ok },
  warn: { color: TERMINAL.warn },
  error: { color: TERMINAL.error },
  input: { color: TERMINAL.accent },
  out: { color: TERMINAL.foreground },
});

function levelClass(level: LogEntry["level"], styles: ReturnType<typeof useStyles>): string {
  switch (level) {
    case "info":
      return styles.info;
    case "warn":
      return styles.warn;
    case "error":
      return styles.error;
    case "input":
      return styles.input;
    default:
      return styles.out;
  }
}

/**
 * Logs tab: fetch increments every 1500ms, append new lines and scroll to bottom.
 * The timer is cleared on tab leave / unmount.
 *
 * The top can switch to **stored history** (design doc §10): pick a day to read that
 * day's `/api/logs/{day}`; source filters share one set of checkboxes in both modes.
 */
export function LogsTab({
  client,
  status,
}: {
  client: ClientInfo;
  status: Status;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);
  const [entries, setEntries] = useState<LogEntry[]>([]);
  /** Sources unchecked (the "source filter checkboxes" of design doc §10). */
  const [hiddenSources, setHiddenSources] = useState<string[]>([]);

  /** null = live; otherwise the day being viewed. */
  const [day, setDay] = useState<string | null>(null);
  const [days, setDays] = useState<string[]>([]);
  const [history, setHistory] = useState<StoredLogEntry[]>([]);
  const [historyNote, setHistoryNote] = useState<string | null>(null);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [copied, setCopied] = useState(false);

  const seqRef = useRef(0);
  const viewRef = useRef<HTMLDivElement | null>(null);

  const handleCopyLogs = () => {
    if (visible.length === 0) return;
    const text = visible
      .map(
        (row) =>
          `[${formatClock(row.ts)}] [${row.source}]${row.scope ? ` [${row.scope}]` : ""} ${row.text}`,
      )
      .join("\n");
    void navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  useEffect(() => {
    let cancelled = false;
    let timer: number | null = null;
    // Guard against overlapping requests (S17): if one round trip outlasts the interval,
    // the naive `setInterval` would fire again and stack requests, each appending its own
    // batch — duplicated lines and unbounded burst load on the daemon.
    let inFlight = false;

    const poll = async (): Promise<void> => {
      if (inFlight) {
        return;
      }
      inFlight = true;
      try {
        const result = await apiClientLogs(client.id, seqRef.current);
        if (cancelled) {
          return;
        }
        const fresh = result.entries ?? [];
        if (fresh.length) {
          for (const entry of fresh) {
            seqRef.current = Math.max(seqRef.current, entry.seq);
          }
          setEntries((current) => {
            const next = [...current, ...fresh];
            return next.length > MAX_LIVE_ENTRIES
              ? next.slice(next.length - MAX_LIVE_ENTRIES)
              : next;
          });
        }
      } catch {
        // Poll failure: just try again next tick.
      } finally {
        inFlight = false;
      }
    };

    void poll();
    timer = window.setInterval(() => void poll(), POLL_INTERVAL_MS);

    return () => {
      cancelled = true;
      if (timer !== null) {
        window.clearInterval(timer);
        timer = null;
      }
    };
  }, [client.id]);

  // Reset filters and history on client switch so state does not leak across clients
  useEffect(() => {
    setEntries([]);
    setHiddenSources([]);
    setDay(null);
    setHistory([]);
    setHistoryNote(null);
    seqRef.current = 0;
  }, [client.id]);

  // Which days are available (ask once before entering history mode)
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const result = await apiLogDays();
        if (!cancelled) {
          setDays(result.days ?? []);
        }
      } catch {
        // Persistence off means no history to pick; keep the list empty
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client.id, status.version]);

  // Read the selected day
  useEffect(() => {
    if (day === null) {
      return;
    }
    let cancelled = false;
    setLoadingHistory(true);
    setHistoryNote(null);
    (async () => {
      try {
        const result = await apiLogDay(day, { limit: 2000 });
        if (!cancelled) {
          setHistory(result.entries ?? []);
        }
      } catch (err) {
        if (!cancelled) {
          setHistory([]);
          setHistoryNote(err instanceof ApiError ? err.message : String(err));
        }
      } finally {
        if (!cancelled) {
          setLoadingHistory(false);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [day]);

  useEffect(() => {
    const view = viewRef.current;
    if (view) {
      // Live mode scrolls to bottom; history mode stays at top (oldest first)
      view.scrollTop = day === null ? view.scrollHeight : 0;
    }
  }, [entries, history, day]);

  // Normalize both modes into one display shape
  const rows: Row[] = useMemo(() => {
    if (day === null) {
      return entries.map((entry) => ({
        key: `live-${entry.seq}`,
        level: entry.level,
        text: entry.text,
        ts: entry.ts_ms,
        source: entry.source || "daemon",
        scope: null,
      }));
    }
    return history.map((entry, index) => ({
      key: `hist-${index}-${entry.tsMs}`,
      level: entry.level,
      text: entry.text,
      ts: entry.tsMs,
      source: entry.source || "daemon",
      scope: entry.scope,
    }));
  }, [entries, history, day]);

  // Sources are an open enum: the fixed three (a slot for mcp) plus any seen in practice
  const sources = useMemo(() => {
    const known = new Set<string>([SOURCE_SEED[0], SOURCE_SEED[1], SOURCE_SEED[2]]);
    for (const row of rows) {
      known.add(row.source);
    }
    return Array.from(known).sort();
  }, [rows]);

  const visible = rows.filter((row) => !hiddenSources.includes(row.source));
  const hiddenCount = rows.length - visible.length;

  return (
    <>
      <div className={styles.filters}>
        <Dropdown
          aria-label={t("logRange")}
          selectedOptions={[day ?? LIVE]}
          value={day === null ? t("logLive") : day}
          onOptionSelect={(_, data) => setDay(data.optionValue === LIVE ? null : (data.optionValue ?? null))}
        >
          <Option value={LIVE}>{t("logLive")}</Option>
          {days.map((item) => (
            <Option key={item} value={item}>
              {item}
            </Option>
          ))}
        </Dropdown>
        {loadingHistory ? <Spinner size="tiny" /> : null}
        <span className={styles.filterNote}>
          {day === null
            ? t("logLiveHint")
            : t("logHistoryHint", { n: rows.length })}
        </span>
      </div>

      {historyNote ? (
        <MessageBar intent="warning" style={{ marginBottom: 8 }}>
          <MessageBarBody>{historyNote}</MessageBarBody>
        </MessageBar>
      ) : null}

      <div className={styles.filters}>
        <Checkbox
          checked={hiddenSources.length === 0}
          label={t("logAllSources")}
          onChange={(_, data) => setHiddenSources(data.checked === true ? [] : [...sources])}
        />
        {sources.map((source) => (
          <Checkbox
            key={source}
            checked={!hiddenSources.includes(source)}
            label={source}
            onChange={(_, data) =>
              setHiddenSources((current) =>
                data.checked === true
                  ? current.filter((item) => item !== source)
                  : current.includes(source)
                    ? current
                    : [...current, source],
              )
            }
          />
        ))}
        {hiddenCount > 0 ? (
          <span className={styles.filterNote}>{t("logHidden", { n: hiddenCount })}</span>
        ) : null}
        <div style={{ marginLeft: "auto" }}>
          <Tooltip content={copied ? t("logCopied") : t("logCopy")} relationship="description" withArrow>
            <Button
              size="small"
              icon={<Copy className="w-4 h-4" />}
              disabled={visible.length === 0}
              onClick={handleCopyLogs}
            >
              {copied ? t("logCopied") : t("logCopy")}
            </Button>
          </Tooltip>
        </div>
      </div>

      <div className={styles.view} ref={viewRef} role="log" aria-live="polite" aria-label={t("tabLogs")}>
        {visible.length === 0 ? <div className={styles.empty}>{t("emptyLogs")}</div> : null}
        {visible.map((row) => (
          <div key={row.key} className={styles.line}>
            <span className={styles.time}>{formatClock(row.ts)}</span>
            <span className={styles.source}>{row.source}</span>
            {row.scope ? <span className={styles.scope}>{row.scope}</span> : null}
            <span className={`${styles.text} ${levelClass(row.level, styles)}`}>{row.text}</span>
          </div>
        ))}
      </div>
    </>
  );
}

/** The item in the stored-log dropdown that means "live". */
const LIVE = "__live__";

interface Row {
  key: string;
  level: LogEntry["level"];
  text: string;
  ts: number;
  source: string;
  /** Only set in history mode (live mode is fetched per client, so the domain is fixed). */
  scope: string | null;
}
