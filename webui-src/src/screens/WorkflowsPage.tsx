import type { JSX } from "react";
import {
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  DialogActions,
  DialogBody,
  DialogSurface,
  DialogTitle,
  Dropdown,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  SearchBox,
  Spinner,
  Textarea,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import {
  Add20Regular,
  ArrowDownload20Regular,
  ArrowSync20Regular,
  BranchFork20Regular,
  Delete20Regular,
  Edit20Regular,
  Play20Regular,
} from "@fluentui/react-icons";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ApiError,
  apiApplyWorkflow,
  apiDeleteWorkflow,
  apiListWorkflows,
  apiResetWorkflows,
  apiSaveWorkflow,
  apiWorkflowRaw,
  apiWorkflowTemplates,
  type Project,
  type Status,
  type WorkflowSummary,
  type WorkflowTemplateItem,
} from "../api";
import { makeT } from "../i18n/strings";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { PageHead } from "../components/Common";

const STARTER_WORKFLOW = `{
  "name": "Custom Pipeline",
  "description": "Custom reusable pipeline template",
  "variables": [
    { "name": "branch", "label": "Git Branch", "default": "main", "required": true }
  ],
  "tasks": [
    {
      "id": "push",
      "title": "Push sources",
      "kind": "push"
    },
    {
      "id": "build",
      "title": "Run build",
      "kind": "build",
      "manifest": "rust-native"
    }
  ],
  "steps": [
    { "id": "step-push", "task": "push" },
    { "id": "step-build", "task": "build" }
  ]
}
`;

const useStyles = makeStyles({
  screen: {
    maxWidth: "1080px",
  },
  toolbar: {
    display: "flex",
    flexWrap: "nowrap",
    alignItems: "center",
    columnGap: "8px",
    flex: "0 0 auto",
  },
  search: {
    minWidth: "200px",
    maxWidth: "320px",
  },
  list: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
    marginTop: "16px",
  },
  card: {
    padding: "16px",
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  cardHeader: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    columnGap: "12px",
    marginBottom: "8px",
  },
  cardTitleRow: {
    display: "flex",
    alignItems: "center",
    flexWrap: "wrap",
    gap: "8px",
  },
  title: {
    fontSize: tokens.fontSizeBase500,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground1,
  },
  cardActions: {
    display: "flex",
    alignItems: "center",
    gap: "6px",
    flexShrink: 0,
  },
  desc: {
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground2,
    marginBottom: "12px",
    lineHeight: "1.5",
  },
  metaGrid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))",
    gap: "12px",
    marginTop: "8px",
    paddingTop: "8px",
    borderTop: `1px solid ${tokens.colorNeutralStroke2}`,
  },
  sectionTitle: {
    fontSize: tokens.fontSizeBase200,
    fontWeight: tokens.fontWeightSemibold,
    color: tokens.colorNeutralForeground3,
    marginBottom: "6px",
    textTransform: "uppercase",
    letterSpacing: "0.5px",
  },
  tagList: {
    display: "flex",
    flexWrap: "wrap",
    gap: "6px",
  },
  stepItem: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground2,
    display: "flex",
    alignItems: "center",
    gap: "6px",
    padding: "2px 0",
  },
  editorSurface: {
    maxWidth: "960px",
    width: "90vw",
  },
  editorBody: {
    padding: "16px 24px",
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
  },
  editorTextarea: {
    width: "100%",
    "& textarea": {
      fontFamily: tokens.fontFamilyMonospace,
      fontSize: "12px",
      lineHeight: "1.5",
      minHeight: "420px",
    },
  },
  applySurface: {
    maxWidth: "560px",
    width: "90vw",
  },
  applyBody: {
    padding: "16px 24px",
    display: "flex",
    flexDirection: "column",
    rowGap: "14px",
  },
  empty: {
    padding: "24px 0",
    textAlign: "center",
    color: tokens.colorNeutralForeground3,
  },
});

export function WorkflowsPage({
  status,
  projects,
  onOpenProject,
}: {
  status: Status;
  projects: Project[];
  onOpenProject?: (projectId: string) => void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [workflows, setWorkflows] = useState<WorkflowSummary[]>([]);
  const [missingTemplates, setMissingTemplates] = useState<WorkflowTemplateItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [errorText, setErrorText] = useState<string | null>(null);
  const [successText, setSuccessText] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  // Editor modal state
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingRaw, setEditingRaw] = useState("");
  const [editorBusy, setEditorBusy] = useState(false);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [isNewWorkflow, setIsNewWorkflow] = useState(false);
  const [newId, setNewId] = useState("");

  // Delete modal state
  const [deletingId, setDeletingId] = useState<string | null>(null);

  // Apply modal state
  const [applyingWorkflow, setApplyingWorkflow] = useState<WorkflowSummary | null>(null);

  const loadData = useCallback(async () => {
    setLoading(true);
    setErrorText(null);
    try {
      const [listRes, tmplRes] = await Promise.all([
        apiListWorkflows(),
        apiWorkflowTemplates(),
      ]);
      setWorkflows(listRes.workflows);
      setMissingTemplates(tmplRes.missing);
    } catch (err) {
      setErrorText(err instanceof ApiError ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadData();
  }, [loadData]);

  async function handleOpenEdit(id: string) {
    setEditingRaw("");
    setEditorBusy(true);
    setEditorError(null);
    setIsNewWorkflow(false);
    setEditingId(id);
    try {
      const res = await apiWorkflowRaw(id);
      setEditingRaw(res.raw);
    } catch (err) {
      setEditorError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setEditorBusy(false);
    }
  }

  function handleOpenNew() {
    setIsNewWorkflow(true);
    setEditingId(null);
    setNewId("");
    setEditingRaw(STARTER_WORKFLOW);
    setEditorError(null);
  }

  async function handleSaveWorkflow() {
    setEditorBusy(true);
    setEditorError(null);
    const targetId = isNewWorkflow ? newId.trim() : editingId;
    if (!targetId) {
      setEditorError("Workflow ID is required");
      setEditorBusy(false);
      return;
    }
    try {
      await apiSaveWorkflow(targetId, editingRaw);
      setEditingId(null);
      setIsNewWorkflow(false);
      setSuccessText(t("saved"));
      await loadData();
    } catch (err) {
      setEditorError(err instanceof ApiError ? err.message : String(err));
    } finally {
      setEditorBusy(false);
    }
  }

  async function handleDeleteWorkflow() {
    if (!deletingId) return;
    try {
      await apiDeleteWorkflow(deletingId);
      setDeletingId(null);
      setSuccessText(t("saved"));
      await loadData();
    } catch (err) {
      setErrorText(err instanceof ApiError ? err.message : String(err));
    }
  }

  async function handleRestoreTemplates() {
    setErrorText(null);
    try {
      await apiResetWorkflows();
      setSuccessText(t("saved"));
      await loadData();
    } catch (err) {
      setErrorText(err instanceof ApiError ? err.message : String(err));
    }
  }

  const filteredWorkflows = useMemo(
    () =>
      workflows.filter((w) => {
        const q = query.trim().toLowerCase();
        if (!q) return true;
        return (
          w.name.toLowerCase().includes(q) ||
          w.id.toLowerCase().includes(q) ||
          (w.description && w.description.toLowerCase().includes(q))
        );
      }),
    [workflows, query],
  );

  return (
    <div className={styles.screen}>
      <PageHead
        title={t("workflowsTitle")}
        subtitle={t("workflowsSubtitle")}
        actions={
          <div className={styles.toolbar}>
            <SearchBox
              className={styles.search}
              placeholder={t("searchHint")}
              value={query}
              onChange={(_, d) => setQuery(d.value)}
            />
            {missingTemplates.length > 0 && (
              <Tooltip content={t("restoreWorkflows", { n: missingTemplates.length })} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  icon={<ArrowDownload20Regular />}
                  onClick={() => void handleRestoreTemplates()}
                >
                  {t("restoreWorkflows", { n: missingTemplates.length })}
                </Button>
              </Tooltip>
            )}
            <Tooltip content={t("newWorkflow")} relationship="description" withArrow>
              <Button
                appearance="primary"
                icon={<Add20Regular />}
                onClick={handleOpenNew}
              >
                {t("newWorkflow")}
              </Button>
            </Tooltip>
            <Tooltip content={t("refresh")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<ArrowSync20Regular />}
                onClick={() => { setSuccessText(null); void loadData(); }}
              />
            </Tooltip>
          </div>
        }
      />

      {errorText && (
        <MessageBar intent="error" style={{ marginBottom: "12px" }}>
          <MessageBarBody>{errorText}</MessageBarBody>
        </MessageBar>
      )}

      {successText && (
        <MessageBar intent="success" style={{ marginBottom: "12px" }}>
          <MessageBarBody>{successText}</MessageBarBody>
        </MessageBar>
      )}

      {loading ? (
        <div className={styles.empty}>
          <Spinner label={t("refresh")} />
        </div>
      ) : filteredWorkflows.length === 0 ? (
        <p className={styles.empty}>{t("emptyWorkflows")}</p>
      ) : (
        <div className={styles.list}>
          {filteredWorkflows.map((wf) => (
            <Card key={wf.id} className={styles.card}>
              <div className={styles.cardHeader}>
                <div className={styles.cardTitleRow}>
                  <BranchFork20Regular />
                  <span className={styles.title}>{wf.name}</span>
                  <Badge appearance="outline" color="informative" size="small">
                    {wf.id}
                  </Badge>
                  {wf.template ? (
                    <Badge appearance="tint" color="brand" size="small">
                      {t("workflowBuiltin")}
                    </Badge>
                  ) : (
                    <Badge appearance="tint" color="informative" size="small">
                      {t("workflowCustom")}
                    </Badge>
                  )}
                  <Badge appearance="tint" color="subtle" size="small">
                    {t("workflowStepsCount", { n: wf.steps.length })}
                  </Badge>
                  {wf.variables.length > 0 && (
                    <Badge appearance="tint" color="subtle" size="small">
                      {t("workflowVarsCount", { n: wf.variables.length })}
                    </Badge>
                  )}
                  {!wf.valid && (
                    <Badge appearance="tint" color="danger" size="small">
                      {wf.errors.length} error(s)
                    </Badge>
                  )}
                </div>
                <div className={styles.cardActions}>
                  <Button
                    appearance="primary"
                    icon={<Play20Regular />}
                    disabled={!wf.valid || projects.length === 0}
                    onClick={() => setApplyingWorkflow(wf)}
                  >
                    {t("applyToProject")}
                  </Button>
                  <Tooltip content={t("edit")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Edit20Regular />}
                      onClick={() => void handleOpenEdit(wf.id)}
                    />
                  </Tooltip>
                  <Tooltip content={t("del")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Delete20Regular />}
                      onClick={() => setDeletingId(wf.id)}
                    />
                  </Tooltip>
                </div>
              </div>

              {wf.description && <div className={styles.desc}>{wf.description}</div>}

              <div className={styles.metaGrid}>
                {wf.variables.length > 0 && (
                  <div>
                    <div className={styles.sectionTitle}>{t("workflowVariables")}</div>
                    <div className={styles.tagList}>
                      {wf.variables.map((v) => (
                        <Badge key={v.name} appearance="outline" size="small">
                          {v.label || v.name}
                          {v.default ? ` = "${v.default}"` : ""}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}
                <div>
                  <div className={styles.sectionTitle}>{t("workflowStepsPreview")}</div>
                  {wf.steps.map((st, idx) => (
                    <div key={st.id || idx} className={styles.stepItem}>
                      <span>{idx + 1}.</span>
                      <strong>{st.name || st.task}</strong>
                      <span style={{ color: tokens.colorNeutralForeground4 }}>({st.task})</span>
                    </div>
                  ))}
                </div>
              </div>
            </Card>
          ))}
        </div>
      )}

      {/* Workflow JSON Editor Dialog */}
      <Dialog
        open={editingId !== null || isNewWorkflow}
        onOpenChange={(_, d) => {
          if (!d.open) {
            setEditingId(null);
            setIsNewWorkflow(false);
          }
        }}
      >
        <DialogSurface className={styles.editorSurface}>
          <DialogTitle>
            {isNewWorkflow ? t("newWorkflow") : `${t("edit")}: ${editingId}`}
          </DialogTitle>
          <DialogBody className={styles.editorBody}>
            {editorError && (
              <MessageBar intent="error">
                <MessageBarBody>{editorError}</MessageBarBody>
              </MessageBar>
            )}

            {isNewWorkflow && (
              <Field label="Workflow ID" required>
                <Input
                  value={newId}
                  placeholder="e.g. build-deploy"
                  onChange={(_, d) => setNewId(d.value)}
                />
              </Field>
            )}

            <Field label="JSON Definition" required>
              <Textarea
                className={styles.editorTextarea}
                value={editingRaw}
                onChange={(_, d) => setEditingRaw(d.value)}
              />
            </Field>

            <DialogActions>
              <Button
                appearance="secondary"
                disabled={editorBusy}
                onClick={() => {
                  setEditingId(null);
                  setIsNewWorkflow(false);
                }}
              >
                {t("cancel")}
              </Button>
              <Button
                appearance="primary"
                disabled={editorBusy}
                onClick={() => void handleSaveWorkflow()}
              >
                {t("save")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* Delete Confirmation Dialog */}
      <ConfirmDialog
        open={deletingId !== null}
        title={t("del")}
        message={deletingId ? t("deleteWorkflowConfirm", { name: deletingId }) : ""}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setDeletingId(null)}
        onConfirm={() => void handleDeleteWorkflow()}
      />

      {/* Apply Workflow to Project Dialog */}
      {applyingWorkflow && (
        <ApplyWorkflowModal
          status={status}
          workflow={applyingWorkflow}
          projects={projects}
          onClose={() => setApplyingWorkflow(null)}
          onSuccess={(projectId) => {
            setApplyingWorkflow(null);
            setSuccessText(
              t("applySuccess", {
                project: projects.find((p) => p.id === projectId)?.name || projectId,
              })
            );
            if (onOpenProject) {
              onOpenProject(projectId);
            }
          }}
        />
      )}
    </div>
  );
}

function ApplyWorkflowModal({
  status,
  workflow,
  projects,
  onClose,
  onSuccess,
}: {
  status: Status;
  workflow: WorkflowSummary;
  projects: Project[];
  onClose: () => void;
  onSuccess: (projectId: string) => void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [selectedProjectId, setSelectedProjectId] = useState<string>(
    projects[0]?.id || ""
  );
  const [batchName, setBatchName] = useState(workflow.name);
  const [overwrite, setOverwrite] = useState(true);
  const [runImmediately, setRunImmediately] = useState(false);
  const [overrides, setOverrides] = useState<Record<string, string>>(() => {
    const init: Record<string, string> = {};
    for (const v of workflow.variables) {
      init[v.name] = v.default || "";
    }
    return init;
  });

  const [busy, setBusy] = useState(false);
  const [errorText, setErrorText] = useState<string | null>(null);

  async function handleApply() {
    if (!selectedProjectId) {
      setErrorText("Please select a target project");
      return;
    }
    setBusy(true);
    setErrorText(null);
    try {
      await apiApplyWorkflow(selectedProjectId, workflow.id, {
        overrides,
        batchName: batchName.trim() || undefined,
        overwrite,
        run: runImmediately,
      });
      onSuccess(selectedProjectId);
    } catch (err) {
      setErrorText(err instanceof ApiError ? err.message : String(err));
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(_, d) => !d.open && onClose()}>
      <DialogSurface className={styles.applySurface}>
        <DialogTitle>{t("applyWorkflowTitle", { name: workflow.name })}</DialogTitle>
        <DialogBody className={styles.applyBody}>
          {errorText && (
            <MessageBar intent="error">
              <MessageBarBody>{errorText}</MessageBarBody>
            </MessageBar>
          )}

          <Field label={t("applySelectProject")} required>
            <Dropdown
              value={
                projects.find((p) => p.id === selectedProjectId)?.name ||
                t("applySelectProject")
              }
              selectedOptions={[selectedProjectId]}
              onOptionSelect={(_, d) => {
                if (d.optionValue) setSelectedProjectId(d.optionValue);
              }}
            >
              {projects.map((p) => (
                <Option key={p.id} value={p.id} text={`${p.name} (${p.id})`}>
                  {p.name} ({p.id})
                </Option>
              ))}
            </Dropdown>
          </Field>

          <Field label={t("applyBatchName")} hint={t("applyBatchNameHint")}>
            <Input
              value={batchName}
              onChange={(_, d) => setBatchName(d.value)}
            />
          </Field>

          {workflow.variables.length > 0 ? (
            <div>
              <div className={styles.sectionTitle}>{t("workflowVariables")}</div>
              {workflow.variables.map((v) => (
                <Field
                  key={v.name}
                  label={v.label || v.name}
                  hint={v.description}
                  required={v.required}
                  style={{ marginBottom: "8px" }}
                >
                  <Input
                    value={overrides[v.name] || ""}
                    onChange={(_, d) =>
                      setOverrides((prev) => ({ ...prev, [v.name]: d.value }))
                    }
                  />
                </Field>
              ))}
            </div>
          ) : (
            <div style={{ color: tokens.colorNeutralForeground3, fontSize: "12px" }}>
              {t("workflowNoVars")}
            </div>
          )}

          <div style={{ display: "flex", flexDirection: "column", gap: "8px", marginTop: "4px" }}>
            <Checkbox
              checked={overwrite}
              label={t("applyOverwrite")}
              onChange={(_, d) => setOverwrite(Boolean(d.checked))}
            />
            <Checkbox
              checked={runImmediately}
              label={t("applyRun")}
              onChange={(_, d) => setRunImmediately(Boolean(d.checked))}
            />
          </div>

          <DialogActions>
            <Button appearance="secondary" disabled={busy} onClick={onClose}>
              {t("cancel")}
            </Button>
            <Button
              appearance="primary"
              disabled={busy || !selectedProjectId || workflow.variables.some(v => v.required && !v.default && !overrides[v.name]?.trim())}
              onClick={() => void handleApply()}
            >
              {t("applyToProject")}
            </Button>
          </DialogActions>
        </DialogBody>
      </DialogSurface>
    </Dialog>
  );
}
