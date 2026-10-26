import type { ReactNode } from "react";
import {
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { useId, useState } from "react";

const useStyles = makeStyles({
  surface: {
    maxWidth: "min(480px, 92vw)",
    boxSizing: "border-box",
  },
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
  },
  extra: {
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
  },
  danger: {
    backgroundColor: tokens.colorPaletteRedBackground3,
  },
});

export interface ConfirmExtraState {
  /** State of the "clean remote dir first" checkbox; defaults to false. */
  clean: boolean;
  setClean: (value: boolean) => void;
}

/**
 * Confirm dialog. Replaces window.confirm(): keyboard accessible, confirm focused by default.
 */
export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  cancelLabel,
  danger = false,
  busy = false,
  extra,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  message?: ReactNode;
  confirmLabel: string;
  cancelLabel: string;
  danger?: boolean;
  busy?: boolean;
  extra?: (state: ConfirmExtraState) => ReactNode;
  onConfirm: (state: { clean: boolean }) => void;
  onCancel: () => void;
}): ReactNode {
  const styles = useStyles();
  const [clean, setClean] = useState(false);
  const titleId = useId();

  if (!open) {
    return null;
  }

  return (
    <Dialog
      open
      modalType="modal"
      onOpenChange={(_, data) => {
        if (!data.open && !busy) {
          onCancel();
        }
      }}
    >
      <DialogSurface className={styles.surface} aria-labelledby={titleId}>
        <DialogBody>
          <DialogTitle id={titleId}>{title}</DialogTitle>
          <DialogContent className={styles.body}>
            {message ? <div>{message}</div> : null}
            {extra ? (
              <div className={styles.extra}>
                {extra({
                  clean,
                  setClean: (value: boolean) => setClean(value),
                })}
              </div>
            ) : null}
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onCancel} disabled={busy}>
              {cancelLabel}
            </Button>
            <Button
              appearance="primary"
              className={danger ? styles.danger : undefined}
              onClick={() => onConfirm({ clean })}
              disabled={busy}
            >
              {confirmLabel}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

/** The "clear remote dir before push" checkbox used in ConfirmDialog's extra. */
export function CleanCheckbox({
  label,
  state,
}: {
  label: string;
  state: ConfirmExtraState;
}): ReactNode {
  const id = useId();
  return (
    <Checkbox
      id={id}
      checked={state.clean}
      label={label}
      onChange={(_, data) => state.setClean(data.checked === true)}
    />
  );
}
