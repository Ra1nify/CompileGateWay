import type { JSX } from "react";
import {
  Button,
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
import { ArrowUp20Regular, Folder20Regular, Home20Regular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import { ApiError, apiBrowse, type BrowseResult, type Status } from "../api";
import { makeT } from "../i18n/strings";

const useStyles = makeStyles({
  surface: {
    maxWidth: "700px",
    width: "700px",
  },
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "10px",
  },
  toolbar: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    flexWrap: "wrap",
  },
  crumb: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
    wordBreak: "break-all",
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
    textAlign: "left",
    justifyContent: "flex-start",
  },
  empty: {
    margin: 0,
    padding: "14px 10px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  hint: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
});

/**
 * Folder picker (design doc §6.1). Deliberately avoids native pickers: the WebUI may run on
 * another device, so we ask the daemon via `GET /api/browse?path=...` (home dir only).
 */
export function FolderPickerDialog({
  open,
  status,
  startPath,
  onPick,
  onClose,
}: {
  open: boolean;
  status: Status;
  /** Where to start on open (default = daemon user home). */
  startPath?: string;
  onPick: (absolutePath: string) => void;
  onClose: () => void;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);

  const [listing, setListing] = useState<BrowseResult | null>(null);
  const [path, setPath] = useState(startPath ?? "");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setPath(startPath ?? "");
      setError(null);
    }
  }, [open, startPath]);

  useEffect(() => {
    if (!open) {
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    (async () => {
      try {
        const result = await apiBrowse(path);
        if (!cancelled) {
          setListing(result);
        }
      } catch (err) {
        if (!cancelled) {
          setListing(null);
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
  }, [open, path]);

  if (!open) {
    return null;
  }

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
          <DialogTitle>{t("browseTitle")}</DialogTitle>
          <DialogContent className={styles.body}>
            <div className={styles.toolbar}>
              <Tooltip content={t("browseHome")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  icon={<Home20Regular />}
                  aria-label={t("browseHome")}
                  disabled={!listing}
                  onClick={() => setPath(listing?.root ?? "")}
                />
              </Tooltip>
              <Tooltip content={t("treeUp")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  icon={<ArrowUp20Regular />}
                  aria-label={t("treeUp")}
                  disabled={!listing?.parent}
                  onClick={() => setPath(listing?.parent ?? "")}
                />
              </Tooltip>
              <span className={styles.crumb}>{listing?.path ?? t("browseLoading")}</span>
              {loading ? <Spinner size="tiny" /> : null}
            </div>
            <span className={styles.hint}>
              {t("browseHint", { root: listing?.root ?? "~" })}
            </span>

            {error ? (
              <MessageBar intent="warning">
                <MessageBarBody>{error}</MessageBarBody>
              </MessageBar>
            ) : null}

            <div className={styles.list}>
              {(listing?.entries ?? []).length === 0 && !loading ? (
                <p className={styles.empty}>{t("browseNoSubdirs")}</p>
              ) : null}
              {(listing?.entries ?? []).map((entry) => (
                <div key={entry.path} className={styles.row}>
                  <Button
                    className={styles.name}
                    appearance="subtle"
                    size="small"
                    icon={<Folder20Regular />}
                    onClick={() => setPath(entry.path)}
                  >
                    {entry.name}
                  </Button>
                </div>
              ))}
            </div>
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose}>
              {t("cancel")}
            </Button>
            <Tooltip content={t("browseUseThis")} relationship="description" withArrow>
              <Button
                appearance="primary"
                disabled={!listing}
                onClick={() => {
                  if (listing) {
                    onPick(listing.path);
                    onClose();
                  }
                }}
              >
                {t("browseUseThis")}
              </Button>
            </Tooltip>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
