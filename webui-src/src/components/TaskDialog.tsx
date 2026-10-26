import type { JSX } from "react";
import {
  Button,
  Checkbox,
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
  Spinner,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { FolderOpen } from "lucide-react";
import { useEffect, useId, useState } from "react";
import {
  ApiError,
  apiGetEnv,
  apiListManifests,
  apiSaveTask,
  type Location,
  type ManifestSummary,
  type PathSide,
  type SigningMode,
  type Status,
  type Task,
  type TaskKind,
  type XcodeInstance,
} from "../api";
import { makeT } from "../i18n/strings";
import { TreeDialog } from "./TreeDialog";

/** `sources` entries are keyed by index; everything else by field name. */
type LocationTarget = "from" | "to" | "target" | number;

const useStyles = makeStyles({
  pathRow: {
    display: "flex",
    alignItems: "center",
    columnGap: "6px",
  },
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
  sources: {
    display: "flex",
    flexDirection: "column",
    rowGap: "8px",
  },
  sourceRow: {
    display: "flex",
    alignItems: "flex-end",
    columnGap: "8px",
  },
});

/** All creatable task kinds, in dropdown order. */
export const TASK_KINDS: TaskKind[] = [
  "build",
  "push",
  "pull",
  "copy",
  "move",
  "delete",
  "export",
];

/** Empty shell for a new task (design doc §7.1/§7.2). */
export function emptyTask(kind: TaskKind): Task {
  const remote = (path: string): Location => ({ side: "remote", path });
  const base: Task = { id: "", kind, name: "" };
  switch (kind) {
    case "build":
      return {
        ...base,
        manifest: "",
        variables: {},
        signingMode: "signed",
        artifacts: [],
        artifactPath: "",
      };
    case "push":
      return { ...base, clean: false };
    case "copy":
    case "move":
      return { ...base, from: remote(""), to: remote(""), overwrite: false };
    case "delete":
      return { ...base, target: remote("") };
    case "pull":
      return {
        ...base,
        sources: [remote("")],
        to: { side: "host", path: ".artifacts" },
        overwrite: true,
      };
    default:
      return { ...base, sources: [remote("")], outputName: "export.zip" };
  }
}

/**
 * Create / edit a project task (design doc §7.1). One dialog for all seven kinds:
 * pick the kind, then only its fields show.
 */
export function TaskDialog({
  open,
  status,
  clientId,
  projectId,
  projectName,
  task,
  onClose,
  onSaved,
}: {
  open: boolean;
  status: Status;
  /** Used to read this target's env-detect cache (Xcode picker). */
  clientId: string;
  projectId: string;
  projectName: string;
  task: Task | null;
  onClose: () => void;
  onSaved: () => Promise<void>;
}): JSX.Element | null {
  const styles = useStyles();
  const t = makeT(status.language);
  const titleId = useId();

  const [manifests, setManifests] = useState<ManifestSummary[]>([]);
  const [loadingManifests, setLoadingManifests] = useState(false);
  const [xcodes, setXcodes] = useState<XcodeInstance[]>([]);
  const [envChecked, setEnvChecked] = useState(false);
  const [kind, setKind] = useState<TaskKind>("build");
  const [name, setName] = useState("");
  const [manifestId, setManifestId] = useState("");
  const [variables, setVariables] = useState<Record<string, string>>({});
  const [signingMode, setSigningMode] = useState<SigningMode>("signed");
  const [artifactPath, setArtifactPath] = useState("");
  const [artifacts, setArtifacts] = useState("");
  const [xcodePath, setXcodePath] = useState("");
  const [clean, setClean] = useState(false);  const [from, setFrom] = useState<Location>({ side: "remote", path: "" });
  const [to, setTo] = useState<Location>({ side: "remote", path: "" });
  const [target, setTarget] = useState<Location>({ side: "remote", path: "" });
  const [sources, setSources] = useState<Location[]>([]);
  const [overwrite, setOverwrite] = useState(false);
  const [outputName, setOutputName] = useState("");
  /**
   * "Pick a path from the file tree": `target` is the field, `current` its present value.
   */
  const [picking, setPicking] = useState<{ target: LocationTarget; current: Location } | null>(
    null,
  );

  /** Write the picked path back to the matching field. */
  function applyPicked(side: PathSide, path: string): void {
    if (!picking) {
      return;
    }
    const next: Location = { side, path };
    if (picking.target === "from") {
      setFrom(next);
    } else if (picking.target === "to") {
      setTo(next);
    } else if (picking.target === "target") {
      setTarget(next);
    } else {
      const index = picking.target;
      setSources((current) => current.map((item, at) => (at === index ? next : item)));
    }
    setPicking(null);
  }
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // On open: load the manifest list (needed by build tasks) + read the env cache (Xcode).
  useEffect(() => {
    if (!open) {
      return;
    }
    let cancelled = false;
    setLoadingManifests(true);
    setError(null);
    (async () => {
      try {
        const result = await apiListManifests();
        if (!cancelled) {
          setManifests(result.manifests ?? []);
        }
      } catch (err) {
        if (!cancelled) {
          const message = err instanceof ApiError ? err.message : String(err);
          setError(t("loadFailed", { msg: message }));
          setManifests([]);
        }
      } finally {
        if (!cancelled) {
          setLoadingManifests(false);
        }
      }
    })();

    // Env may never have been detected; then only "global selection" is offered, not an error.
    (async () => {
      try {
        const result = await apiGetEnv(clientId);
        if (!cancelled) {
          setXcodes(result.report?.xcodes ?? []);
          setEnvChecked(result.report !== null);
        }
      } catch {
        if (!cancelled) {
          setXcodes([]);
          setEnvChecked(false);
        }
      }
    })();

    return () => {
      cancelled = true;
    };
    // t follows the language, but changing language should not refetch the lists
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, clientId]);

  // Seed the draft fields when opening / switching task.
  useEffect(() => {
    if (!open) {
      return;
    }
    setError(null);
    setBusy(false);

    const draft = task ?? emptyTask("build");
    setKind(draft.kind);
    setName(draft.name ?? "");
    setManifestId(draft.manifest ?? "");
    setVariables({ ...(draft.variables ?? {}) });
    setSigningMode(draft.signingMode === "unsigned" ? "unsigned" : "signed");
    setArtifactPath(draft.artifactPath ?? "");
    setArtifacts((draft.artifacts ?? []).join("\n"));
    setXcodePath(draft.xcodePath ?? "");
    setClean(draft.clean === true);
    setFrom(draft.from ?? { side: "remote", path: "" });
    setTo(draft.to ?? { side: "remote", path: "" });
    setTarget(draft.target ?? { side: "remote", path: "" });
    setSources(draft.sources ?? []);
    setOverwrite(draft.overwrite === true);
    setOutputName(draft.outputName ?? "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, task]);

  if (!open) {
    return null;
  }

  const selected = manifests.find((item) => item.id === manifestId) ?? null;

  function pickManifest(next: ManifestSummary): void {
    setManifestId(next.id);
    const seeded: Record<string, string> = {};
    for (const variable of next.variables ?? []) {
      seeded[variable.name] = variables[variable.name] ?? variable.default ?? "";
    }
    setVariables(seeded);
  }

  function changeKind(next: TaskKind): void {
    setKind(next);
    // On kind change, fill the minimal structure that kind needs; keep existing fields
    if ((next === "copy" || next === "move") && !from.path && !to.path) {
      setFrom({ side: "remote", path: "" });
      setTo({ side: "remote", path: "" });
    }
    if (next === "delete" && !target.path) {
      setTarget({ side: "remote", path: "" });
    }
    if ((next === "pull" || next === "export") && sources.length === 0) {
      setSources([{ side: "remote", path: "" }]);
    }
    if (next === "pull" && !to.path) {
      setTo({ side: "host", path: ".artifacts" });
    }
  }

  function buildPayload(): Task {
    const base: Task = {
      id: task?.id ?? "",
      kind,
      name: name.trim(),
    };
    switch (kind) {
      case "build":
        return {
          ...base,
          manifest: manifestId,
          variables,
          signingMode,
          artifactPath: artifactPath.trim(),
          artifacts: artifacts
            .split("\n")
            .map((line) => line.trim())
            .filter(Boolean),
          xcodePath: xcodePath.trim(),
        };
      case "push":
        return { ...base, clean };
      case "copy":
      case "move":
        return { ...base, from, to, overwrite };
      case "delete":
        return { ...base, target };
      case "pull":
        return { ...base, sources, to, overwrite };
      default:
        return {
          ...base,
          sources,
          outputName: outputName.trim() || "export.zip",
        };
    }
  }

  async function save(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiSaveTask(projectId, buildPayload());
      await onSaved();
      onClose();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("saveFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  const sourceFieldsFillable =
    (kind === "pull" || kind === "export") &&
    sources.length > 0 &&
    sources.every((item) => item.path.trim() !== "");

  const canSave =
    !busy &&
    (kind === "build"
      ? manifestId !== ""
      : kind === "copy" || kind === "move"
        ? from.path.trim() !== "" && to.path.trim() !== ""
        : kind === "delete"
          ? target.path.trim() !== ""
          : sourceFieldsFillable);

  function locationFields(
    target: LocationTarget,
    label: string,
    value: Location,
    onChange: (next: Location) => void,
  ): JSX.Element {
    return (
      <div className={styles.grid} key={label}>
        <Field label={label} className={styles.field}>
          <Dropdown
            className={styles.input}
            selectedOptions={[value.side]}
            value={value.side === "remote" ? t("sideRemote") : t("sideHost")}
            onOptionSelect={(_, data) => {
              const side = data.optionValue as PathSide | undefined;
              if (side) {
                onChange({ ...value, side });
              }
            }}
          >
            <Option value="remote">{t("sideRemote")}</Option>
            <Option value="host">{t("sideHost")}</Option>
          </Dropdown>
        </Field>
        <Field label={t("path")} className={`${styles.field} ${styles.wide}`} hint={t("pathHint")}>
          <div className={styles.pathRow}>
            <Input
              className={styles.input}
              value={value.path}
              onChange={(_, data) => onChange({ ...value, path: data.value })}
            />
            {/* Design doc §7.4 / §9: this field takes a relative path from the file tree */}
            <Tooltip content={t("taskPickPathHint")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<FolderOpen className="w-4 h-4" />}
                aria-label={t("taskPickPath")}
                disabled={!projectId}
                onClick={() => setPicking({ target, current: value })}
              />
            </Tooltip>
          </div>
        </Field>
      </div>
    );
  }

  return (
    <>
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
            {task ? t("buildEdit") : t("buildAdd")}
            <span className={styles.hint}>{` · ${projectName}`}</span>
          </DialogTitle>
          <DialogContent className={styles.body}>
            <div className={styles.grid}>
              <Field label={t("taskKind")} className={styles.field}>
                <Dropdown
                  className={styles.input}
                  selectedOptions={[kind]}
                  value={t(`jobKind${kind.charAt(0).toUpperCase()}${kind.slice(1)}` as never)}
                  onOptionSelect={(_, data) => {
                    const next = data.optionValue as TaskKind | undefined;
                    if (next) {
                      changeKind(next);
                    }
                  }}
                >
                  {TASK_KINDS.map((item) => (
                    <Option key={item} value={item}>
                      {t(`jobKind${item.charAt(0).toUpperCase()}${item.slice(1)}` as never)}
                    </Option>
                  ))}
                </Dropdown>
              </Field>

              <Field label={t("buildName")} className={`${styles.field} ${styles.wide}`}>
                <Input
                  className={styles.input}
                  value={name}
                  onChange={(_, data) => setName(data.value)}
                />
              </Field>
            </div>

            {kind === "build" ? (
              <>
                <Field label={t("buildManifest")}>
                  {loadingManifests ? (
                    <Spinner size="tiny" label={t("buildManifest")} />
                  ) : (
                    <Dropdown
                      className={styles.input}
                      selectedOptions={manifestId ? [manifestId] : []}
                      value={selected ? selected.displayName : ""}
                      placeholder={t("buildManifest")}
                      onOptionSelect={(_, data) => {
                        const next = manifests.find((item) => item.id === data.optionValue);
                        if (next && next.valid) {
                          pickManifest(next);
                        }
                      }}
                    >
                      {manifests.map((item) => (
                        <Option
                          key={item.id}
                          value={item.id}
                          disabled={!item.valid}
                          text={item.displayName}
                        >
                          {item.valid
                            ? item.displayName
                            : `${item.displayName} — ${item.errors.join("; ")}`}
                        </Option>
                      ))}
                    </Dropdown>
                  )}
                </Field>

                {selected?.variables?.length ? (
                  <div className={styles.grid}>
                    {selected.variables.map((variable) => (
                      <Field
                        key={variable.name}
                        label={variable.required ? `${variable.label} *` : variable.label}
                        className={styles.field}
                      >
                        <Input
                          className={styles.input}
                          value={variables[variable.name] ?? ""}
                          placeholder={variable.default ?? ""}
                          onChange={(_, data) =>
                            setVariables((current) => ({
                              ...current,
                              [variable.name]: data.value,
                            }))
                          }
                        />
                      </Field>
                    ))}
                  </div>
                ) : null}

                <div className={styles.grid}>
                  <Field label={t("buildSigning")} className={styles.field}>
                    <Dropdown
                      selectedOptions={[signingMode]}
                      value={signingMode === "signed" ? t("signingSigned") : t("signingUnsigned")}
                      onOptionSelect={(_, data) => {
                        const next = data.optionValue;
                        if (next === "signed" || next === "unsigned") {
                          setSigningMode(next);
                        }
                      }}
                    >
                      <Option value="signed">{t("signingSigned")}</Option>
                      <Option value="unsigned">{t("signingUnsigned")}</Option>
                    </Dropdown>
                  </Field>

                  <Field
                    label={t("buildArtifactPath")}
                    hint={t("artifactHint")}
                    className={`${styles.field} ${styles.wide}`}
                  >
                    <Input
                      className={styles.input}
                      value={artifactPath}
                      placeholder={selected?.build?.artifactPath ?? ""}
                      onChange={(_, data) => setArtifactPath(data.value)}
                    />
                  </Field>
                </div>

                <Field label={t("buildArtifacts")} hint={t("buildArtifactsHint")}>
                  <textarea
                    className={styles.input}
                    style={{
                      minHeight: "72px",
                      fontFamily: tokens.fontFamilyMonospace,
                      fontSize: tokens.fontSizeBase200,
                    }}
                    value={artifacts}
                    onChange={(event) => setArtifacts(event.target.value)}
                  />
                </Field>

                <Field
                  label={t("taskXcode")}
                  hint={
                    envChecked
                      ? t("taskXcodeHint")
                      : t("taskXcodeUnknown")
                  }
                >
                  <Dropdown
                    className={styles.input}
                    selectedOptions={[xcodePath]}
                    value={
                      xcodePath === ""
                        ? t("taskXcodeGlobal")
                        : (xcodes.find((item) => item.path === xcodePath)?.path ?? xcodePath)
                    }
                    onOptionSelect={(_, data) => setXcodePath(data.optionValue ?? "")}
                  >
                    <Option value="">{t("taskXcodeGlobal")}</Option>
                    {xcodes.map((instance) => (
                      <Option key={instance.path} value={instance.path}>
                        {instance.version
                          ? `Xcode ${instance.version} — ${instance.path}`
                          : instance.path}
                      </Option>
                    ))}
                    {/* Paths installed on the target but not detected now: keep the user's choice */}
                    {xcodePath !== "" && !xcodes.some((item) => item.path === xcodePath) ? (
                      <Option value={xcodePath}>{xcodePath}</Option>
                    ) : null}
                  </Dropdown>
                </Field>
              </>
            ) : null}

            {kind === "push" ? (
              <Checkbox
                checked={clean}
                label={t("pushClean")}
                onChange={(_, data) => setClean(data.checked === true)}
              />
            ) : null}

            {kind === "copy" || kind === "move" ? (
              <>
                {locationFields("from", t("taskFrom"), from, setFrom)}
                {locationFields("to", t("taskTo"), to, setTo)}
              </>
            ) : null}

            {kind === "delete" ? locationFields("target", t("taskTarget"), target, setTarget) : null}

            {kind === "pull" || kind === "export" ? (
              <>
                <div className={styles.sources}>
                  <span className={styles.hint}>{t("taskSources")}</span>
                  {sources.map((source, index) => (
                    <div key={`source-${index}`} className={styles.sourceRow}>
                      <Dropdown
                        selectedOptions={[source.side]}
                        value={source.side === "remote" ? t("sideRemote") : t("sideHost")}
                        onOptionSelect={(_, data) => {
                          const side = data.optionValue as PathSide | undefined;
                          if (side) {
                            setSources((current) =>
                              current.map((item, at) =>
                                at === index ? { ...item, side } : item,
                              ),
                            );
                          }
                        }}
                      >
                        <Option value="remote">{t("sideRemote")}</Option>
                        <Option value="host">{t("sideHost")}</Option>
                      </Dropdown>
                      <Input
                        className={styles.input}
                        value={source.path}
                        placeholder={t("pathHint")}
                        onChange={(_, data) =>
                          setSources((current) =>
                            current.map((item, at) =>
                              at === index ? { ...item, path: data.value } : item,
                            ),
                          )
                        }
                      />
                      <Tooltip content={t("taskPickPathHint")} relationship="description" withArrow>
                        <Button
                          appearance="subtle"
                          icon={<FolderOpen className="w-4 h-4" />}
                          aria-label={t("taskPickPath")}
                          disabled={!projectId}
                          onClick={() => setPicking({ target: index, current: source })}
                        />
                      </Tooltip>
                      <Button
                        appearance="subtle"
                        disabled={sources.length <= 1}
                        onClick={() =>
                          setSources((current) => current.filter((_, at) => at !== index))
                        }
                      >
                        {t("del")}
                      </Button>
                    </div>
                  ))}
                  <Button
                    appearance="secondary"
                    size="small"
                    onClick={() => setSources((current) => [...current, { side: "remote", path: "" }])}
                  >
                    {t("sourceAdd")}
                  </Button>
                </div>

                {kind === "pull"
                  ? locationFields("to", t("taskTo"), to, setTo)
                  : (
                    <Field label={t("taskOutputName")} hint={t("taskOutputNameHint")}>
                      <Input
                        className={styles.input}
                        value={outputName}
                        onChange={(_, data) => setOutputName(data.value)}
                      />
                    </Field>
                  )}
              </>
            ) : null}

            {kind === "copy" || kind === "move" || kind === "pull" ? (
              <Checkbox
                checked={overwrite}
                label={t("overwrite")}
                onChange={(_, data) => setOverwrite(data.checked === true)}
              />
            ) : null}

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

      {/* Design doc §7.4 / §9: pick a path from the project tree into that field.
          Outside Dialog because Fluent's Dialog accepts only one child. */}
      {picking ? (
        <TreeDialog
          open
          status={status}
          projectId={projectId}
          projectName={projectName}
          onClose={() => setPicking(null)}
          pick={{
            side: picking.current.side,
            path: picking.current.path,
            onPick: applyPicked,
          }}
        />
      ) : null}
    </>
  );
}






