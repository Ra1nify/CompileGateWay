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
  Dropdown,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Plus, ArrowDown, ArrowUp, Trash2 } from "lucide-react";
import { useEffect, useId, useState } from "react";
import { ApiError, apiSaveBatch, type Batch, type Project, type Status, type Task } from "../api";
import { makeT } from "../i18n/strings";

const useStyles = makeStyles({
  body: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
  },
  surface: {
    maxWidth: "680px",
    width: "min(680px, 94vw)",
    boxSizing: "border-box",
  },
  grid: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    columnGap: "18px",
    rowGap: "12px",
  },
  field: {
    display: "flex",
    flexDirection: "column",
    rowGap: "4px",
    flex: "0 0 auto",
  },
  wide: {
    flex: "1 1 260px",
    minWidth: 0,
  },
  input: {
    minWidth: "200px",
    width: "100%",
  },
  hint: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  steps: {
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
  },
  stepRow: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
  },
  stepIndex: {
    width: "24px",
    textAlign: "right",
    color: tokens.colorNeutralForeground3,
    fontSize: tokens.fontSizeBase200,
  },
});

/**
 * "Add to batch workflow" (design doc §9): pick an existing batch or create one with this task.
 */
/**
 * Create / edit a batch workflow (design doc §7.3). Steps are ordered task refs; order is execution order.
 */
export function BatchDialog({
  open,
  status,
  projectId,
  tasks,
  batch,
  onClose,
  onSaved,
}: {
  open: boolean;
  status: Status;
  projectId: string;
  tasks: Task[];
  batch: Batch | null;
  onClose: () => void;
  onSaved: () => Promise<void>;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);
  const titleId = useId();

  const [name, setName] = useState("");
  const [steps, setSteps] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    setError(null);
    setBusy(false);
    setName(batch?.name ?? "");
    setSteps((batch?.steps ?? []).map((step) => step.taskId));
  }, [open, batch]);

  if (!open) {
    return null;
  }

  const labelOf = (taskId: string): string => {
    const task = tasks.find((item) => item.id === taskId);
    if (!task) {
      return taskId;
    }
    return task.name || task.kind;
  };

  function move(index: number, delta: number): void {
    setSteps((current) => {
      const next = [...current];
      const target = index + delta;
      if (target < 0 || target >= next.length) {
        return current;
      }
      [next[index], next[target]] = [next[target], next[index]];
      return next;
    });
  }

  async function save(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiSaveBatch(projectId, {
        id: batch?.id ?? "",
        name: name.trim() || t("batches"),
        steps: steps.map((taskId) => ({ taskId })),
      });
      await onSaved();
      onClose();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("saveFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  const canSave = !busy && steps.length > 0;
  const unused = tasks.filter((task) => !steps.includes(task.id));

  return (
    <Dialog
      open
      modalType="modal"
      onOpenChange={(_, data) => {
        if (!data.open && !busy) {
          onClose();
        }
      }}
    >
      <DialogSurface className={styles.surface}>
        <DialogBody>
          <DialogTitle id={titleId}>
            {batch ? t("batchEdit") : t("batchAdd")}
          </DialogTitle>
          <DialogContent className={styles.body}>
            <div className={styles.grid}>
              <Field label={t("batchName")} className={`${styles.field} ${styles.wide}`}>
                <Input
                  className={styles.input}
                  value={name}
                  onChange={(_, data) => setName(data.value)}
                />
              </Field>
            </div>

            <div className={styles.steps}>
              <span className={styles.hint}>{t("batchStepsHint")}</span>
              {steps.length === 0 ? (
                <span className={styles.hint}>{t("emptyBatchSteps")}</span>
              ) : (
                steps.map((taskId, index) => (
                  <div key={`${taskId}-${index}`} className={styles.stepRow}>
                    <span className={styles.stepIndex}>{index + 1}</span>
                    <Badge appearance="tint" color="informative" size="small">
                      {labelOf(taskId)}
                    </Badge>
                    <span className={styles.hint}>{taskId}</span>
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<ArrowUp className="w-3.5 h-3.5" />}
                      aria-label={t("moveUp")}
                      disabled={index === 0}
                      onClick={() => move(index, -1)}
                    />
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<ArrowDown className="w-3.5 h-3.5" />}
                      aria-label={t("moveDown")}
                      disabled={index === steps.length - 1}
                      onClick={() => move(index, 1)}
                    />
                    <Button
                      appearance="subtle"
                      size="small"
                      icon={<Trash2 className="w-3.5 h-3.5" />}
                      aria-label={t("del")}
                      style={{ color: tokens.colorPaletteRedForeground1 }}
                      onClick={() =>
                        setSteps((current) => current.filter((_, at) => at !== index))
                      }
                    />
                  </div>
                ))
              )}

              <div className={styles.stepRow}>
                <Dropdown
                  className={styles.input}
                  placeholder={t("batchAddStep")}
                  selectedOptions={[]}
                  value=""
                  onOptionSelect={(_, data) => {
                    if (data.optionValue) {
                      setSteps((current) => [...current, data.optionValue as string]);
                    }
                  }}
                >
                  {tasks.map((task) => (
                    <Option key={task.id} value={task.id}>
                      {`${task.name || task.kind} · ${task.kind}`}
                    </Option>
                  ))}
                </Dropdown>
                <Button
                  appearance="secondary"
                  size="small"
                  icon={<Plus className="w-4 h-4" />}
                  disabled={unused.length === 0}
                  onClick={() => {
                    if (unused[0]) {
                      setSteps((current) => [...current, unused[0].id]);
                    }
                  }}
                >
                  {t("batchAddStep")}
                </Button>
              </div>
            </div>

            {error ? (
              <MessageBar intent="error">
                <MessageBarBody>{error}</MessageBarBody>
              </MessageBar>
            ) : null}
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose} disabled={busy}>
              {t("cancel")}
            </Button>
            <Button appearance="primary" disabled={!canSave} onClick={() => void save()}>
              {t("save")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}

export function AddToBatchDialog({
  open,
  status,
  project,
  task,
  onClose,
  onSaved,
}: {
  open: boolean;
  status: Status;
  project: Project;
  task: Task;
  onClose: () => void;
  onSaved: () => Promise<void>;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setError(null);
      setBusy(false);
    }
  }, [open]);

  if (!open) {
    return null;
  }

  const batches: Batch[] = project.batches ?? [];
  const label = task.name || task.kind;

  /** Append this step to a batch (leaves its existing steps alone). */
  async function appendTo(batch: Batch): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiSaveBatch(project.id, {
        id: batch.id,
        name: batch.name,
        steps: [...batch.steps.map((step) => ({ taskId: step.taskId })), { taskId: task.id }],
      });
      await onSaved();
      onClose();
    } catch (err) {
      setError(t("saveFailed", { msg: err instanceof ApiError ? err.message : String(err) }));
    } finally {
      setBusy(false);
    }
  }

  async function createNew(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiSaveBatch(project.id, {
        id: "",
        name: `${label} pipeline`,
        steps: [{ taskId: task.id }],
      });
      await onSaved();
      onClose();
    } catch (err) {
      setError(t("saveFailed", { msg: err instanceof ApiError ? err.message : String(err) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open
      modalType="modal"
      onOpenChange={(_, data) => {
        if (!data.open && !busy) {
          onClose();
        }
      }}
    >
      <DialogSurface className={styles.surface}>
        <DialogBody>
          <DialogTitle>{t("batchAddTask")}</DialogTitle>
          <DialogContent className={styles.body}>
            <span className={styles.hint}>
              {t("batchAddTaskHint", { task: label })}
            </span>
            <div className={styles.steps}>
              {batches.length === 0 ? (
                <span className={styles.hint}>{t("emptyBatches")}</span>
              ) : (
                batches.map((batch) => (
                  <div key={batch.id} className={styles.stepRow}>
                    <Button
                      className={styles.input}
                      appearance="secondary"
                      disabled={busy}
                      onClick={() => void appendTo(batch)}
                    >
                      {`${batch.name} · ${t("stepCount", { n: batch.steps.length })}`}
                    </Button>
                  </div>
                ))
              )}
              <div className={styles.stepRow}>
                <Button
                  className={styles.input}
                  appearance="primary"
                  icon={<Plus className="w-4 h-4" />}
                  disabled={busy}
                  onClick={() => void createNew()}
                >
                  {t("batchCreateWithTask")}
                </Button>
              </div>
            </div>
            {error ? (
              <MessageBar intent="error">
                <MessageBarBody>{error}</MessageBarBody>
              </MessageBar>
            ) : null}
          </DialogContent>
          <DialogActions>
            <Button appearance="secondary" onClick={onClose} disabled={busy}>
              {t("cancel")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}