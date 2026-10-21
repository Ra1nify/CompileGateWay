import type { JSX } from "react";
import {
  Button,
  Card,
  Checkbox,
  Dropdown,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Option,
  Switch,
  Tooltip,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { Copy16Regular, Save20Regular } from "@fluentui/react-icons";
import { useEffect, useState } from "react";
import {
  ApiError,
  apiSetConfig,
  type Config,
  type LanguageSetting,
  type Listen,
  type Status,
} from "../api";
import { makeT } from "../i18n/strings";

const useStyles = makeStyles({
  screen: {
    maxWidth: "1080px",
  },
  card: {
    maxWidth: "620px",
    display: "flex",
    flexDirection: "column",
    rowGap: "16px",
    padding: "22px 24px",
    borderRadius: tokens.borderRadiusXLarge,
  },
  title: {
    margin: "0 0 6px",
    fontSize: tokens.fontSizeBase500,
    fontWeight: tokens.fontWeightSemibold,
  },
  cardTitle: {
    margin: "0 0 4px",
    fontSize: tokens.fontSizeBase400,
    fontWeight: tokens.fontWeightSemibold,
  },
  inline: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
  },
  roots: {
    width: "100%",
    padding: "8px 10px",
    borderRadius: tokens.borderRadiusMedium,
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    background: tokens.colorNeutralBackground1,
    color: tokens.colorNeutralForeground1,
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase200,
    resize: "vertical",
  },
  subtitle: {
    margin: 0,
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  footer: {
    display: "flex",
    alignItems: "center",
    columnGap: "12px",
  },
});

const LANGUAGE_VALUES: LanguageSetting[] = ["auto", "zh-Hans", "en-US"];
const LISTEN_VALUES: Listen[] = ["lan", "wan"];

/**
 * Settings page. After a successful save the parent refetches /api/status, because
 * the language follows the server.
 */
export function SettingsPage({
  status,
  config,
  onSaved,
}: {
  status: Status;
  config: Config;
  onSaved: () => Promise<void> | void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);

  const [port, setPort] = useState(String(config.port));
  const [language, setLanguage] = useState<LanguageSetting>(config.language);
  const [listen, setListen] = useState<Listen>(config.listen);
  const [username, setUsername] = useState(config.username);
  const [password, setPassword] = useState("");
  const [clearPassword, setClearPassword] = useState(false);
  const [loginInterval, setLoginInterval] = useState(String(config.loginIntervalSeconds));
  const [logRetain, setLogRetain] = useState(String(config.logRetainDays));
  // Extra allowed host directories (one per line). Shared by browse / project import / build type detect.
  const [allowedRoots, setAllowedRoots] = useState(config.allowedRoots.join("\n"));
  // Host-level mutex (design doc §7.6.6, last item): takes effect immediately, like login-interval / log-retain
  const [hostLock, setHostLock] = useState(config.hostLock);
  // The MCP group is saved **independently**: it applies immediately, unlike the card
  // above whose fields need a restart, and mixing them would imply a port change restarts.
  const [mcpOn, setMcpOn] = useState(config.mcp.enabled);
  const [mcpListen, setMcpListen] = useState<Listen>(config.mcp.listen);
  const [mcpPort, setMcpPort] = useState(
    String(config.mcp.running ? config.mcp.port : config.mcp.port || 0),
  );
  const [mcpBusy, setMcpBusy] = useState(false);
  const [mcpOk, setMcpOk] = useState<string | null>(null);
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busy, setBusy] = useState(false);
  const [okText, setOkText] = useState<string | null>(null);
  const [errorText, setErrorText] = useState<string | null>(null);

  // Sync the server config back into the form when it changes (e.g. refetched after a language rename).
  useEffect(() => {
    setPort(String(config.port));
    setLanguage(config.language);
    setListen(config.listen);
    setUsername(config.username);
    setLoginInterval(String(config.loginIntervalSeconds));
    setLogRetain(String(config.logRetainDays));
    setAllowedRoots(config.allowedRoots.join("\n"));
    setHostLock(config.hostLock);
    setMcpOn(config.mcp.enabled);
    setMcpListen(config.mcp.listen);
    setMcpPort(String(config.mcp.running ? config.mcp.port : config.mcp.port || 0));
  }, [config]);

  async function save(): Promise<void> {
    setBusy(true);
    setOkText(null);
    setErrorText(null);

    const patch = {
      port: Number.parseInt(port, 10) || 0,
      language,
      listen,
      username,
      loginIntervalSeconds: Number.parseInt(loginInterval, 10) || 0,
      logRetainDays: Number.parseInt(logRetain, 10) || 7,
      allowedRoots: allowedRoots
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line.length > 0),
      hostLock,
      ...(password ? { password } : clearPassword ? { password: "" } : {}),
    };

    try {
      const result = await apiSetConfig(patch);
      setOkText(result.restartRequired ? t("savedRestart") : t("saved"));
      setPassword("");
      setClearPassword(false);
      await onSaved();
    } catch (err) {
      const message = err instanceof ApiError ? err.message : String(err);
      setErrorText(t("saveFailed", { msg: message }));
    } finally {
      setBusy(false);
    }
  }

  async function saveMcp(): Promise<void> {
    setMcpBusy(true);
    setMcpOk(null);
    setMcpError(null);
    try {
      const port = Number.parseInt(mcpPort, 10);
      await apiSetConfig({
        mcp: {
          enabled: mcpOn,
          listen: mcpListen,
          port: Number.isFinite(port) ? Math.min(Math.max(port, 0), 65535) : 0,
        },
      });
      setMcpOk(t("saved"));
      await onSaved();
    } catch (err) {
      // Errors like a busy port happen **right away** (it really binds), so show them truthfully.
      const message = err instanceof ApiError ? err.message : String(err);
      setMcpError(t("mcpError", { msg: message }));
    } finally {
      setMcpBusy(false);
    }
  }

  async function copyEndpoint(): Promise<void> {
    try {
      await navigator.clipboard.writeText(config.mcp.endpoint);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  }

  return (
    <div className={styles.screen}>
      <h2 className={styles.title}>{t("configTitle")}</h2>
      <p className={styles.subtitle}>{t("configSubtitle")}</p>

      <Card className={styles.card}>
        <Field label={t("port")} hint={t("portHint")}>
          <Input
            type="number"
            min={0}
            max={65535}
            value={port}
            onChange={(_, data) => setPort(data.value)}
            onBlur={() => {
              const parsed = Number.parseInt(port, 10);
              const safe = Number.isFinite(parsed) ? Math.min(Math.max(parsed, 0), 65535) : 0;
              setPort(String(safe));
            }}
          />
        </Field>

        <Field label={t("language")} hint={t("languageHint")}>
          <Dropdown
            value={language}
            selectedOptions={[language]}
            onOptionSelect={(_, data) => {
              const value = data.optionValue as LanguageSetting | undefined;
              if (value) {
                setLanguage(value);
              }
            }}
          >
            {LANGUAGE_VALUES.map((value) => (
              <Option key={value} value={value}>
                {value}
              </Option>
            ))}
          </Dropdown>
        </Field>

        <Field label={t("listen")} hint={t("restartNeeded")}>
          <Dropdown
            value={listen}
            selectedOptions={[listen]}
            onOptionSelect={(_, data) => {
              const value = data.optionValue as Listen | undefined;
              if (value) {
                setListen(value);
              }
            }}
          >
            {LISTEN_VALUES.map((value) => (
              <Option key={value} value={value}>
                {value === "lan" ? t("listenLan") : t("listenWan")}
              </Option>
            ))}
          </Dropdown>
        </Field>

        <Field label={t("loginInterval")} hint={t("loginIntervalHint")}>
          <Input
            type="number"
            min={0}
            max={86400}
            value={loginInterval}
            onChange={(_, data) => setLoginInterval(data.value)}
            onBlur={() => {
              const parsed = Number.parseInt(loginInterval, 10);
              const safe = Number.isFinite(parsed) ? Math.min(Math.max(parsed, 0), 86400) : 60;
              setLoginInterval(String(safe));
            }}
          />
        </Field>

        <Field label={t("logRetain")} hint={t("logRetainHint")}>
          <Input
            type="number"
            min={1}
            max={3650}
            value={logRetain}
            onChange={(_, data) => setLogRetain(data.value)}
            onBlur={() => {
              const parsed = Number.parseInt(logRetain, 10);
              const safe = Number.isFinite(parsed) ? Math.min(Math.max(parsed, 1), 3650) : 7;
              setLogRetain(String(safe));
            }}
          />
        </Field>

        <Field label={t("logDir")}>
          <Input value={config.logDir} readOnly />
        </Field>

        {/* browse / project import / build type detect **share** this root list -- when sources
            live on another drive, add a line here and all three follow. Previously only browse
            was restricted, which produced "browse says no but import says yes" contradictions. */}
        <Field label={t("allowedRoots")} hint={t("allowedRootsHint")}>
          <textarea
            className={styles.roots}
            rows={3}
            value={allowedRoots}
            spellCheck={false}
            placeholder={t("allowedRootsPlaceholder")}
            onChange={(event) => setAllowedRoots(event.target.value)}
          />
        </Field>
        <p className={styles.subtitle}>
          {t("hostRootsNow", { list: config.hostRoots.join("、") })}
        </p>

        {/* Host-level mutex (design doc §7.6.6, last item): projects could run in parallel, but
            Xcode / JDK / devices on the same Mac are shared resources. Applies immediately. */}
        <Switch
          checked={hostLock}
          label={t("hostLock")}
          onChange={(_, data) => setHostLock(data.checked)}
        />
        <p className={styles.subtitle}>{t("hostLockHint")}</p>

        <Field
          label={t("authUser")}
          hint={status.client.loopback ? undefined : t("authLocalOnly")}
        >
          <Input
            value={username}
            autoComplete="username"
            disabled={!status.client.loopback}
            onChange={(_, data) => setUsername(data.value)}
          />
        </Field>

        <Field label={t("authPass")} hint={t("authPassHint")}>
          <Input
            type="password"
            value={password}
            autoComplete="new-password"
            placeholder={config.hasPassword ? "••••••••" : ""}
            disabled={!status.client.loopback}
            onChange={(_, data) => setPassword(data.value)}
          />
        </Field>

        <Checkbox
          checked={clearPassword}
          label={t("authClear")}
          disabled={!status.client.loopback}
          onChange={(_, data) => setClearPassword(data.checked === true)}
        />

        <div className={styles.footer}>
          <Tooltip content={t("save")} relationship="description" withArrow>
            <Button
              appearance="primary"
              icon={<Save20Regular />}
              aria-label={t("save")}
              disabled={busy}
              onClick={() => void save()}
            />
          </Tooltip>
        </div>

        {okText ? (
          <MessageBar intent="success">
            <MessageBarBody>{okText}</MessageBarBody>
          </MessageBar>
        ) : null}
        {errorText ? (
          <MessageBar intent="error">
            <MessageBarBody>{errorText}</MessageBarBody>
          </MessageBar>
        ) : null}
      </Card>

      {/* ---- MCP adapter layer (design doc §12 Phase 6) ----------------------
          It is a **second port** next to the card above: that one is the WebUI (for
          humans), this one is MCP (for AI agents). Hence its own switches, applying at once. */}
      <Card className={styles.card}>
        <div>
          <h3 className={styles.cardTitle}>{t("mcpTitle")}</h3>
          <p className={styles.subtitle}>{t("mcpSubtitle")}</p>
        </div>

        <Switch
          checked={mcpOn}
          label={t("mcpEnable")}
          onChange={(_, data) => setMcpOn(data.checked)}
        />
        <p className={styles.subtitle}>{t("mcpEnableHint")}</p>

        <Field label={t("mcpListen")}>
          <Dropdown
            value={mcpListen}
            selectedOptions={[mcpListen]}
            onOptionSelect={(_, data) => {
              const value = data.optionValue as Listen | undefined;
              if (value) {
                setMcpListen(value);
              }
            }}
          >
            {LISTEN_VALUES.map((value) => (
              <Option key={value} value={value}>
                {value === "lan" ? t("listenLan") : t("listenWan")}
              </Option>
            ))}
          </Dropdown>
        </Field>

        <Field label={t("mcpPort")} hint={t("mcpPortHint")}>
          <Input
            type="number"
            min={0}
            max={65535}
            value={mcpPort}
            onChange={(_, data) => setMcpPort(data.value)}
            onBlur={() => {
              const parsed = Number.parseInt(mcpPort, 10);
              const safe = Number.isFinite(parsed) ? Math.min(Math.max(parsed, 0), 65535) : 0;
              setMcpPort(String(safe));
            }}
          />
        </Field>

        <Field label={t("mcpLive")}>
          <Input
            value={
              config.mcp.running
                ? `${config.mcp.endpoint}  ·  ${t("mcpTools", { n: String(config.mcp.tools) })}`
                : config.mcp.enabled
                  ? t("mcpNotRunning")
                  : t("mcpOff")
            }
            readOnly
          />
        </Field>

        {config.mcp.running ? (
          <Field label={t("mcpEndpoint")} hint={t("mcpEndpointHint")}>
            <div className={styles.inline}>
              <Input value={config.mcp.endpoint} readOnly />
              <Tooltip content={t("mcpCopy")} relationship="description" withArrow>
                <Button
                  icon={<Copy16Regular />}
                  aria-label={t("mcpCopy")}
                  onClick={() => void copyEndpoint()}
                />
              </Tooltip>
              {copied ? <span className={styles.subtitle}>{t("mcpCopied")}</span> : null}
            </div>
          </Field>
        ) : null}

        <div className={styles.footer}>
          <Tooltip content={t("save")} relationship="description" withArrow>
            <Button
              appearance="primary"
              icon={<Save20Regular />}
              aria-label={t("save")}
              disabled={mcpBusy}
              onClick={() => void saveMcp()}
            />
          </Tooltip>
          <span className={styles.subtitle}>{t("mcpApplyNow")}</span>
        </div>

        {status.mcpWarning ? (
          <MessageBar intent="warning">
            <MessageBarBody>{t("mcpWanWarning")}</MessageBarBody>
          </MessageBar>
        ) : null}
        {/* Say so when the default port gave way, or users wonder why an empty field runs elsewhere */}
        {config.mcp.note ? (
          <MessageBar intent="info">
            <MessageBarBody>{config.mcp.note}</MessageBarBody>
          </MessageBar>
        ) : null}
        {config.mcp.error ? (
          <MessageBar intent="error">
            <MessageBarBody>{t("mcpError", { msg: config.mcp.error })}</MessageBarBody>
          </MessageBar>
        ) : null}
        {mcpOk ? (
          <MessageBar intent="success">
            <MessageBarBody>{mcpOk}</MessageBarBody>
          </MessageBar>
        ) : null}
        {mcpError ? (
          <MessageBar intent="error">
            <MessageBarBody>{mcpError}</MessageBarBody>
          </MessageBar>
        ) : null}
      </Card>
    </div>
  );
}
