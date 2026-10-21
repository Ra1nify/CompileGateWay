import type { JSX, ReactNode } from "react";
import {
  Badge,
  Button,
  Card,
  Checkbox,
  Dropdown,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  SearchBox,
  Tooltip,
  makeStyles,
  mergeClasses,
  tokens,
} from "@fluentui/react-components";
import {
  Add16Regular,
  Checkmark16Regular,
  Delete20Regular,
  Edit20Regular,
  FolderOpen20Regular,
  Copy20Regular,
} from "@fluentui/react-icons";
import { useRef, useState } from "react";
import {
  ApiError,
  apiDeleteClient,
  apiPickFile,
  apiSaveClient,
  type AuthMethod,
  type ClientInfo,
  type ClientInput,
  type Status,
} from "../api";
import { makeT, type Translate } from "../i18n/strings";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { PageHead } from "../components/Common";
import { TagEditor } from "../components/TagEditor";
import { useSearchFit } from "../hooks/useSearchFit";
import { matchesQuery } from "../format";

/** Kept in sync with clients::DEFAULT_BASE_DIR on the Rust side. */
export const DEFAULT_BASE_DIR = "~/CompileGateway";

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
    flex: "0 0 auto",
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
  },
  cardView: {
    cursor: "pointer",
    ":hover": {
      borderTopColor: tokens.colorBrandStroke1,
      borderRightColor: tokens.colorBrandStroke1,
      borderBottomColor: tokens.colorBrandStroke1,
      borderLeftColor: tokens.colorBrandStroke1,
    },
    ":focus-visible": {
      outline: `2px solid ${tokens.colorStrokeFocus2}`,
      outlineOffset: "1px",
    },
  },
  cardEdit: {
    borderTopColor: tokens.colorBrandStroke2,
    borderRightColor: tokens.colorBrandStroke2,
    borderBottomColor: tokens.colorBrandStroke2,
    borderLeftColor: tokens.colorBrandStroke2,
  },
  fields: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    columnGap: "22px",
    rowGap: "12px",
    minWidth: 0,
  },
  row: {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "flex-start",
    justifyContent: "space-between",
    columnGap: "20px",
    rowGap: "12px",
  },
  group: {
    display: "flex",
    flexDirection: "column",
    rowGap: "5px",
    flex: "0 0 auto",
    maxWidth: "100%",
    minWidth: 0,
  },
  groupWide: {
    flex: "1 1 320px",
  },
  groupNarrow: {
    width: "96px",
  },
  label: {
    fontSize: "11px",
    color: tokens.colorNeutralForeground3,
    letterSpacing: "0.4px",
  },
  value: {
    whiteSpace: "nowrap",
    fontSize: tokens.fontSizeBase300,
    color: tokens.colorNeutralForeground1,
  },
  input: {
    minWidth: "176px",
    width: "100%",
  },
  inputNarrow: {
    minWidth: "96px",
    width: "96px",
  },
  inline: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
  },
  tagsGroup: {
    width: "100%",
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
  errorBar: {
    marginTop: "12px",
  },
});

interface DraftAuth {
  method: AuthMethod;
  username: string;
  keyPath: string;
  hasPassword: boolean;
  /** Empty string = keep the saved password unchanged. */
  password: string;
  hasPassphrase: boolean;
  /** Empty string = keep the saved key passphrase unchanged. */
  passphrase: string;
}

interface DraftClient {
  id: string;
  name: string;
  host: string;
  port: string;
  auth: DraftAuth;
  tags: string[];
  baseDir: string;
  hostLock: boolean;
}

function toDraft(client: ClientInfo): DraftClient {
  return {
    id: client.id,
    name: client.name,
    host: client.host,
    port: String(client.port),
    auth: {
      method: client.auth.method,
      username: client.auth.username,
      keyPath: client.auth.keyPath,
      hasPassword: client.auth.hasPassword,
      password: "",
      hasPassphrase: client.auth.hasPassphrase,
      passphrase: "",
    },
    tags: [...(client.tags ?? [])],
    baseDir: client.baseDir || "",
    hostLock: Boolean(client.hostLock),
  };
}

function newDraft(): DraftClient {
  return {
    id: "",
    name: "",
    host: "",
    port: "22",
    auth: {
      method: "password",
      username: "",
      keyPath: "",
      hasPassword: false,
      password: "",
      hasPassphrase: false,
      passphrase: "",
    },
    tags: [],
    baseDir: DEFAULT_BASE_DIR,
    hostLock: false,
  };
}

function authLabel(client: ClientInfo, t: Translate): string {
  const method = client.auth.method === "key" ? t("authKey") : t("authPassword");
  const user = client.auth.username;
  return user ? `${method} · ${user}` : method;
}

export function ClientsPage({
  status,
  clients,
  onReload,
  onOpenInstance,
}: {
  status: Status;
  clients: ClientInfo[];
  onReload: () => Promise<void>;
  onOpenInstance: (clientId: string) => void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<DraftClient[]>([]);
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [errorText, setErrorText] = useState<string | null>(null);
  const [copiedCmd, setCopiedCmd] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<{ index: number; name: string } | null>(null);

  const searchRef = useSearchFit(t("searchHint"));
  const portRefs = useRef(new Map<number, HTMLInputElement | null>());

  const source: { key: string; client: ClientInfo | DraftClient; index: number }[] = editing
    ? draft.map((item, index) => ({ key: `${item.id || "new"}-${index}`, client: item, index }))
    : clients.map((item, index) => ({ key: item.id, client: item, index }));

  const visible = source.filter((entry) =>
    matchesQuery(
      {
        name: entry.client.name,
        host: entry.client.host,
        port: Number(entry.client.port) || 0,
        tags: entry.client.tags,
      },
      query,
    ),
  );

  function startEdit(): void {
    setDraft(clients.map(toDraft));
    setErrorText(null);
    setEditing(true);
  }

  function patchDraft(index: number, patch: (item: DraftClient) => DraftClient): void {
    setDraft((current) => current.map((item, i) => (i === index ? patch(item) : item)));
  }

  async function saveAll(): Promise<void> {
    setBusy(true);
    setErrorText(null);
    const kept = new Set<string>();
    try {
      for (const item of draft) {
        const input: ClientInput = {
          id: item.id,
          name: item.name,
          host: item.host,
          port: Number.parseInt(item.port, 10) || 22,
          auth: {
            method: item.auth.method,
            username: item.auth.username,
            keyPath: item.auth.keyPath,
            // Empty string = unchanged, so omit the whole field.
            ...(item.auth.password ? { password: item.auth.password } : {}),
            // Same for the key passphrase: `ssh-keygen` encrypts keys by default and this is a common gate
            ...(item.auth.passphrase ? { passphrase: item.auth.passphrase } : {}),
          },
          tags: item.tags.map((tag) => tag.trim()).filter(Boolean),
          baseDir: item.baseDir,
          hostLock: item.hostLock,
        };
        const result = await apiSaveClient(input);
        kept.add(result.client.id);
      }
      for (const client of clients) {
        if (!kept.has(client.id)) {
          await apiDeleteClient(client.id);
        }
      }
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setErrorText(t("saveFailed", { msg: message }));
      setBusy(false);
      return;
    }

    setEditing(false);
    setDraft([]);
    setBusy(false);
    await onReload();
  }

  async function pickKey(index: number): Promise<void> {
    setErrorText(null);
    try {
      const picked = await apiPickFile(t("chooseKey"));
      if (picked.path) {
        patchDraft(index, (item) => ({ ...item, auth: { ...item.auth, keyPath: picked.path! } }));
      }
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setErrorText(t("saveFailed", { msg: message }));
    }
  }

  const toolbar: ReactNode = editing ? (
    <div className={styles.toolbar}>
      <Tooltip content={t("addClient")} relationship="description" withArrow>
        <Button
          appearance="subtle"
          icon={<Add16Regular />}
          aria-label={t("addClient")}
          onClick={() => setDraft((current) => [...current, newDraft()])}
        />
      </Tooltip>
      <SearchBox
        className={styles.search}
        input={{ ref: searchRef }}
        value={query}
        placeholder={t("searchHint")}
        aria-label={t("search")}
        onChange={(_, data) => setQuery(data.value)}
      />
      <Tooltip content={t("done")} relationship="description" withArrow>
        <Button
          appearance="primary"
          icon={<Checkmark16Regular />}
          aria-label={t("done")}
          disabled={busy}
          onClick={() => void saveAll()}
        />
      </Tooltip>
    </div>
  ) : (
    <div className={styles.toolbar}>
      <Tooltip content={t("addClient")} relationship="description" withArrow>
        <Button
          appearance="primary"
          icon={<Add16Regular />}
          aria-label={t("addClient")}
          onClick={() => {
            startEdit();
            setDraft((current) => [...current, newDraft()]);
          }}
        >
          {t("addClient")}
        </Button>
      </Tooltip>
      <SearchBox
        className={styles.search}
        input={{ ref: searchRef }}
        value={query}
        placeholder={t("searchHint")}
        aria-label={t("search")}
        onChange={(_, data) => setQuery(data.value)}
      />
      <Tooltip content={t("edit")} relationship="description" withArrow>
        <Button
          appearance="secondary"
          icon={<Edit20Regular />}
          aria-label={t("edit")}
          onClick={startEdit}
        />
      </Tooltip>
    </div>
  );

  return (
    <div className={styles.screen}>
      <PageHead
        title={t("clientsTitle")}
        subtitle={t("clientsSubtitle")}
        actions={toolbar}
      />

      {errorText ? (
        <MessageBar className={styles.errorBar} intent="error">
          <MessageBarBody style={{ whiteSpace: "pre-wrap" }}>
            <div>{errorText}</div>
            {(() => {
              const cmdMatch = errorText.match(/(?:openssl pkcs8 -topk8|puttygen)[^\n\r]+/);
              if (!cmdMatch) return null;
              const cmd = cmdMatch[0].trim();
              return (
                <div
                  style={{
                    marginTop: "8px",
                    display: "flex",
                    alignItems: "center",
                    gap: "8px",
                    background: tokens.colorNeutralBackground3,
                    padding: "6px 10px",
                    borderRadius: tokens.borderRadiusMedium,
                  }}
                >
                  <code
                    style={{
                      fontFamily: tokens.fontFamilyMonospace,
                      fontSize: "12px",
                      flex: 1,
                      userSelect: "all",
                    }}
                  >
                    {cmd}
                  </code>
                  <Button
                    appearance="subtle"
                    size="small"
                    icon={<Copy20Regular />}
                    onClick={() => {
                      void navigator.clipboard.writeText(cmd);
                      setCopiedCmd(true);
                      setTimeout(() => setCopiedCmd(false), 2000);
                    }}
                  >
                    {copiedCmd ? t("copied") : t("copyCommand")}
                  </Button>
                </div>
              );
            })()}
          </MessageBarBody>
        </MessageBar>
      ) : null}

      <div className={styles.list}>
        {source.length === 0 ? (
          <div style={{ textAlign: "center", padding: "32px 0" }}>
            <p className={styles.empty}>{t("emptyClients")}</p>
            <Button
              appearance="primary"
              icon={<Add16Regular />}
              style={{ marginTop: "12px" }}
              onClick={() => {
                startEdit();
                setDraft([newDraft()]);
              }}
            >
              {t("addClient")}
            </Button>
          </div>
        ) : null}
        {source.length > 0 && visible.length === 0 ? (
          <p className={styles.empty}>{t("noMatch")}</p>
        ) : null}

        {visible.map((entry) =>
          editing ? (
            <DraftCard
              key={entry.key}
              item={entry.client as DraftClient}
              index={entry.index}
              t={t}
              styles={styles}
              portRef={(element) => portRefs.current.set(entry.index, element)}
              onPatch={patchDraft}
              onPickKey={pickKey}
              onRequestDelete={() =>
                setPendingDelete({
                  index: entry.index,
                  name: (entry.client as DraftClient).name || t("addClient"),
                })
              }
            />
          ) : (
            <ViewCard
              key={entry.key}
              client={entry.client as ClientInfo}
              t={t}
              styles={styles}
              onOpen={() => onOpenInstance((entry.client as ClientInfo).id)}
            />
          ),
        )}
      </div>

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t("delConfirm")}
        message={pendingDelete?.name}
        confirmLabel={t("del")}
        cancelLabel={t("cancel")}
        danger
        onCancel={() => setPendingDelete(null)}
        onConfirm={() => {
          if (pendingDelete) {
            setDraft((current) => current.filter((_, index) => index !== pendingDelete.index));
          }
          setPendingDelete(null);
        }}
      />
    </div>
  );
}

type Styles = ReturnType<typeof useStyles>;

function ViewCard({
  client,
  t,
  styles,
  onOpen,
}: {
  client: ClientInfo;
  t: Translate;
  styles: Styles;
  onOpen: () => void;
}): JSX.Element {
  return (
    <Card
      className={mergeClasses(styles.card, styles.cardView)}
      role="button"
      tabIndex={0}
      aria-label={`${client.name} — ${t("openHint")}`}
      title={t("openHint")}
      onClick={onOpen}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onOpen();
        }
      }}
    >
      <div className={styles.row}>
        <div className={styles.fields}>
          <FieldBlock label={t("fName")} styles={styles}>
            {client.name}
          </FieldBlock>
          <FieldBlock label={t("fHost")} styles={styles}>
            {client.host}
          </FieldBlock>
          <FieldBlock label={t("fPort")} styles={styles}>
            {String(client.port)}
          </FieldBlock>
          <FieldBlock label={t("fAuth")} styles={styles}>
            {authLabel(client, t)}
          </FieldBlock>
          {client.tags?.length ? (
            <FieldBlock label={t("fTags")} styles={styles}>
              {client.tags.join(", ")}
            </FieldBlock>
          ) : null}
          {client.hostLock ? (
            <FieldBlock label={t("clientHostLock")} styles={styles}>
              <Badge appearance="tint" color="warning" size="small">
                {t("clientHostLockBadge")}
              </Badge>
            </FieldBlock>
          ) : null}
        </div>
      </div>
    </Card>
  );
}

function FieldBlock({
  label,
  styles,
  children,
}: {
  label: string;
  styles: Styles;
  children: ReactNode;
}): JSX.Element {
  return (
    <div className={styles.group}>
      <div className={styles.label}>{label}</div>
      <div className={styles.value}>{children}</div>
    </div>
  );
}

function DraftCard({
  item,
  index,
  t,
  styles,
  portRef,
  onPatch,
  onPickKey,
  onRequestDelete,
}: {
  item: DraftClient;
  index: number;
  t: Translate;
  styles: Styles;
  portRef: (element: HTMLInputElement | null) => void;
  onPatch: (index: number, patch: (item: DraftClient) => DraftClient) => void;
  onPickKey: (index: number) => Promise<void>;
  onRequestDelete: () => void;
}): JSX.Element {
  const isPassword = item.auth.method !== "key";

  return (
    <Card className={mergeClasses(styles.card, styles.cardEdit)}>
      <div className={styles.fields}>
        <Field label={t("fName")} className={styles.group}>
          <Input
            className={styles.input}
            value={item.name}
            onChange={(_, data) => onPatch(index, (c) => ({ ...c, name: data.value }))}
          />
        </Field>
        <Field label={t("fHost")} className={styles.group}>
          <Input
            className={styles.input}
            value={item.host}
            onChange={(_, data) => onPatch(index, (c) => ({ ...c, host: data.value }))}
          />
        </Field>
        <Field label={t("fPort")} className={mergeClasses(styles.group, styles.groupNarrow)}>
          <Input
            className={styles.inputNarrow}
            type="number"
            min={1}
            max={65535}
            value={item.port}
            ref={portRef}
            onChange={(_, data) => onPatch(index, (c) => ({ ...c, port: data.value }))}
          />
        </Field>
      </div>

      <div className={styles.fields}>
        <Field label={t("fAuth")} className={styles.group}>
          <Dropdown
            value={isPassword ? t("authPassword") : t("authKey")}
            selectedOptions={[item.auth.method]}
            onOptionSelect={(_, data) => {
              const method = data.optionValue as AuthMethod | undefined;
              if (method) {
                onPatch(index, (c) => ({ ...c, auth: { ...c.auth, method } }));
              }
            }}
          >
            <Option value="password">{t("authPassword")}</Option>
            <Option value="key">{t("authKey")}</Option>
          </Dropdown>
        </Field>

        <Field label={t("fUser")} className={styles.group}>
          <Input
            className={styles.input}
            value={item.auth.username}
            onChange={(_, data) =>
              onPatch(index, (c) => ({ ...c, auth: { ...c.auth, username: data.value } }))
            }
          />
        </Field>

        {isPassword ? (
          <Field label={t("fPassword")} className={styles.group}>
            <Input
              className={styles.input}
              type="password"
              value={item.auth.password}
              placeholder={item.auth.hasPassword ? t("passwordKept") : ""}
              onChange={(_, data) =>
                onPatch(index, (c) => ({ ...c, auth: { ...c.auth, password: data.value } }))
              }
            />
          </Field>
        ) : (
          <>
          <Field label={t("fKeyFile")} className={mergeClasses(styles.group, styles.groupWide)}>
            <div className={styles.inline}>
              <Input
                className={styles.input}
                value={item.auth.keyPath}
                onChange={(_, data) =>
                  onPatch(index, (c) => ({ ...c, auth: { ...c.auth, keyPath: data.value } }))
                }
              />
              <Tooltip content={t("chooseKey")} relationship="description" withArrow>
                <Button
                  appearance="subtle"
                  icon={<FolderOpen20Regular />}
                  aria-label={t("chooseKey")}
                  onClick={() => void onPickKey(index)}
                />
              </Tooltip>
            </div>
          </Field>
          {/* Key passphrase: ssh-keygen encrypts keys by default, so this is a very common gate */}
          <Field
            label={t("fKeyPassphrase")}
            hint={item.auth.hasPassphrase ? t("passphraseKept") : undefined}
            className={mergeClasses(styles.group, styles.groupWide)}
          >
            <Input
              className={styles.input}
              type="password"
              autoComplete="off"
              value={item.auth.passphrase}
              placeholder={item.auth.hasPassphrase ? "••••••••" : ""}
              onChange={(_, data) =>
                onPatch(index, (c) => ({ ...c, auth: { ...c.auth, passphrase: data.value } }))
              }
            />
          </Field>
          </>
        )}
      </div>

      <div className={mergeClasses(styles.group, styles.tagsGroup)}>
        <div className={styles.label}>{t("fTags")}</div>
        <TagEditor
          tags={item.tags}
          addLabel={t("addTag")}
          deleteLabel={t("del")}
          onChange={(tags) => onPatch(index, (c) => ({ ...c, tags }))}
        />
      </div>

      <div className={styles.fields}>
        <Field
          label={t("baseDir")}
          hint={t("baseDirHint")}
          className={mergeClasses(styles.group, styles.groupWide)}
        >
          <Input
            className={styles.input}
            value={item.baseDir}
            placeholder={DEFAULT_BASE_DIR}
            onChange={(_, data) => onPatch(index, (c) => ({ ...c, baseDir: data.value }))}
          />
        </Field>
      </div>

      <div className={styles.fields}>
        <Checkbox
          checked={item.hostLock}
          label={t("clientHostLock")}
          onChange={(_, data) => onPatch(index, (c) => ({ ...c, hostLock: Boolean(data.checked) }))}
        />
      </div>

      <div className={styles.actions}>
        <Tooltip content={t("del")} relationship="description" withArrow>
          <Button
            appearance="subtle"
            icon={<Delete20Regular />}
            aria-label={t("del")}
            style={{ color: tokens.colorPaletteRedForeground1 }}
            onClick={onRequestDelete}
          />
        </Tooltip>
      </div>
    </Card>
  );
}
