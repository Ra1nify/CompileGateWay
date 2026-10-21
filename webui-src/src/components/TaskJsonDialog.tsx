import type { JSX } from "react";
import { apiSaveTask, type Project, type Status, type Task } from "../api";
import { makeT } from "../i18n/strings";
import { JsonEditorDialog } from "./JsonEditorDialog";

/** Editable skeleton for a new task (defaults to the commonest kind, copy). */
const NEW_TASK = `{
  "kind": "copy",
  "name": "",
  "from": { "side": "remote", "path": "" },
  "to": { "side": "host", "path": "" }
}`;

/**
 * JSON args editor for a task (design doc §9). POSTs the task object, like `cgw tasks save --body`.
 */
export function TaskJsonDialog({
  open,
  project,
  task,
  status,
  onClose,
  onSaved,
}: {
  open: boolean;
  project: Project | null;
  task: Task | null;
  status: Status;
  onClose: () => void;
  onSaved: () => void;
}): JSX.Element | null {
  const t = makeT(status.language);
  if (!open || !project) {
    return null;
  }

  return (
    <JsonEditorDialog
      open={open}
      title={task ? t("taskJsonEdit") : t("taskJsonNew")}
      hint={t("taskJsonHint")}
      initial={task ? JSON.stringify(task, null, 2) : NEW_TASK}
      status={status}
      onClose={onClose}
      onSave={async (parsed) => {
        await apiSaveTask(project.id, parsed as Task);
        onSaved();
      }}
    />
  );
}
