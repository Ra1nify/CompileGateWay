import type { JSX } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
  makeStyles,
  tokens,
} from "@fluentui/react-components";
import { LockClosedRegular, PersonRegular } from "@fluentui/react-icons";
import { useState } from "react";
import { ApiError, apiLogin, type Status } from "../api";
import { makeT } from "../i18n/strings";

const useStyles = makeStyles({
  screen: {
    display: "grid",
    placeItems: "center",
    minHeight: "100vh",
    padding: "24px",
  },
  card: {
    width: "min(92vw, 360px)",
    display: "flex",
    flexDirection: "column",
    rowGap: "14px",
    padding: "26px 26px 22px",
  },
  brand: {
    display: "flex",
    alignItems: "center",
    columnGap: "8px",
    fontWeight: tokens.fontWeightSemibold,
    fontSize: tokens.fontSizeBase400,
  },
  logo: {
    fontFamily: tokens.fontFamilyMonospace,
    color: tokens.colorBrandForeground1,
    fontWeight: tokens.fontWeightBold,
  },
  title: {
    margin: 0,
    fontSize: tokens.fontSizeBase400,
    color: tokens.colorNeutralForeground2,
  },
  submit: {
    justifyContent: "center",
  },
});

/**
 * Login gate: only this screen shows while status.auth.required && !status.client.authenticated.
 */
export function LoginPage({
  status,
  onAuthenticated,
}: {
  status: Status;
  onAuthenticated: () => Promise<void> | void;
}): JSX.Element {
  const styles = useStyles();
  const t = makeT(status.language);
  const [username, setUsername] = useState(status.auth.username);
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await apiLogin(username, password);
      await onAuthenticated();
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) {
        setError(t("loginRateLimited", { s: err.retryAfterSeconds ?? 60 }));
      } else if (err instanceof ApiError && err.code === "rateLimited") {
        setError(t("loginRateLimited", { s: err.retryAfterSeconds ?? 60 }));
      } else {
        setError(t("loginFailed"));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.screen}>
      <Card className={styles.card}>
        <div className={styles.brand}>
          <span className={styles.logo}>&gt;_</span>
          <span>{status.app}</span>
        </div>
        <h1 className={styles.title}>{t("signIn")}</h1>

        {error ? (
          <MessageBar intent="error">
            <MessageBarBody>{error}</MessageBarBody>
          </MessageBar>
        ) : null}

        <Field label={t("authUser")} required>
          <Input
            value={username}
            autoComplete="username"
            autoFocus
            contentBefore={<PersonRegular />}
            onChange={(_, data) => setUsername(data.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                void submit();
              }
            }}
          />
        </Field>

        <Field label={t("authPass")} required>
          <Input
            type="password"
            value={password}
            autoComplete="current-password"
            contentBefore={<LockClosedRegular />}
            onChange={(_, data) => setPassword(data.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                void submit();
              }
            }}
          />
        </Field>

        <Button
          className={styles.submit}
          appearance="primary"
          disabled={busy}
          icon={busy ? <Spinner size="tiny" /> : undefined}
          onClick={() => void submit()}
        >
          {t("ok")}
        </Button>
      </Card>
    </div>
  );
}
