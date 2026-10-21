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
  Radio,
  Spinner,
  Tab,
  TabList,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  Add20Regular,
  ArrowClockwise20Regular,
  Copy20Regular,
  DismissCircle20Regular,
  Document20Regular,
  Folder20Regular,
  FolderOpen20Regular,
  Link20Regular,
} from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiProjectFile,
  apiProjectTree,
  type PathSide,
  type Status,
  type TreeEntry,
  type TreeListing,
} from "../api";
import { makeT } from "../i18n/strings";
import { formatBytes, formatDateTime } from "../format";

const useStyles = makeStyles({
  preview: {
    marginTop: "10px",
    padding: "10px 12px",
    display: "flex",
    flexDirection: "column",
    rowGap: "6px",
  },
  previewHead: {
    display: "flex",
    alignItems: "center",
    columnGap: "10px",
  },
  previewText: {
    margin: 0,
    maxHeight: "220px",
    overflow: "auto",
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12.5px",
    lineHeight: "1.6",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
  },
  surface: {
    maxWidth: "820px",
    width: "820px",
  },
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "10px",
  },
  crumb: {
    display: "flex",
    alignItems: "center",
    columnGap: "6px",
    flexWrap: "wrap",
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
  },
  list: {
    display: "flex",
    flexDirection: "column",
    maxHeight: "46vh",
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
  },
  row: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    padding: "4px 8px",
    borderBottom: `1px solid ${tokens.colorNeutralStroke3}`,
    ":hover": {
      backgroundColor: tokens.colorNeutralBackground1Hover,
    },
  },
  name: {
    flex: "1 1 auto",
    minWidth: 0,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  nameButton: {
    justifyContent: "flex-start",
    minWidth: 0,
    flex: "1 1 auto",
  },
  meta: {
    flex: "0 0 auto",
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  empty: {
    margin: 0,
    padding: "14px 10px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  truncatedNotice: {
    padding: "8px 12px",
    backgroundColor: tokens.colorNeutralBackground3,
    borderRadius: tokens.borderRadiusMedium,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
    borderLeft: `3px solid ${tokens.colorPaletteYellowBorder2}`,
  },
});

/**
 * Project file tree (design doc §7.4 / §9). Read-only, one level at a time; the copy button
 * gives the relative path that copy / move / delete / export tasks need as `path`.
 */
export function TreeDialog({
  open,
  status,
  projectId,
  projectName,
  onClose,
  onCreateTask,
  pick,
}: {
  open: boolean;
  status: Status;
  projectId: string;
  projectName: string;
  onClose: () => void;
  /**
   * Design doc §9: hand `(side, path)` back so the project page can open a new task.
   */
  onCreateTask?: (side: PathSide, path: string) => void;
  /**
   * "Pick a path" mode for a task field (`from` / `to` / `target` / `sources`): rows become
   * selectable instead of creating a task.
   */
  pick?: {
    /** Which side / directory to start in. */
    side: PathSide;
    path: string;
    /** Directories only (e.g. `to` is usually a dir); by default files are pickable too. */
    dirsOnly?: boolean;
    onPick: (side: PathSide, path: string) => void;
  };
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);

  const [side, setSide] = useState<PathSide>("remote");
  const [path, setPath] = useState("");
  const [listing, setListing] = useState<TreeListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  // Design doc §7.4 "controlled read": clicking a file fetches it (binaries only report meta).
  const [preview, setPreview] = useState<
    { path: string; text: string; size: number; truncated: boolean; binary: boolean } | null
  >(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  /** The row selected in pick-path mode. */
  const [picked, setPicked] = useState<string>("");
  /** Manual refresh: changing the value refetches (setting the same path does not). */
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    if (!open) {
      return;
    }
    // Pick mode starts from the task's own side/path; normal mode returns to the root.
    setSide(pick?.side ?? "remote");
    setPath(pick?.path ?? "");
    setPicked("");
    setListing(null);
    setError(null);
    setCopied(null);
  }, [open, projectId, pick?.side, pick?.path]);

  useEffect(() => {
    if (!open) {
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    (async () => {
      try {
        const result = await apiProjectTree(projectId, side, path);
        if (!cancelled) {
          setListing(result.tree);
        }
      } catch (err) {
        if (!cancelled) {
          setListing(null);
          setError(
            err instanceof ApiError
              ? `${err.message}${err.code ? ` (${err.code})` : ""}`
              : String(err),
          );
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
  }, [open, projectId, side, path, nonce]);

  /** Click a file: controlled read of its content (binaries only report meta). */
  async function openPreview(entryPath: string): Promise<void> {
    setPreviewBusy(true);
    setPreview(null);
    try {
      const result = await apiProjectFile(projectId, side, entryPath);
      setPreview({
        path: result.path,
        text: result.text,
        size: result.size,
        truncated: result.truncated,
        binary: result.binary,
      });
    } catch (err) {
      setError(
        err instanceof ApiError
          ? `${err.message}${err.code ? ` (${err.code})` : ""}`
          : String(err),
      );
    } finally {
      setPreviewBusy(false);
    }
  }

  if (!open) {
    return null;
  }

  async function copyPath(entry: TreeEntry): Promise<void> {
    try {
      await navigator.clipboard.writeText(entry.path);
      setCopied(entry.path);
      window.setTimeout(() => setCopied((current) => (current === entry.path ? null : current)), 1500);
    } catch {
      setCopied(null);
    }
  }

  function iconOf(entry: TreeEntry): JSX.Element {
    if (entry.kind === "dir") {
      return <Folder20Regular />;
    }
    if (entry.kind === "symlink") {
      return <Link20Regular />;
    }
    return <Document20Regular />;
  }

  const segments = listing?.path ? listing.path.split("/") : [];

  return (
    <Dialog
      open
      modalType="modal"
      onOpenChange={(_, data) => {
        if (!data.open) {
          onClose();
        }
      }}
    >
      <DialogSurface className={styles.surface}>
        <DialogBody>
          <DialogTitle>
            {t("treeTitle")}
            <span className={styles.meta}>{` · ${projectName}`}</span>
          </DialogTitle>
          <DialogContent className={styles.body}>
            <TabList
              selectedValue={side}
              onTabSelect={(_, data) => {
                setSide(data.value as PathSide);
                setPath("");
              }}
            >
              <Tab value="remote">{t("sideRemote")}</Tab>
              <Tab value="host">{t("sideHost")}</Tab>
            </TabList>

            <div className={styles.crumb}>
              <Button
                appearance="subtle"
                size="small"
                icon={<FolderOpen20Regular />}
                disabled={listing?.parent === null || listing === null}
                onClick={() => setPath(listing?.parent ?? "")}
              >
                {t("treeUp")}
              </Button>
              <span>{`${side === "remote" ? t("sideRemote") : t("sideHost")}:/`}</span>
              {segments.map((segment, index) => (
                <span key={`${segment}-${index}`}>
                  <Button
                    appearance="subtle"
                    size="small"
                    onClick={() => setPath(segments.slice(0, index + 1).join("/"))}
                  >
                    {segment}
                  </Button>
                  {index < segments.length - 1 ? <span>/</span> : null}
                </span>
              ))}
              {loading ? <Spinner size="tiny" /> : null}
            </div>

            {listing ? (
              <div className={styles.meta}>{`${t("treeRoot")} ${listing.root}`}</div>
            ) : null}

            {error ? (
              <MessageBar intent="warning">
                <MessageBarBody>{error}</MessageBarBody>
              </MessageBar>
            ) : null}

            <div className={styles.list}>
              {listing && listing.entries.length === 0 && !loading ? (
                <p className={styles.empty}>{t("treeEmpty")}</p>
              ) : null}
              {pick ? (
                // Pick mode: the current directory itself is selectable (e.g. `to` is a dir)
                <div className={styles.row}>
                  <Radio
                    checked={picked === ""}
                    label={`${t("treePickHere")}${path ? ` /${path}` : " /"}`}
                    onChange={() => setPicked("")}
                  />
                </div>
              ) : null}
              {(listing?.entries ?? []).map((entry) => (
                <div key={entry.path} className={styles.row}>
                  {pick ? (
                    <Radio
                      className={styles.nameButton}
                      checked={picked === entry.path}
                      disabled={entry.kind !== "dir" && pick.dirsOnly === true}
                      label={entry.name}
                      onChange={() => setPicked(entry.path)}
                    />
                  ) : entry.kind === "dir" ? (
                    <Button
                      className={styles.nameButton}
                      appearance="subtle"
                      size="small"
                      icon={iconOf(entry)}
                      onClick={() => setPath(entry.path)}
                    >
                      <span className={styles.name}>{entry.name}</span>
                    </Button>
                  ) : (
                    <Button
                      className={styles.nameButton}
                      appearance="subtle"
                      size="small"
                      icon={iconOf(entry)}
                      disabled={entry.kind !== "file"}
                      onClick={() => void openPreview(entry.path)}
                    >
                      <span className={styles.name}>{entry.name}</span>
                    </Button>
                  )}
                  {entry.kind === "symlink" ? (
                    <Badge appearance="tint" color="warning" size="small">
                      {t("treeSymlink")}
                    </Badge>
                  ) : null}
                  <span className={styles.meta}>
                    {entry.kind === "file" ? formatBytes(entry.size) : ""}
                  </span>
                  <span className={styles.meta}>
                    {entry.modifiedAt > 0 ? formatDateTime(entry.modifiedAt * 1000) : ""}
                  </span>
                  <Tooltip
                    content={copied === entry.path ? t("copied") : t("treeCopyPath")}
                    relationship="description"
                    withArrow
                  >
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<Copy20Regular />}
                      aria-label={t("treeCopyPath")}
                      onClick={() => void copyPath(entry)}
                    />
                  </Tooltip>
                </div>
              ))}
            </div>

            {listing?.truncated ? (
              <div className={styles.truncatedNotice}>{t("treeTruncated")}</div>
            ) : null}

            {/* Controlled preview (design doc §7.4): fetch on click, binaries report size only */}
            {previewBusy ? (
              <div className={styles.meta}>
                <Spinner size="tiny" /> {t("filePreviewLoading")}
              </div>
            ) : null}
            {preview ? (
              <Card className={styles.preview}>
                <div className={styles.previewHead}>
                  <span className={styles.name}>{preview.path}</span>
                  <span className={styles.meta}>{formatBytes(preview.size)}</span>
                  <Button
                    appearance="subtle"
                    size="small"
                    icon={<DismissCircle20Regular />}
                    aria-label={t("close")}
                    onClick={() => setPreview(null)}
                  />
                </div>
                {preview.binary ? (
                  <div className={styles.meta}>{t("filePreviewBinary")}</div>
                ) : (
                  <>
                    {preview.truncated ? (
                      <div className={styles.meta}>{t("filePreviewTruncated")}</div>
                    ) : null}
                    <pre className={styles.previewText}>{preview.text}</pre>
                  </>
                )}
              </Card>
            ) : null}
          </DialogContent>
          <DialogActions>
            <Button
              appearance="subtle"
              icon={<ArrowClockwise20Regular />}
              aria-label={t("refresh")}
              onClick={() => setNonce((current) => current + 1)}
            />
            {onCreateTask ? (
              <Tooltip content={t("treeNewTaskHint")} relationship="description" withArrow>
                <Button
                  appearance="secondary"
                  icon={<Add20Regular />}
                  onClick={() => {
                    onCreateTask(side, listing?.path ?? "");
                    onClose();
                  }}
                >
                  {t("treeNewTask")}
                </Button>
              </Tooltip>
            ) : null}
            {pick ? (
              <Button
                appearance="primary"
                disabled={picked !== "" && listing === null}
                onClick={() => {
                  pick.onPick(side, picked);
                  onClose();
                }}
              >
                {picked === ""
                  ? `${t("treePickHere")}${path ? ` /${path}` : " /"}`
                  : t("treePickUse")}
              </Button>
            ) : (
              <Button appearance="primary" onClick={onClose}>
                {t("ok")}
              </Button>
            )}
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}




