import type { JSX } from "react";
import {
  Button,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  Spinner,
} from "@fluentui/react-components";
import { Lock, Terminal, User } from "lucide-react";
import { useState } from "react";
import { ApiError, apiLogin, type Status } from "../api";
import { makeT } from "../i18n/strings";

/**
 * Login gate: modern developer console authentication screen.
 */
export function LoginPage({
  status,
  onAuthenticated,
}: {
  status: Status;
  onAuthenticated: () => Promise<void> | void;
}): JSX.Element {
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
    <div className="min-h-screen bg-background flex flex-col items-center justify-center p-4">
      <div className="w-full max-w-sm bg-card border border-border rounded-2xl p-6 sm:p-8 shadow-xl flex flex-col gap-5">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-xl bg-primary/10 border border-primary/20 flex items-center justify-center text-primary shadow-inner">
            <Terminal className="w-5 h-5" />
          </div>
          <div>
            <h1 className="text-base font-bold text-text tracking-tight">{status.app}</h1>
            <p className="text-xs text-text-muted">{t("signIn")}</p>
          </div>
        </div>

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
            contentBefore={<User className="w-4 h-4 text-text-muted" />}
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
            contentBefore={<Lock className="w-4 h-4 text-text-muted" />}
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
          style={{ width: "100%", justifyContent: "center", marginTop: "4px" }}
          appearance="primary"
          disabled={busy}
          icon={busy ? <Spinner size="tiny" /> : undefined}
          onClick={() => void submit()}
        >
          {t("ok")}
        </Button>
      </div>
    </div>
  );
}
