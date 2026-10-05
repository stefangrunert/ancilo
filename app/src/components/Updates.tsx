import { useCallback, useEffect, useState } from "react";
import { useI18n } from "../i18n";
import { useClient, useOp, useRefresh } from "../state/store";
import { Icon } from "./Icon";
import { Dialog, ErrorNote } from "./ui";

/** What the app's updater says (Tauri, `update_state`). */
interface UpdateView {
  configured: boolean;
  current: string;
  available: { version: string; notes: string | null } | null;
  checking: boolean;
}

const native = () => window.__TAURI_INTERNALS__;

/** Without a check for this long, "Check for updates" reminds a little. */
const STALE_DAYS = 30;

/**
 * The updater lives in the app (not in the daemon): what it found, looking
 * for an update, installing it – each only on the user's click, unless they
 * allowed the daily check. Nothing outside the desktop app (and nothing in
 * Ancilo Dev, which never updates).
 */
export function useUpdates() {
  const [view, setView] = useState<UpdateView | null>(null);
  const [upToDate, setUpToDate] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const refresh = useRefresh();
  const read = useCallback(async () => {
    const n = native();
    if (!n || !window.__ANCILO__?.app) return;
    try {
      setView((await n.invoke("update_state")) as UpdateView);
    } catch {
      setView(null);
    }
  }, []);
  useEffect(() => {
    void read();
    // The daily check runs in the app – what it finds shows up here.
    const timer = setInterval(() => void read(), 60_000);
    return () => clearInterval(timer);
  }, [read]);
  const check = async () => {
    const n = native();
    if (!n) return;
    setError(null);
    setView((v) => (v ? { ...v, checking: true } : v));
    try {
      const v = (await n.invoke("check_update")) as UpdateView;
      setView(v);
      if (!v.available) {
        setUpToDate(true);
        setTimeout(() => setUpToDate(false), 4000);
      }
    } catch (e) {
      setError(e);
      await read();
    }
    await refresh("get_update_settings");
  };
  const install = async () => {
    await native()?.invoke("install_update");
  };
  return { view: view?.configured ? view : null, upToDate, error, setError, check, install };
}

/** Whether the last look for an update is long ago (a gentle reminder). */
function stale(lastChecked: string | null | undefined): boolean {
  const key = "ancilo.updates.firstSeen";
  let since = lastChecked ? Date.parse(lastChecked) : NaN;
  if (Number.isNaN(since)) {
    try {
      since = Number(localStorage.getItem(key)) || 0;
      if (!since) {
        since = Date.now();
        localStorage.setItem(key, String(since));
      }
    } catch {
      return false;
    }
  }
  return Date.now() - since > STALE_DAYS * 86_400_000;
}

/** The update's notes, short: the first few points, without Markdown. */
function shortNotes(notes: string | null): string[] {
  return (notes ?? "")
    .split("\n")
    .filter((l) => l.startsWith("- "))
    .slice(0, 3)
    .map((l) => l.slice(2).replace(/\*\*|\*|`/g, "").split(/[.:](\s|$)/)[0] ?? "");
}

/** Installing an update restarts Ancilo – what runs right now would stop. */
function UpdateDialog({ open, onClose, version, notes, install }: { open: boolean; onClose: () => void; version: string; notes: string | null; install: () => Promise<void> }) {
  const { t } = useI18n();
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const sessions = useOp("list_sessions", undefined, open);
  const busy = (sessions.data ?? []).some((s: { status?: string }) => s.status === "running");
  return (
    <Dialog open={open} title={t("updates.dialogTitle", { version })} onClose={() => !installing && onClose()}>
      {shortNotes(notes).length > 0 && (
        <>
          <p className="small muted">{t("updates.whatsNew")}</p>
          <ul className="small">
            {shortNotes(notes).map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </>
      )}
      <p>{t("updates.restartHint")}</p>
      {busy && (
        <p className="note warn" role="alert">
          {t("updates.busy")}
        </p>
      )}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <div className="row end">
        <button type="button" className="secondary" disabled={installing} onClick={onClose}>
          {t("updates.later")}
        </button>
        <button
          type="button"
          disabled={installing}
          data-testid="install-update"
          onClick={async () => {
            setInstalling(true);
            try {
              await install();
            } catch (e) {
              setError(e);
              setInstalling(false);
            }
          }}
        >
          {installing ? t("updates.installing") : t("updates.install")}
        </button>
      </div>
    </Dialog>
  );
}

/**
 * The header's update button: "Update to 0.4.5" when one is there; while
 * Ancilo does not look by itself, "Check for updates" (a little highlighted
 * after a month without a look); nothing while it looks by itself and all is
 * current.
 */
export function UpdateButton() {
  const { t } = useI18n();
  const { view, upToDate, error, check, install } = useUpdates();
  const settings = useOp("get_update_settings", undefined, Boolean(view));
  const [open, setOpen] = useState(false);
  if (!view || !settings.data) return null;
  if (view.available) {
    return (
      <>
        <button type="button" className="header-link update-ready" data-testid="update-button" onClick={() => setOpen(true)}>
          <Icon name="update" />
          <span>{t("updates.available", { version: view.available.version })}</span>
        </button>
        <UpdateDialog open={open} onClose={() => setOpen(false)} version={view.available.version} notes={view.available.notes} install={install} />
      </>
    );
  }
  if (settings.data.auto_check && !view.checking && !upToDate && !error) return null;
  const label = view.checking ? t("updates.checking") : upToDate ? t("updates.upToDate") : error ? t("updates.failed") : t("updates.check");
  return (
    <button
      type="button"
      className={stale(settings.data.last_checked) ? "header-link update-stale" : "header-link"}
      data-testid="update-button"
      disabled={view.checking}
      onClick={() => void check()}
    >
      <Icon name="update" />
      <span>{label}</span>
    </button>
  );
}

/**
 * System › Updates (and in the setup): this version, the switch for the
 * daily look, looking now, installing what was found.
 */
export function UpdatesPanel() {
  const { t, lang } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const { view, upToDate, error, setError, check, install } = useUpdates();
  const settings = useOp("get_update_settings", undefined, Boolean(view));
  const [open, setOpen] = useState(false);
  if (!view || !settings.data) return null;
  const last = settings.data.last_checked;
  return (
    <section className="panel" aria-labelledby="panel-updates" data-testid="updates">
      <header>
        <h2 id="panel-updates">{t("updates.title")}</h2>
      </header>
      <p>
        {t("updates.version", { version: view.current })}
        {" · "}
        <span className="muted">
          {last
            ? t("updates.lastChecked", { when: new Date(last).toLocaleDateString(lang, { day: "numeric", month: "short", year: "numeric" }) })
            : t("updates.neverChecked")}
        </span>
      </p>
      <label className="row">
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
      <p className="hint">{t("updates.hint")}</p>
      <div className="row">
        {view.available ? (
          <button type="button" onClick={() => setOpen(true)}>
            {t("updates.available", { version: view.available.version })}
          </button>
        ) : (
          <button type="button" className="secondary" disabled={view.checking} onClick={() => void check()}>
            {view.checking ? t("updates.checking") : upToDate ? t("updates.upToDate") : t("updates.check")}
          </button>
        )}
      </div>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      {view.available && <UpdateDialog open={open} onClose={() => setOpen(false)} version={view.available.version} notes={view.available.notes} install={install} />}
    </section>
  );
}
