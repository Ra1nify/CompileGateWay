import type { JSX } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { RotateCw, Trash2, FileDown, FileUp } from "lucide-react";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiExportKnownHosts,
  apiImportKnownHosts,
  apiPickFile,
  apiSetBaseDir,
  apiSetHostKey,
  type ClientInfo,
  type Status,
} from "../../api";
import { makeT } from "../../i18n/strings";
import { DescriptionList } from "../../components/Common";

const useStyles = makeStyles({
  card: {
    maxWidth: "720px",
    borderRadius: tokens.borderRadiusXLarge,
    padding: "20px 22px",
  },
  spaced: {
    marginTop: "14px",
  },
  inline: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
  },
  resolved: {
    marginTop: "6px",
    display: "block",
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
    overflowWrap: "anywhere",
  },
});

/** The instance view's "status" tab: basic info plus base-directory editing (validated live on save). */
export function InstanceHome({
  client,
  status,
  onReload,
}: {
  client: ClientInfo;
  status: Status;
  onReload: () => Promise<void>;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [baseDir, setBaseDir] = useState(client.baseDir ?? "");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [hostKey, setHostKey] = useState(client.hostKey ?? "");
  const [keyLine, setKeyLine] = useState("");

  async function forgetHostKey(): Promise<void> {
    try {
      await apiSetHostKey(client.id, "");
      setHostKey("");
      setKeyLine("");
      await onReload();
    } catch (err) {
      setResult({ ok: false, text: err instanceof ApiError ? err.message : String(err) });
    }
  }

  /** Import from known_hosts: the file picker opens on the daemon machine. */
  async function importKnownHosts(): Promise<void> {
    setBusy(true);
    setResult(null);
    try {
      const picked = await apiPickFile(t("knownHostsPick"));
      if (!picked.path) {
        return;
      }
      const response = await apiImportKnownHosts(client.id, { path: picked.path });
      const lines = [
        response.alreadyTrusted
          ? t("knownHostsAlready", { key: response.fingerprint })
          : t("knownHostsImported", {
              line: String(response.line),
              type: response.keyType,
              key: response.fingerprint,
            }),
        ...response.notes,
      ];
      setResult({ ok: true, text: lines.join("\n") });
      setHostKey(response.fingerprint);
      setKeyLine(response.keyLine);
      await onReload();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setResult({ ok: false, text: message });
    } finally {
      setBusy(false);
    }
  }

  /** Export as one known_hosts line (can be appended to ~/.ssh/known_hosts directly). */
  async function exportKnownHosts(): Promise<void> {
    setResult(null);
    try {
      const response = await apiExportKnownHosts(client.id);
      setKeyLine(response.line);
      setResult({
        ok: true,
        text: response.warning ? `${response.line}\n${response.warning}` : response.line,
      });
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setResult({ ok: false, text: message });
    }
  }

  useEffect(() => {
    setBaseDir(client.baseDir ?? "");
    setHostKey(client.hostKey ?? "");
    setKeyLine("");
    setResult(null);
  }, [client.id, client.baseDir]);

  async function saveBaseDir(): Promise<void> {
    setBusy(true);
    setResult(null);
    try {
      const response = await apiSetBaseDir(client.id, baseDir, true);
      setResult({ ok: response.ok, text: response.message || t(response.ok ? "saved" : "syncFailed") });
      if (response.ok) {
        await onReload();
      }
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setResult({ ok: false, text: t("basedirFailed", { msg: message }) });
    } finally {
      setBusy(false);
    }
  }

  const method = client.auth.method === "key" ? t("authKey") : t("authPassword");
  const authText = client.auth.username ? `${method} · ${client.auth.username}` : method;

  return (
    <>
      <Card className={styles.card}>
        <DescriptionList
          items={[
            { term: t("fHost"), detail: client.host, nowrap: true },
            { term: t("fPort"), detail: String(client.port) },
            { term: t("fAuth"), detail: authText },
            { term: t("fTags"), detail: client.tags?.length ? client.tags.join(", ") : "-" },
            { term: t("hostKey"), detail: client.hostKey || "-" },
          ]}
        />
      </Card>

      <Card className={`${styles.card} ${styles.spaced}`}>
        <Field label={t("baseDir")} hint={t("baseDirHint")}>
          <div className={styles.inline}>
            <Input
              style={{ flex: "1 1 auto" }}
              value={baseDir}
              placeholder="~/CompileGateway"
              onChange={(_, data) => setBaseDir(data.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void saveBaseDir();
                }
              }}
            />
            <Tooltip content={t("baseDirSave")} relationship="description" withArrow>
              <Button
                appearance="primary"
                icon={busy ? <Spinner size="tiny" /> : <RotateCw className="w-4 h-4" />}
                aria-label={t("baseDirSave")}
                disabled={busy}
                onClick={() => void saveBaseDir()}
              />
            </Tooltip>
          </div>
        </Field>
        {client.baseDirResolved ? (
          <em className={styles.resolved}>
            {t("projectRemote")}: {client.baseDirResolved}
          </em>
        ) : null}

        {result ? (
          <MessageBar intent={result.ok ? "success" : "error"} style={{ marginTop: 14 }}>
            <MessageBarBody>{result.text}</MessageBarBody>
          </MessageBar>
        ) : null}
      </Card>

      {/* Host fingerprint (TOFU). A changed fingerprint rejects the connection, but we
          need an escape hatch: rebuilding a macOS VM or reinstalling swaps host keys. */}
      <Card className={`${styles.card} ${styles.spaced}`}>
        <Field label={t("hostKeyStored")} hint={t("hostKeyHint")}>
          <div className={styles.inline}>
            <Input
              style={{ flex: "1 1 auto" }}
              value={hostKey}
              readOnly
              placeholder={t("hostKeyNone")}
            />
            <Tooltip content={t("hostKeyForget")} relationship="description" withArrow>
              <Button
                appearance="subtle"
                icon={<Trash2 className="w-4 h-4" />}
                aria-label={t("hostKeyForget")}
                disabled={!hostKey}
                onClick={() => void forgetHostKey()}
              />
            </Tooltip>
          </div>
        </Field>

        {/* known_hosts import/export: beyond TOFU there must be an "offline pre-trust" and portable path */}
        <div className={styles.inline} style={{ marginTop: 12 }}>
          <Tooltip content={t("knownHostsImportHint")} relationship="description" withArrow>
            <Button
              appearance="secondary"
              icon={busy ? <Spinner size="tiny" /> : <FileUp className="w-4 h-4" />}
              disabled={busy}
              onClick={() => void importKnownHosts()}
            >
              {t("knownHostsImport")}
            </Button>
          </Tooltip>
          <Tooltip content={t("knownHostsExportHint")} relationship="description" withArrow>
            <Button
              appearance="secondary"
              icon={<FileDown className="w-4 h-4" />}
              disabled={!hostKey}
              onClick={() => void exportKnownHosts()}
            >
              {t("knownHostsExport")}
            </Button>
          </Tooltip>
          <span className={styles.resolved}>
            {client.hasKeyLine ? t("hostKeyLineStored") : t("hostKeyLineMissing")}
          </span>
        </div>
        {keyLine ? (
          <Field label={t("knownHostsLine")} style={{ marginTop: 12 }}>
            <Input
              value={keyLine}
              readOnly
              onFocus={(event) => event.target.select()}
            />
          </Field>
        ) : null}
      </Card>
    </>
  );
}
