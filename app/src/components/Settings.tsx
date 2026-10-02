import { useState } from "react";
import { useI18n } from "../i18n";
import { useClient, useOp, useRefresh } from "../state/store";
import { ErrorNote, StatusDot } from "./ui";

type Access = "read" | "edit" | "shell";

/** What delegated tasks may do at most. */
export function Permissions() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const perms = useOp("get_permissions");
  const [error, setError] = useState<unknown>(null);
  const current = perms.data?.max_access as Access | undefined;
  return (
    <fieldset className="row radios">
      <legend className="label">{t("allow.label")}</legend>
      {(["read", "edit", "shell"] as const).map((a) => (
        <label key={a}>
          <input
            type="radio"
            name="allow"
            value={a}
            checked={current === a}
            onChange={async () => {
              try {
                await client.op("set_permissions", { max_access: a });
                await refresh("get_permissions");
              } catch (e) {
                setError(e);
              }
            }}
          />
          {t(`allow.${a}`)}
        </label>
      ))}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </fieldset>
  );
}

const CLIENTS = [
  { id: "claude_code", connect: "connect_claude_code", disconnect: "disconnect_claude_code" },
  { id: "codex", connect: "connect_codex", disconnect: "disconnect_codex" },
] as const;

/** Let Claude Code and Codex delegate to Ancilo – a line each, like the models: state and one click. */
export function Connect() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const state = useOp("connections");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ client: string; e: unknown } | null>(null);
  return (
    <div className="stack-tight" role="group" aria-label={t("connect.label")}>
      {CLIENTS.map((c) => {
        const connected = state.data?.find((s) => s.client === c.id)?.connected ?? false;
        const name = t(`connect.${c.id}`);
        return (
          <div key={c.id} className="model-line" data-testid={`connection-${c.id}`}>
            <div className="row">
              <StatusDot status={connected ? "running" : "idle"} />
              <strong>{name}</strong>
              <span className="muted">{connected ? t("connect.connected") : t("connect.notConnected")}</span>
              <span className="spacer" />
              <button
                type="button"
                className={connected ? "secondary" : undefined}
                aria-label={t(connected ? "connect.disconnectAria" : "connect.connectAria", { client: name })}
                disabled={busy !== null || !state.data}
                onClick={async () => {
                  setBusy(c.id);
                  setError(null);
                  try {
                    // The click is the user's decision to change the other program's configuration.
                    await client.op(connected ? c.disconnect : c.connect, {}, true);
                    await refresh("connections");
                  } catch (e) {
                    setError({ client: c.id, e });
                  } finally {
                    setBusy(null);
                  }
                }}
              >
                {busy === c.id ? "…" : connected ? t("connect.disconnect") : t("connect.connect")}
              </button>
            </div>
          </div>
        );
      })}
      {error && (
        <ErrorNote
          error={new Error(t("connect.failed", { client: t(`connect.${error.client}` as "connect.codex"), message: (error.e as Error).message }))}
          onDismiss={() => setError(null)}
        />
      )}
    </div>
  );
}

/**
 * Only in the desktop app: may it look for updates automatically? Checking
 * is network traffic, so it is off until the user allows it; "Check for
 * Updates…" in the menu works either way.
 */
export function Updates() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const settings = useOp("get_update_settings", undefined, Boolean(window.__ANCILO__?.app));
  const [error, setError] = useState<unknown>(null);
  if (!window.__ANCILO__?.app || !settings.data) return null;
  return (
    <div className="row">
      <label>
        <input
          type="checkbox"
          checked={settings.data.auto_check}
          onChange={async (e) => {
            try {
              await client.op("set_update_settings", { auto_check: e.target.checked });
              await refresh("get_update_settings");
            } catch (err) {
              setError(err);
            }
          }}
        />
        {t("updates.auto")}
      </label>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </div>
  );
}
