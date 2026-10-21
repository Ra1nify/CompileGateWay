import type { JSX } from "react";
import {
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  Input,
  Link,
  MessageBar,
  MessageBarBody,
  Radio,
  RadioGroup,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { ArrowDownload20Regular, DocumentArrowDown20Regular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiListExports,
  apiProjectTree,
  exportDownloadUrl,
  type ExportItem,
  type Project,
  type Status,
} from "../api";
import { makeT } from "../i18n/strings";
import { formatBytes, formatDateTime } from "../format";

const useStyles = makeStyles({
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "14px",
    minWidth: "420px",
  },
  hint: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    lineHeight: "1.5",
  },
  list: {
    display: "flex",
    flexDirection: "column",
    rowGap: "6px",
    maxHeight: "180px",
    overflowY: "auto",
  },
  item: {
    display: "flex",
    alignItems: "center",
    columnGap: "10px",
    fontSize: tokens.fontSizeBase300,
  },
  grow: {
    flex: "1 1 auto",
    overflowWrap: "anywhere",
  },
  muted: {
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
  },
});

/**
 * Export shortcut task (design doc §9). Top half lists past archives + download links;
 * bottom half builds a new one. Real start is delegated to `onConfirm`.
 */
export function ExportDialog({
  open,
  project,
  status,
  onClose,
  onConfirm,
}: {
  open: boolean;
  project: Project | null;
  status: Status;
  onClose: () => void;
  onConfirm: (params: { source: "remote" | "host"; path: string; name: string }) => void;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);

  const [source, setSource] = useState<"remote" | "host">("remote");
  const [format, setFormat] = useState<"zip" | "tar.gz">("zip");
  const [hasSymlinks, setHasSymlinks] = useState(false);
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [items, setItems] = useState<ExportItem[]>([]);
  const [dir, setDir] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open || !project) {
      return;
    }
    setError(null);
    setName("");
    setFormat("zip");
    setHasSymlinks(false);
    // Default path: the last build artifact (§9). Strip any `remote:` / `host:` prefix and
    // set the source to match.
    const artifact = (project.lastArtifact ?? "")
      .split("\n")
      .map((line) => line.trim())
      .find((line) => line.length > 0);
    if (artifact) {
      setPath(artifact.replace(/^(remote|host):/, ""));
      setSource(artifact.startsWith("host:") ? "host" : "remote");
    } else {
      setPath("");
      setSource("remote");
    }
    void refresh(project.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, project?.id]);

  useEffect(() => {
    if (!open || !project) {
      setHasSymlinks(false);
      return;
    }
    let active = true;
    const targetPath = path.trim();
    apiProjectTree(project.id, source, targetPath)
      .then((res) => {
        if (!active) return;
        const found = (res.tree.entries ?? []).some((e) => e.kind === "symlink");
        setHasSymlinks(found);
        if (found) {
          setFormat("tar.gz");
        }
      })
      .catch(() => {
        if (!active) return;
        setHasSymlinks(false);
      });
    return () => {
      active = false;
    };
  }, [open, project?.id, source, path]);

  async function refresh(projectId: string): Promise<void> {
    try {
      const result = await apiListExports(projectId);
      setItems(result.exports ?? []);
      setDir(result.dir ?? "");
    } catch (err) {
      setError(err instanceof ApiError ? err.message : String(err));
    }
  }

  if (!open || !project) {
    return null;
  }

  return (
    <Dialog open onOpenChange={(_, data) => (!data.open ? onClose() : undefined)}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{t("exportTitle")}</DialogTitle>
          <DialogContent className={styles.body}>
            <div className={styles.hint}>{t("exportHint")}</div>

            <div>
              <div className={styles.muted}>{t("exportExisting")}</div>
              {items.length === 0 ? (
                <div className={styles.hint}>{t("exportEmpty")}</div>
              ) : (
                <div className={styles.list}>
                  {items.map((item) => (
                    <div key={item.name} className={styles.item}>
                      <Link
                        className={styles.grow}
                        href={exportDownloadUrl(project.id, item.name)}
                        download={item.name}
                      >
                        {item.name}
                      </Link>
                      <span className={styles.muted}>
                        {formatBytes(item.bytes)} · {formatDateTime(item.modifiedAt)}
                      </span>
                      <Button
                        as="a"
                        appearance="subtle"
                        size="small"
                        href={exportDownloadUrl(project.id, item.name)}
                        download={item.name}
                        icon={<ArrowDownload20Regular />}
                        aria-label={`${t("exportDownload")} ${item.name}`}
                      />
                    </div>
                  ))}
                </div>
              )}
              {dir ? <div className={styles.muted}>{`${t("exportDir")}: ${dir}`}</div> : null}
            </div>

            <RadioGroup
              layout="horizontal"
              value={source}
              onChange={(_, data) => setSource(data.value as "remote" | "host")}
            >
              <Radio value="remote" label={t("sideRemote")} />
              <Radio value="host" label={t("sideHost")} />
            </RadioGroup>

            <Field label={t("exportFormat")}>
              <RadioGroup
                layout="horizontal"
                value={format}
                onChange={(_, data) => {
                  const newFormat = data.value as "zip" | "tar.gz";
                  setFormat(newFormat);
                  if (name.trim()) {
                    if (newFormat === "zip" && (name.endsWith(".tar.gz") || name.endsWith(".tgz"))) {
                      setName(name.replace(/\.(tar\.gz|tgz)$/, ".zip"));
                    } else if (newFormat === "tar.gz" && name.endsWith(".zip")) {
                      setName(name.replace(/\.zip$/, ".tar.gz"));
                    }
                  }
                }}
              >
                <Radio
                  value="zip"
                  disabled={hasSymlinks}
                  label={
                    hasSymlinks
                      ? `${t("exportFormatZip")} (${t("exportZipDisabledSymlink")})`
                      : t("exportFormatZip")
                  }
                />
                <Radio
                  value="tar.gz"
                  label={`${t("exportFormatTarGz")} (${t("exportTarGzPreserveHint")})`}
                />
              </RadioGroup>
            </Field>

            {hasSymlinks ? (
              <MessageBar intent="warning">
                <MessageBarBody>{t("exportSymlinkWarning")}</MessageBarBody>
              </MessageBar>
            ) : null}

            <Field label={t("exportPath")} hint={t("exportPathHint")}>
              <Input
                value={path}
                placeholder=".artifacts/app"
                onChange={(_, data) => setPath(data.value)}
              />
            </Field>
            <Field label={t("exportName")} hint={t("exportNameHint")}>
              <Input
                value={name}
                placeholder={`${project.name}-<unix>.${format}`}
                onChange={(_, data) => setName(data.value)}
              />
            </Field>

            {error ? (
              <MessageBar intent="error">
                <MessageBarBody>{error}</MessageBarBody>
              </MessageBar>
            ) : null}
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose}>
              {t("cancel")}
            </Button>
            <Button
              appearance="primary"
              icon={<DocumentArrowDown20Regular />}
              disabled={path.trim().length === 0}
              onClick={() => {
                setError(null);
                const trimmed = name.trim();
                let finalName = trimmed;
                if (finalName) {
                  if (format === "tar.gz" && !finalName.endsWith(".tar.gz") && !finalName.endsWith(".tgz")) {
                    finalName = finalName.endsWith(".zip")
                      ? finalName.replace(/\.zip$/, ".tar.gz")
                      : `${finalName}.tar.gz`;
                  } else if (format === "zip" && !finalName.endsWith(".zip")) {
                    finalName = finalName.endsWith(".tar.gz")
                      ? finalName.replace(/\.tar\.gz$/, ".zip")
                      : `${finalName}.zip`;
                  }
                } else {
                  finalName = `${project.name}-${Math.floor(Date.now() / 1000)}.${format}`;
                }
                onConfirm({ source, path: path.trim(), name: finalName });
              }}
            >
              {t("exportStart")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

