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
  Textarea,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Save } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiError, type Status } from "../api";
import { makeT } from "../i18n/strings";

const useStyles = makeStyles({
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
    minWidth: "560px",
  },
  hint: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    lineHeight: "1.5",
  },
  editor: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    minHeight: "320px",
  },
});

/**
 * Generic editor for the project JSON object (design doc §9 "edit JSON args").
 * POSTs that object, like CLI `tasks save --body`; only entry for per-step batch vars.
 */
export function JsonEditorDialog({
  open,
  title,
  hint,
  initial,
  status,
  onClose,
  onSave,
}: {
  open: boolean;
  title: string;
  hint: string;
  /** Text loaded into the editor on open; empty means new. */
  initial: string;
  status: Status;
  onClose: () => void;
  onSave: (parsed: unknown) => Promise<void>;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);

  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!open) {
      return;
    }
    setError(null);
    setText(initial);
  }, [open, initial]);

  async function save(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await onSave(JSON.parse(text));
      onClose();
    } catch (err) {
      if (err instanceof SyntaxError) {
        setError(t("jsonBadSyntax", { msg: err.message }));
      } else {
        setError(err instanceof ApiError ? err.message : String(err));
      }
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return null;
  }

  return (
    <Dialog open onOpenChange={(_, data) => (!data.open ? onClose() : undefined)}>
      <DialogSurface>
        <DialogBody>
          <DialogTitle>{title}</DialogTitle>
          <DialogContent className={styles.body}>
            <div className={styles.hint}>{hint}</div>
            <Textarea
              className={styles.editor}
              resize="vertical"
              value={text}
              onChange={(_, data) => setText(data.value)}
            />
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
              icon={<Save className="w-4 h-4" />}
              disabled={busy}
              onClick={() => void save()}
            >
              {t("save")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
