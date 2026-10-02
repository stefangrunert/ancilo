import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { usePro } from "../state/prefs";
import { navigate } from "../state/route";
import { useClient, useRefresh } from "../state/store";
import { LevelSlider } from "./LevelSlider";
import { ErrorNote, formatRam } from "./ui";

type Status = OpOutput<"resource_status">;
type Settings = Status["settings"];
type Level = Settings["level"];

const LEVELS: Level[] = ["eco", "balanced", "performance", "max"];

/** The cockpit's state, refreshed every few seconds (memory and heat change by themselves). */
export function useResources() {
  const client = useClient();
  return useQuery<Status>({
    queryKey: ["resource_status"],
    queryFn: () => client.op("resource_status"),
    refetchInterval: 4000,
    retry: false,
  });
}

function minutes(secs: number | null | undefined, t: (k: Key, v?: Record<string, string | number>) => string): string {
  if (secs === null || secs === undefined) return t("cockpit.always");
  if (secs < 60) return t("cockpit.seconds", { n: secs });
  return secs % 3600 === 0 ? t("cockpit.hours", { n: secs / 3600 }) : t("cockpit.minutes", { n: Math.round(secs / 60) });
}

function Tiles({ s }: { s: Settings }) {
  const { t } = useI18n();
  const tiles: [Key, string][] = [
    ["cockpit.keep", minutes(s.keep_loaded_secs, t)],
    ["cockpit.share", `${Math.round(s.max_share * 100)} %`],
    ["cockpit.parallel", String(s.parallel)],
    ["cockpit.priority", t(`cockpit.priority.${s.priority}` as Key)],
    ["cockpit.variant", t(`cockpit.variant.${s.variant}` as Key)],
    ["cockpit.guard", t(`cockpit.guard.${s.guard}` as Key)],
  ];
  return (
    <div className="tiles">
      {tiles.map(([k, v]) => (
        <div key={k} className="tile">
          <span className="muted small">{t(k)}</span>
          <strong>{v}</strong>
        </div>
      ))}
    </div>
  );
}

function FineTuning({ s, onSet }: { s: Settings; onSet: (input: Record<string, unknown>) => void }) {
  const { t } = useI18n();
  const keep = s.keep_loaded_secs === null || s.keep_loaded_secs === undefined ? "always" : String(s.keep_loaded_secs);
  return (
    <div className="fine">
      <label>
        <span>{t("cockpit.keep")}</span>
        <select
          value={keep}
          onChange={(e) => onSet(e.target.value === "always" ? { keep_loaded_always: true } : { keep_loaded_secs: Number(e.target.value) })}
        >
          {[300, 900, 3600].map((v) => (
            <option key={v} value={v}>
              {minutes(v, t)}
            </option>
          ))}
          {![300, 900, 3600].includes(Number(keep)) && keep !== "always" && <option value={keep}>{minutes(Number(keep), t)}</option>}
          <option value="always">{t("cockpit.always")}</option>
        </select>
      </label>
      <label>
        <span>{t("cockpit.share")}</span>
        <select value={String(s.max_share)} onChange={(e) => onSet({ max_share: Number(e.target.value) })}>
          {[0.35, 0.55, 0.7, 0.9].concat([0.35, 0.55, 0.7, 0.9].includes(s.max_share) ? [] : [s.max_share]).map((v) => (
            <option key={v} value={v}>
              {Math.round(v * 100)} %
            </option>
          ))}
        </select>
      </label>
      <label>
        <span>{t("cockpit.parallel")}</span>
        <select value={s.parallel} onChange={(e) => onSet({ parallel: Number(e.target.value) })}>
          {[1, 2, 3, 4].map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      </label>
      <label>
        <span>{t("cockpit.priority")}</span>
        <select value={s.priority} onChange={(e) => onSet({ priority: e.target.value })}>
          {(["background", "low", "normal"] as const).map((v) => (
            <option key={v} value={v}>
              {t(`cockpit.priority.${v}`)}
            </option>
          ))}
        </select>
      </label>
      <label>
        <span>{t("cockpit.variant")}</span>
        <select value={s.variant} onChange={(e) => onSet({ variant: e.target.value })}>
          {(["small", "auto", "precise"] as const).map((v) => (
            <option key={v} value={v}>
              {t(`cockpit.variant.${v}`)}
            </option>
          ))}
        </select>
      </label>
      <label>
        <span>{t("cockpit.guard")}</span>
        <select value={s.guard} onChange={(e) => onSet({ guard: e.target.value })}>
          {(["unload", "unload_when_tight", "warn"] as const).map((v) => (
            <option key={v} value={v}>
              {t(`cockpit.guard.${v}`)}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}

/** Memory now: other programs, Ancilo, free – and Ancilo's limit. */
function MemoryBar({ st }: { st: Status }) {
  const { t } = useI18n();
  const total = st.total_bytes;
  const free = st.system.available_bytes ?? null;
  const ancilo = st.used_bytes;
  const others = free === null ? null : Math.max(0, total - free - ancilo);
  const pct = (b: number) => `${Math.min(100, (b / total) * 100)}%`;
  return (
    <div className="membar-wrap">
      <div className="membar" role="img" aria-label={t("cockpit.memoryAria", { ancilo: formatRam(ancilo), total: formatRam(total) })}>
        {/* Ancilo first: the limit marker then measures Ancilo's own share. */}
        <span className="seg ancilo" style={{ width: pct(ancilo) }} />
        {others !== null && <span className="seg others" style={{ width: pct(others) }} />}
        <span className="cap" style={{ left: pct(st.cap_bytes) }} title={t("cockpit.capHint")} />
      </div>
      <div className="legend small">
        <span>
          <i className="dot-l ancilo" /> {t("cockpit.ancilo", { size: formatRam(ancilo) })}
        </span>
        {others !== null && (
          <span>
            <i className="dot-l others" /> {t("cockpit.others", { size: formatRam(others) })}
          </span>
        )}
        {free !== null && <span>{t("cockpit.free", { size: formatRam(free) })}</span>}
        <span className="muted">{t("cockpit.cap", { size: formatRam(st.cap_bytes) })}</span>
      </div>
    </div>
  );
}

function stateText(st: Status, t: (k: Key, v?: Record<string, string | number>) => string) {
  const p = st.system.pressure;
  const th = st.system.thermal;
  return {
    memory: t(`cockpit.pressure.${p}` as Key),
    memoryBad: p === "warn" || p === "critical",
    heat: t(`cockpit.thermal.${th}` as Key),
    heatBad: th === "heavy" || th === "critical",
  };
}

/**
 * How much of the computer Ancilo may take: one slider from "eco" to "max",
 * the values it sets, and what the computer looks like right now.
 */
export function Cockpit() {
  const { t, lang } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const status = useResources();
  const pro = usePro();
  const [error, setError] = useState<unknown>(null);
  // The level nearest to the handle while it moves (its text shows at once).
  const [preview, setPreview] = useState<number | null>(null);
  const st = status.data;
  if (status.error) return <ErrorNote error={status.error} />;
  if (!st) return null;
  const s = st.settings;
  const set = async (input: Record<string, unknown>) => {
    setError(null);
    try {
      await client.op("set_resources", input);
      await refresh("resource_status", "recommend_models");
    } catch (e) {
      setError(e);
    }
  };
  const level = preview ?? LEVELS.indexOf(s.level);
  const shown = LEVELS[level] ?? s.level;
  const now = stateText(st, t);
  const swap = st.system.swap_used_bytes ?? 0;
  return (
    <section className="panel wide cockpit" id="cockpit" aria-labelledby="cockpit-title" data-testid="cockpit">
      <header>
        <h2 id="cockpit-title">{t("cockpit.title")}</h2>
        {s.custom && <span className="badge">{t("cockpit.custom")}</span>}
      </header>
      <div className="slider">
        <LevelSlider
          levels={LEVELS.length}
          value={LEVELS.indexOf(s.level)}
          label={t("cockpit.title")}
          valueText={(i) => t(`cockpit.level.${LEVELS[i]}` as Key)}
          onPreview={setPreview}
          onChange={(i) => void set({ level: LEVELS[i] }).finally(() => setPreview(null))}
        />
        <div className="ticks">
          {LEVELS.map((l, i) => (
            <button key={l} type="button" className={i === level ? "tick on" : "tick"} onClick={() => void set({ level: l })}>
              {t(`cockpit.level.${l}` as Key)}
            </button>
          ))}
        </div>
      </div>
      <p>{t(`cockpit.describe.${shown}` as Key)}</p>
      {pro && (
        <>
          <Tiles s={s} />
          <details className="section bare">
            <summary>{t("cockpit.fine")}</summary>
            <div className="section-body">
              <FineTuning s={s} onSet={(i) => void set(i)} />
            </div>
          </details>
        </>
      )}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <div className="now">
        <h3>{t("cockpit.now")}</h3>
        <MemoryBar st={st} />
        <p className="small">
          <span className={now.memoryBad ? "outcome bad" : ""}>{now.memory}</span>
          {" · "}
          <span className={now.heatBad ? "outcome bad" : ""}>{now.heat}</span>
          {swap > 2 ** 30 && <span className="muted"> · {t("cockpit.swap", { size: formatRam(swap) })}</span>}
        </p>
        {st.loaded.length === 0 ? (
          <p className="muted small">{t("cockpit.nothingLoaded")}</p>
        ) : (
          <ul className="loaded">
            {st.loaded.map((m) => (
              <li key={m.id}>
                <strong>{m.name}</strong>
                <span className="muted small">{formatRam(m.ram_bytes)}</span>
                <span className="muted small">
                  {m.busy
                    ? t("cockpit.busy")
                    : m.unload_in_secs === null || m.unload_in_secs === undefined
                      ? t("cockpit.stays")
                      : t("cockpit.unloadIn", { time: minutes(Math.max(1, m.unload_in_secs), t) })}
                </span>
              </li>
            ))}
          </ul>
        )}
        <div className="row">
          {st.loaded.length > 0 && (
            <button
              type="button"
              className="secondary"
              onClick={async () => {
                try {
                  await client.op("unload_models", {});
                  await refresh("resource_status", "list_models");
                } catch (e) {
                  setError(e);
                }
              }}
            >
              {t("cockpit.unloadAll")}
            </button>
          )}
        </div>
        {st.recent.length > 0 && (
          <ul className="recent-actions small muted">
            {st.recent.slice(0, 3).map((a, i) => (
              <li key={i}>
                {new Date(a.at).toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" })} · {t(`cockpit.reason.${a.reason}` as Key, { model: a.model })}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

/** Always visible in the status bar: what Ancilo takes right now. */
export function ResourceMeter() {
  const { t } = useI18n();
  const status = useResources();
  const st = status.data;
  if (!st) return null;
  return (
    <button
      type="button"
      className="meter-button"
      title={t("cockpit.open")}
      onClick={() => {
        navigate({ view: "system" });
        window.setTimeout(() => document.getElementById("cockpit")?.scrollIntoView({ behavior: "smooth" }), 50);
      }}
    >
      {/* Only whether the AI runs – how the computer is doing, the status bar's verdict says. */}
      <span className={st.used_bytes > 0 ? "dot dot-running" : "dot"} aria-hidden="true" />
      <span>{st.used_bytes > 0 ? t("cockpit.meter", { size: formatRam(st.used_bytes) }) : t("cockpit.meterIdle")}</span>
    </button>
  );
}
