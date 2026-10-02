import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key, type Lang } from "../i18n";
import { usePreferences, usePro, useSetPreferences } from "../state/prefs";
import { navigate } from "../state/route";
import { useClient, useLive, useOp, useRefresh } from "../state/store";
import { ResourceMeter, useResources } from "./Cockpit";
import { ErrorNote, formatRam } from "./ui";

type Health = OpOutput<"system_health">;
type Fix = Health["fixes"][number];
type Level = Health["level"];

const RANK: Record<Level, number> = { ok: 0, tight: 1, critical: 2 };
/** After "later", the warning stays away this long – unless it gets worse. */
const SNOOZE_MS = 15 * 60_000;
/** Unloads by the guard younger than this are reported. */
const RECENT_MS = 30 * 60_000;
const SEEN_KEY = "ancilo.guardSeen";

/** The system monitor's verdict, looked at every few seconds (and at once when the guard sees a change). */
export function useHealth() {
  const client = useClient();
  return useQuery<Health>({
    queryKey: ["system_health"],
    queryFn: () => client.op("system_health"),
    refetchInterval: 5000,
    retry: false,
  });
}

function openCockpit() {
  navigate({ view: "system" });
  window.setTimeout(() => document.getElementById("cockpit")?.scrollIntoView({ behavior: "smooth" }), 50);
}

/** Simple or expert view – one switch. */
function ViewSwitch() {
  const { t } = useI18n();
  const prefs = usePreferences();
  const set = useSetPreferences();
  const pro = prefs.data?.view === "pro";
  return (
    <label className="view-switch" title={t("view.hint")}>
      <input type="checkbox" role="switch" checked={pro} onChange={(e) => void set({ view: e.target.checked ? "pro" : "simple" })} />
      <span>{t("view.pro")}</span>
    </label>
  );
}

function LanguageChoice() {
  const { t, lang, setLang } = useI18n();
  return (
    <label className="lang">
      <span className="sr-only">{t("lang.label")}</span>
      <select value={lang} onChange={(e) => setLang(e.target.value as Lang)}>
        <option value="de">DE</option>
        <option value="en">EN</option>
      </select>
    </label>
  );
}

/** The app's own settings, at the right end of the status bar. */
function Settings({ online }: { online: boolean }) {
  const { t } = useI18n();
  return (
    <span className="status-settings" role="group" aria-label={t("monitor.settings")}>
      {online && <ViewSwitch />}
      <LanguageChoice />
    </span>
  );
}

/** One line at the bottom of the window: how the computer is doing (left), the app's settings (right). */
export function StatusBar() {
  const { t } = useI18n();
  const live = useLive();
  const health = useHealth();
  const resources = useResources();
  const pro = usePro();
  const h = health.data;
  const st = resources.data;
  if (!live.online) {
    return (
      <footer className="statusbar" aria-label={t("monitor.label")}>
        <span className="status-item">
          <span className="dot dot-crashed" aria-hidden="true" />
          {t("status.offline")}
        </span>
        <span className="spacer" />
        <Settings online={false} />
      </footer>
    );
  }
  const level = h?.level ?? "ok";
  const swap = st?.system.swap_used_bytes ?? 0;
  return (
    <footer className={`statusbar ${level}`} aria-label={t("monitor.label")} data-testid="statusbar">
      <span className="status-item verdict" data-testid="verdict">
        <span className={`dot ${level === "ok" ? "dot-ok" : level === "tight" ? "dot-queued" : "dot-failed"}`} aria-hidden="true" />
        {h ? t(`monitor.level.${level}` as Key) : "…"}
      </span>
      {h?.memory_percent != null && <span className={h.causes.includes("memory") ? "status-item hot" : "status-item"}>{t("monitor.memory", { n: h.memory_percent })}</span>}
      {h?.cpu_percent != null && <span className={h.causes.includes("cpu") ? "status-item hot" : "status-item"}>{t("monitor.cpu", { n: h.cpu_percent })}</span>}
      {h && h.thermal !== "nominal" && h.thermal !== "unknown" && (
        <span className={h.causes.includes("heat") ? "status-item hot" : "status-item"}>{t(`cockpit.thermal.${h.thermal}` as Key)}</span>
      )}
      {pro && swap > 2 ** 30 && <span className="status-item muted">{t("cockpit.swap", { size: formatRam(swap) })}</span>}
      <span className="status-sep" aria-hidden="true" />
      <ResourceMeter />
      {st && (
        <button type="button" className="status-item link-like" title={t("cockpit.open")} onClick={openCockpit}>
          {t(`cockpit.level.${st.settings.level}` as Key)}
        </button>
      )}
      <span className="spacer" />
      <Settings online />
    </footer>
  );
}

function FixButton({ fix, onDone }: { fix: Fix; onDone: (e?: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      if (fix.kind === "unload_models") await client.op("unload_models", {});
      else if (fix.kind === "level_eco") await client.op("set_resources", { level: "eco" });
      else await client.op("open_activity_monitor", {});
      await refresh("system_health", "resource_status", "list_models");
      onDone();
    } catch (e) {
      onDone(e);
    } finally {
      setBusy(false);
    }
  };
  const label =
    fix.kind === "unload_models" ? t("monitor.fix.unload", { size: formatRam(fix.frees_bytes) }) : fix.kind === "level_eco" ? t("monitor.fix.eco") : t("monitor.fix.activity");
  return (
    <button type="button" className={fix.kind === "activity_monitor" ? "secondary" : undefined} disabled={busy} onClick={() => void run()}>
      {label}
    </button>
  );
}

/** The warning when the computer gets tight – what causes it, and one click that helps. */
function Warning({ h, onLater }: { h: Health; onLater: () => void }) {
  const { t } = useI18n();
  const resources = useResources();
  const [error, setError] = useState<unknown>(null);
  const max = resources.data?.settings.level === "max";
  const cause = h.causes[0];
  const others = h.consumers.map((c) => (cause === "cpu" ? `${c.name} (${Math.round(c.cpu_percent)} %)` : `${c.name} (${formatRam(c.memory_bytes)})`));
  return (
    <section className={`monitor-card ${h.level}`} role="alert" aria-labelledby="monitor-title" data-testid="monitor-warning">
      <h2 id="monitor-title">{t(`monitor.title.${h.level}` as Key)}</h2>
      <p>{h.causes.map((c) => t(`monitor.cause.${c}` as Key)).join(" ")}</p>
      {h.mostly_others && others.length > 0 && <p>{t("monitor.others", { list: others.join(", ") })}</p>}
      {!h.mostly_others && h.ancilo_bytes > 0 && <p>{t("monitor.ancilo", { size: formatRam(h.ancilo_bytes) })}</p>}
      {h.level === "critical" && max && <p className="muted small">{t("monitor.maxNoAction")}</p>}
      {h.fixes.length === 0 && <p className="muted small">{t("monitor.noFix")}</p>}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <div className="row">
        {h.fixes.map((f) => (
          <FixButton key={f.kind} fix={f} onDone={(e) => setError(e ?? null)} />
        ))}
        <button type="button" className="ghost" onClick={onLater}>
          {t("monitor.later")}
        </button>
      </div>
    </section>
  );
}

type GuardAction = OpOutput<"resource_status">["recent"][number];

function readSeen(): number {
  try {
    return Number(localStorage.getItem(SEEN_KEY) ?? 0);
  } catch {
    return 0;
  }
}

/** What Ancilo did by itself in an emergency – with "load again". */
function Acted({ actions, onClose }: { actions: GuardAction[]; onClose: () => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const models = useOp("list_models");
  const [error, setError] = useState<unknown>(null);
  const name = (id: string) => models.data?.find((m) => m.id === id)?.name ?? id;
  const names = [...new Set(actions.map((a) => name(a.model)))].join(", ");
  const heat = actions.some((a) => a.reason === "heat");
  return (
    <section className="monitor-card acted" role="status" data-testid="monitor-acted">
      <h2>{t("monitor.acted.title")}</h2>
      <p>{t(heat ? "monitor.acted.heat" : "monitor.acted.memory", { models: names })}</p>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <div className="row">
        <button
          type="button"
          className="secondary"
          onClick={async () => {
            setError(null);
            try {
              for (const id of new Set(actions.map((a) => a.model))) await client.op("start_model", { model: id });
              await refresh("resource_status", "list_models", "system_health");
              onClose();
            } catch (e) {
              setError(e);
            }
          }}
        >
          {t("monitor.acted.undo")}
        </button>
        <button type="button" onClick={onClose}>
          {t("monitor.acted.ok")}
        </button>
      </div>
    </section>
  );
}

/**
 * Warnings over the status bar: when the computer gets tight (with fixes),
 * and when Ancilo acted by itself in an emergency (with undo).
 */
export function MonitorAlerts() {
  const live = useLive();
  const health = useHealth();
  const resources = useResources();
  const [snoozed, setSnoozed] = useState<{ level: Level; until: number } | null>(null);
  const [seen, setSeen] = useState(readSeen);
  const [now, setNow] = useState(() => Date.now());
  // The snooze runs out by itself.
  useEffect(() => {
    if (!snoozed) return;
    const id = window.setTimeout(() => setNow(Date.now()), Math.max(0, snoozed.until - Date.now()) + 50);
    return () => window.clearTimeout(id);
  }, [snoozed]);
  if (!live.online) return null;
  const h = health.data;
  const acted = (resources.data?.recent ?? []).filter((a) => {
    const at = Date.parse(a.at);
    return a.reason !== "idle" && at > seen && at > Date.now() - RECENT_MS;
  });
  const quiet = snoozed !== null && now < snoozed.until && h !== undefined && RANK[h.level] <= RANK[snoozed.level];
  const warn = h && h.level !== "ok" && !quiet;
  if (!warn && acted.length === 0) return null;
  return (
    <div className="monitor-alerts">
      {acted.length > 0 && (
        <Acted
          actions={acted}
          onClose={() => {
            const latest = Math.max(...acted.map((a) => Date.parse(a.at)));
            setSeen(latest);
            try {
              localStorage.setItem(SEEN_KEY, String(latest));
            } catch {
              // Then it shows again next time – harmless.
            }
          }}
        />
      )}
      {warn && <Warning h={h} onLater={() => setSnoozed({ level: h.level, until: Date.now() + SNOOZE_MS })} />}
    </div>
  );
}
