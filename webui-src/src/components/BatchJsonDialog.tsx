import type { JSX } from "react";
import { apiSaveBatch, type Batch, type Project, type Status } from "../api";
import { makeT } from "../i18n/strings";
import { JsonEditorDialog } from "./JsonEditorDialog";

/** Editable skeleton for a new batch workflow. */
const NEW_BATCH = `{
  "id": "",
  "name": "",
  "steps": [
    { "taskId": "" }
  ]
}`;

/**
 * JSON args editor for a batch workflow (design doc §7.3 / §9).
 * Only entry that can write per-step variables; previously CLI `cgw batch save --body` only.
 */
export function BatchJsonDialog({
  open,
  project,
  batch,
  status,
  onClose,
  onSaved,
}: {
  open: boolean;
  project: Project | null;
  batch: Batch | null;
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
      title={batch ? t("batchJsonEdit") : t("batchJsonNew")}
      hint={t("batchJsonHint")}
      initial={batch ? JSON.stringify(batch, null, 2) : NEW_BATCH}
      status={status}
      onClose={onClose}
      onSave={async (parsed) => {
        await apiSaveBatch(project.id, parsed as Batch);
        onSaved();
      }}
    />
  );
}
