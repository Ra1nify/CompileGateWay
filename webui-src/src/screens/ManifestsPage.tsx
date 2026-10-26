import type { JSX, ReactNode } from "react";
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
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
  Textarea,
  Tooltip,
  makeStyles,
  shorthands,
  tokens,
} from "@fluentui/react-components";
import {
  Plus,
  Download,
  RotateCw,
  Trash2,
  Pencil,
  Search,
  Wand2,
} from "lucide-react";
import { useCallback, useEffect, useId, useState } from "react";
import {
  ApiError,
  apiDeleteManifest,
  apiDetectManifests,
  apiListManifests,
  apiManifestPresets,
  apiReloadManifests,
  apiManifestRaw,
  apiResetManifests,
  apiSaveManifest,
  type ManifestDetectMatch,
  type ManifestPreset,
  type ManifestSummary,
  type Status,
} from "../api";
import { makeT } from "../i18n/strings";
import { TERMINAL } from "../terminalTheme";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { DescriptionList, PageHead } from "../components/Common";

/** Starter content for a new manifest (same shape as the backend's built-in template). */
const STARTER_MANIFEST = `{
  "displayName": "New build type",
  "platform": "xcode",
  "include": { "paths": ["project.xcodeproj"] },
  "variables": [{ "name": "scheme", "label": "Scheme", "required": true }],
  "build": { "steps": ["echo build {{scheme}}"], "artifactPath": "" },
  "cache": { "paths": [] }
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
  detect: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    marginBottom: "12px",
    maxWidth: "620px",
  },
  detectInput: {
    flex: "1 1 auto",
    minWidth: 0,
  },
  detectList: {
    display: "flex",
    flexDirection: "column",
    rowGap: "4px",
    marginTop: "4px",
  },
  detectMatch: {
    fontSize: tokens.fontSizeBase200,
    lineHeight: "1.6",
    overflowWrap: "anywhere",
  },
  detectName: {
    fontWeight: tokens.fontWeightSemibold,
  },
  matched: {
    color: tokens.colorPaletteGreenForeground1,
  },
  missing: {
    color: tokens.colorNeutralForeground3,
  },
  list: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
  },
  card: {
    width: "100%",
    borderRadius: tokens.borderRadiusLarge,
    padding: "14px 16px",
    display: "flex",
    flexDirection: "column",
    rowGap: "10px",
  },
  head: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    justifyContent: "space-between",
    columnGap: "20px",
    rowGap: "10px",
  },
  titles: {
    display: "flex",
    alignItems: "center",
    columnGap: "10px",
    flexWrap: "wrap",
    minWidth: 0,
  },
  name: {
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase400,
    overflowWrap: "anywhere",
  },
  dl: {
    columnGap: "16px",
    rowGap: "6px",
    fontSize: tokens.fontSizeBase200,
  },
  actions: {
    display: "flex",
    columnGap: "8px",
    flex: "0 0 auto",
    marginLeft: "auto",
  },
  empty: {
    padding: "12px 2px",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground3,
  },
  bar: {
    marginBottom: "12px",
  },
  errorBar: {
    marginTop: "12px",
  },
  dialogBody: {
    display: "flex",
    flexDirection: "column",
    rowGap: "12px",
  },
  readOnlyId: {
    display: "flex",
    columnGap: "10px",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  raw: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12.5px",
    lineHeight: "1.5",
    width: "100%",
  },
  dialogSurface: {
    maxWidth: "760px",
    width: "min(760px, 94vw)",
    boxSizing: "border-box",
  },
  mono: {
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: "12.5px",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
    margin: "6px 0 0",
    ...shorthands.padding("8px", "10px"),
    ...shorthands.borderRadius("6px"),
    // Fixed dark: this is a code/terminal surface, not a normal one (Inverted flips to white on dark themes)
    ...shorthands.border("1px", "solid", TERMINAL.border),
    backgroundColor: TERMINAL.background,
    color: TERMINAL.foreground,
  },
  presetList: {
    margin: 0,
    paddingLeft: "18px",
    display: "flex",
    flexDirection: "column",
    rowGap: "4px",
  },
});

interface Feedback {
  ok: boolean;
  text: string;
}

interface DetectState {
  path: string;
  matches: ManifestDetectMatch[];
}

/** Trigger a browser download (raw JSON export of a manifest). */
function downloadJson(id: string, raw: string): void {
  const blob = new Blob([raw], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = `${id}.json`;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

function joinOrDash(values: string[] | undefined): string {
  return values && values.length ? values.join(", ") : "-";
}

/**
 * Build types page: each manifest is one build type.
 * List plus in-place editing (raw JSON) plus create / export / delete / reset built-in presets / detect by directory.
 */
export function ManifestsPage({ status }: { status: Status }): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);
  const titleId = useId();

  const [manifests, setManifests] = useState<ManifestSummary[]>([]);
  const [missing, setMissing] = useState<ManifestPreset[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<Feedback | null>(null);

  // Create manifest
  const [creating, setCreating] = useState(false);
  const [newId, setNewId] = useState("");

  // Edit manifest
  const [editing, setEditing] = useState<ManifestSummary | null>(null);
  const [raw, setRaw] = useState("");
  const [rawLoading, setRawLoading] = useState(false);
  const [saveDetails, setSaveDetails] = useState<string[] | null>(null);

  // Delete / reset
  const [pendingDelete, setPendingDelete] = useState<ManifestSummary | null>(null);
  /** Design doc §6.4: a filename conflict on import asks the user to confirm overwrite. */
  const [pendingOverwrite, setPendingOverwrite] = useState<string | null>(null);
  const [pendingReset, setPendingReset] = useState(false);

  // Detect
  const [detectPath, setDetectPath] = useState("");
  const [detect, setDetect] = useState<DetectState | null>(null);
  const [detecting, setDetecting] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    setError(null);
    const [listResult, presetsResult] = await Promise.allSettled([
      apiListManifests(),
      apiManifestPresets(),
    ]);
    if (listResult.status === "fulfilled") {
      setManifests(listResult.value.manifests ?? []);
    } else {
      // Clear the list when it cannot be fetched, so no stale data plus error remains.
      setManifests([]);
      const err = listResult.reason;
      setError(err instanceof ApiError ? err.message : String(err));
    }
    // Hide "reset missing presets" when the preset query fails (better hidden than wrong).
    setMissing(presetsResult.status === "fulfilled" ? presetsResult.value.missing ?? [] : []);
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void load().finally(() => {
      if (!cancelled) {
        setLoading(false);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  async function openEditor(manifest: ManifestSummary): Promise<void> {
    setEditing(manifest);
    setRaw("");
    setSaveDetails(null);
    setRawLoading(true);
    setError(null);
    try {
      const result = await apiManifestRaw(manifest.id);
      setRaw(result.raw);
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("loadFailed", { msg: message }));
      setEditing(null);
    } finally {
      setRawLoading(false);
    }
  }

  function formatRaw(): void {
    try {
      setRaw(JSON.stringify(JSON.parse(raw), null, 2));
    } catch {
      // Keep it as-is when it does not parse.
    }
  }

  async function saveRaw(): Promise<void> {
    if (!editing) {
      return;
    }
    const id = editing.id;
    setBusy(true);
    setSaveDetails(null);
    setFeedback(null);
    try {
      await apiSaveManifest(id, raw);
      setEditing(null);
      setFeedback({ ok: true, text: t("manifestSaved") });
      await load();
    } catch (err) {
      if (err instanceof ApiError) {
        setSaveDetails(err.details.length ? err.details : [err.message]);
      } else {
        setSaveDetails([String(err)]);
      }
    } finally {
      setBusy(false);
    }
  }

  async function createManifest(): Promise<void> {
    const id = newId.trim();
    if (!id) {
      return;
    }
    // Design doc §6.4: if a file with the same name exists, ask first; write only after confirm
    if (manifests.some((manifest) => manifest.id === id)) {
      setPendingOverwrite(id);
      return;
    }
    await writeManifest(id);
  }

  async function writeManifest(id: string): Promise<void> {
    setPendingOverwrite(null);
    setBusy(true);
    setSaveDetails(null);
    setFeedback(null);
    try {
      await apiSaveManifest(id, STARTER_MANIFEST);
      setCreating(false);
      setNewId("");
      setFeedback({ ok: true, text: t("manifestSaved") });
      await load();
    } catch (err) {
      if (err instanceof ApiError) {
        setSaveDetails(err.details.length ? err.details : [err.message]);
      } else {
        setSaveDetails([String(err)]);
      }
    } finally {
      setBusy(false);
    }
  }

  async function removeManifest(manifest: ManifestSummary): Promise<void> {
    setPendingDelete(null);
    setBusy(true);
    setFeedback(null);
    try {
      await apiDeleteManifest(manifest.id);
      await load();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("saveFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  async function resetPresets(ids: string[]): Promise<void> {
    setPendingReset(false);
    setBusy(true);
    setFeedback(null);
    try {
      const result = await apiResetManifests(ids);
      setFeedback({ ok: true, text: t("presetResetDone", { n: result.restored.length }) });
      await load();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("saveFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  async function runDetect(): Promise<void> {
    const sourcePath = detectPath.trim();
    if (!sourcePath) {
      return;
    }
    setDetecting(true);
    setDetect(null);
    setError(null);
    try {
      const result = await apiDetectManifests(sourcePath);
      setDetect({ path: sourcePath, matches: result.matches ?? [] });
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("loadFailed", { msg: message }));
    } finally {
      setDetecting(false);
    }
  }

  /**
   * Refresh: re-read the build type directory from disk.
   *
   * The json under `manifest/` is **yours** -- you may have edited it in an editor,
   * pulled it with git, or had an AI write the file. The daemon cannot see those
   * actions, so this button means "read it again and tell me what changed".
   */
  async function refresh(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const result = await apiReloadManifests();
      setManifests(result.manifests ?? []);
      const missingNow = await apiManifestPresets();
      setMissing(missingNow.missing ?? []);
      // Decide "no changes" from the structured arrays, never from `summary`: that
      // string follows the daemon's language setting, so comparing it to a literal
      // silently breaks the moment the language changes.
      const nothingChanged =
        (result.added?.length ?? 0) === 0 &&
        (result.changed?.length ?? 0) === 0 &&
        (result.removed?.length ?? 0) === 0;
      setFeedback({
        ok: true,
        text: nothingChanged
          ? t("manifestRefreshNoChange")
          : t("manifestRefreshDone", { summary: result.summary ?? "" }),
      });
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("loadFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  const toolbar: ReactNode = (
    <div className={styles.toolbar}>
      <Tooltip content={t("manifestRefresh")} relationship="description" withArrow>
        <Button
          appearance="subtle"
          icon={<RotateCw className="w-4 h-4" />}
          aria-label={t("manifestRefresh")}
          disabled={busy}
          onClick={() => void refresh()}
        />
      </Tooltip>
      {missing.length > 0 ? (
        <Tooltip content={t("presetReset")} relationship="description" withArrow>
          <Button
            appearance="subtle"
            icon={<RotateCw className="w-4 h-4" />}
            aria-label={t("presetReset")}
            disabled={busy}
            onClick={() => setPendingReset(true)}
          />
        </Tooltip>
      ) : null}
      <Tooltip content={t("manifestImport")} relationship="description" withArrow>
        <Button
          appearance="subtle"
          icon={<Plus className="w-4 h-4" />}
          aria-label={t("manifestImport")}
          disabled={busy}
          onClick={() => {
            setNewId("");
            setSaveDetails(null);
            setCreating(true);
          }}
        />
      </Tooltip>
    </div>
  );

  return (
    <div className={styles.screen}>
      <PageHead
        title={t("manifestsTitle")}
        subtitle={t("manifestsSubtitle")}
        actions={toolbar}
      />

      <div className={styles.detect}>
        <Input
          className={styles.detectInput}
          value={detectPath}
          placeholder={t("detectHint")}
          aria-label={t("detectTitle")}
          onChange={(_, data) => setDetectPath(data.value)}
        />
        <Tooltip content={t("detectRun")} relationship="description" withArrow>
          <Button
            appearance="subtle"
            icon={<Search className="w-4 h-4" />}
            aria-label={t("detectRun")}
            disabled={detecting}
            onClick={() => void runDetect()}
          />
        </Tooltip>
      </div>

      {detect ? (
        <MessageBar className={styles.bar} intent={detect.matches.length ? "info" : "warning"}>
          <MessageBarBody>
            {detect.matches.length === 0 ? (
              t("detectNone")
            ) : (
              <div className={styles.detectList}>
                {detect.matches.map((match) => (
                  <div key={match.id} className={styles.detectMatch}>
                    <span className={styles.detectName}>{match.displayName}</span>
                    {` · ${t("detectMatched")}: `}
                    <span className={styles.matched}>{joinOrDash(match.matched)}</span>
                    {match.missing.length ? (
                      <>
                        {` · ${t("detectMissing")}: `}
                        <span className={styles.missing}>{match.missing.join(", ")}</span>
                      </>
                    ) : null}
                  </div>
                ))}
              </div>
            )}
          </MessageBarBody>
        </MessageBar>
      ) : null}

      {error ? (
        <MessageBar className={styles.bar} intent="error">
          <MessageBarBody>{error}</MessageBarBody>
        </MessageBar>
      ) : null}

      {feedback ? (
        <MessageBar className={styles.bar} intent={feedback.ok ? "success" : "error"}>
          <MessageBarBody>{feedback.text}</MessageBarBody>
        </MessageBar>
      ) : null}
      {loading ? (
        <Spinner size="tiny" label={t("manifestsTitle")} />
      ) : (
        <div className={styles.list}>
          {manifests.length === 0 ? (
            <p className={styles.empty}>{error ? t("none") : t("emptyManifests")}</p>
          ) : null}
          {manifests.map((manifest) => (
            <Card key={manifest.id} className={styles.card}>
              <div className={styles.head}>
                <div className={styles.titles}>
                  <span className={styles.name}>{manifest.displayName}</span>
                  <Badge appearance="tint" color="brand" size="small">
                    {manifest.platform}
                  </Badge>
                  {manifest.template ? (
                    <Badge appearance="outline" color="informative" size="small">
                      {t("manifestBuiltin")}
                    </Badge>
                  ) : null}
                </div>
                <div className={styles.actions}>
                  <Tooltip content={t("manifestEdit")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Pencil className="w-4 h-4" />}
                      aria-label={t("manifestEdit")}
                      disabled={busy}
                      onClick={() => void openEditor(manifest)}
                    />
                  </Tooltip>
                  <Tooltip content={t("manifestExport")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Download className="w-4 h-4" />}
                      aria-label={t("manifestExport")}
                      disabled={busy}
                      onClick={() => void exportManifest(manifest.id)}
                    />
                  </Tooltip>
                  <Tooltip content={t("manifestDelete")} relationship="description" withArrow>
                    <Button
                      appearance="subtle"
                      icon={<Trash2 className="w-4 h-4" />}
                      aria-label={t("manifestDelete")}
                      disabled={busy}
                      style={{ color: tokens.colorPaletteRedForeground1 }}
                      onClick={() => setPendingDelete(manifest)}
                    />
                  </Tooltip>
                </div>
              </div>

              <DescriptionList
                className={styles.dl}
                items={[
                  { term: "id", detail: manifest.id, nowrap: true },
                  { term: "include.paths", detail: joinOrDash(manifest.include?.paths) },
                  {
                    term: t("manifestSteps"),
                    detail: String(manifest.build?.stepCount ?? 0),
                    nowrap: true,
                  },
                  { term: t("manifestArtifact"), detail: manifest.build?.artifactPath || "-" },
                  { term: t("manifestCache"), detail: joinOrDash(manifest.cache?.paths) },
                  {
                    term: t("manifestVariables"),
                    detail: String(manifest.variables?.length ?? 0),
                    nowrap: true,
                  },
                ]}
              />

              {!manifest.valid ? (
                <MessageBar intent="error">
                  <MessageBarBody>
                    <div className={styles.detectList}>
                      {(manifest.errors ?? []).map((item, index) => (
                        <div key={`${manifest.id}-err-${index}`} className={styles.detectMatch}>
                          {item}
                        </div>
                      ))}
                    </div>
                  </MessageBarBody>
                </MessageBar>
              ) : null}
            </Card>
          ))}
        </div>
      )}

      {/* Edit manifest */}
      <Dialog
        open={editing !== null}
        modalType="modal"
        onOpenChange={(_, data) => {
          if (!data.open) {
            setEditing(null);
            setSaveDetails(null);
          }
        }}
      >
        <DialogSurface className={styles.dialogSurface}>
          <DialogBody>
            <DialogTitle>{t("manifestEdit")}</DialogTitle>
            <DialogContent className={styles.dialogBody}>
              <div className={styles.readOnlyId}>
                <span>id</span>
                <span>{editing?.id ?? ""}</span>
              </div>
              {rawLoading ? (
                <Spinner size="tiny" label={t("manifestEdit")} />
              ) : (
                <Textarea
                  className={styles.raw}
                  rows={18}
                  resize="vertical"
                  value={raw}
                  aria-label={t("manifestEdit")}
                  onChange={(_, data) => setRaw(data.value)}
                />
              )}
              {saveDetails ? (
                <MessageBar intent="error">
                  <MessageBarBody>
                    <div className={styles.detectList}>
                      {saveDetails.map((item, index) => (
                        <div key={`detail-${index}`} className={styles.detectMatch}>
                          {item}
                        </div>
                      ))}
                    </div>
                  </MessageBarBody>
                </MessageBar>
              ) : null}
            </DialogContent>
            <DialogActions>
              <Button
                appearance="secondary"
                icon={<Wand2 className="w-4 h-4" />}
                disabled={rawLoading}
                onClick={formatRaw}
              >
                {t("manifestFormat")}
              </Button>
              <Button appearance="secondary" onClick={() => setEditing(null)}>
                {t("cancel")}
              </Button>
              <Button appearance="primary" disabled={busy || rawLoading} onClick={() => void saveRaw()}>
                {t("manifestSave")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* New manifest: the browser cannot read the picked file, so ask only for an id and use the starter template. */}
      <Dialog
        open={creating}
        modalType="modal"
        onOpenChange={(_, data) => {
          if (!data.open) {
            setCreating(false);
            setSaveDetails(null);
          }
        }}
      >
        <DialogSurface aria-labelledby={titleId}>
          <DialogBody>
            <DialogTitle id={titleId}>{t("manifestImport")}</DialogTitle>
            <DialogContent className={styles.dialogBody}>
              <Field label={t("manifestNewId")}>
                <Input
                  value={newId}
                  autoFocus
                  onChange={(_, data) => setNewId(data.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      event.preventDefault();
                      void createManifest();
                    }
                  }}
                />
              </Field>
              {saveDetails ? (
                <MessageBar intent="error">
                  <MessageBarBody>
                    <div className={styles.detectList}>
                      {saveDetails.map((item, index) => (
                        <div key={`new-detail-${index}`} className={styles.detectMatch}>
                          {item}
                        </div>
                      ))}
                    </div>
                  </MessageBarBody>
                </MessageBar>
              ) : null}
            </DialogContent>
            <DialogActions>
              <Button appearance="secondary" onClick={() => setCreating(false)}>
                {t("cancel")}
              </Button>
              <Button
                appearance="primary"
                disabled={busy || newId.trim() === ""}
                onClick={() => void createManifest()}
              >
                {t("manifestSave")}
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>

      {/* Delete manifest */}
      <ConfirmDialog
        open={pendingOverwrite !== null}
        title={t("manifestOverwriteTitle")}
        message={t("manifestOverwriteConfirm", { id: pendingOverwrite ?? "" })}
        confirmLabel={t("overwrite")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingOverwrite(null)}
        onConfirm={() => {
          if (pendingOverwrite) {
            void writeManifest(pendingOverwrite);
          }
        }}
      />

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t("manifestDeleteConfirm")}
        message={pendingDelete?.displayName}
        confirmLabel={t("manifestDelete")}
        cancelLabel={t("cancel")}
        danger
        busy={busy}
        onCancel={() => setPendingDelete(null)}
        onConfirm={() => {
          if (pendingDelete) {
            void removeManifest(pendingDelete);
          }
        }}
      />

      {/* Reset missing presets */}
      <ConfirmDialog
        open={pendingReset}
        title={t("presetResetTitle")}
        message={
          <>
            <p style={{ margin: "0 0 8px" }}>{t("presetResetHint")}</p>
            <ul className={styles.presetList}>
              {missing.map((preset) => (
                <li key={preset.id}>
                  {preset.displayName} (<span className={styles.mono}>{preset.id}</span>)
                </li>
              ))}
            </ul>
          </>
        }
        confirmLabel={t("presetReset")}
        cancelLabel={t("cancel")}
        busy={busy}
        onCancel={() => setPendingReset(false)}
        onConfirm={() => void resetPresets(missing.map((preset) => preset.id))}
      />
    </div>
  );

  async function exportManifest(id: string): Promise<void> {
    setError(null);
    try {
      const result = await apiManifestRaw(id);
      downloadJson(result.id || id, result.raw);
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setError(t("loadFailed", { msg: message }));
    }
  }
}
