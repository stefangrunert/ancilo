import { useEffect, useState, type FormEvent } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { navigate } from "../state/route";
import { usePreferences, usePro, useSetPreferences } from "../state/prefs";
import { useClient, useLive, useOp, useRefresh } from "../state/store";
import { AddModel } from "./AddModel";
import { Connect } from "./Settings";
import { ErrorNote, formatBytes, formatRam } from "./ui";

type Recommendations = OpOutput<"recommend_models">;
type Suggestion = NonNullable<Recommendations["best"]>;
export type Purpose = Recommendations["purposes"][number];
type SearchHit = OpOutput<"search_models">[number];

const PURPOSES: Purpose[] = ["chat", "code", "documents"];
/** "What do you want to use Ancilo for?" – one or more. */
export function PurposeChoice({ value, onChange }: { value: Purpose[]; onChange: (p: Purpose[]) => void }) {
  const { t } = useI18n();
  const icons: Record<Purpose, string> = { chat: "💬", code: "💻", documents: "📄" };
  return (
    <fieldset className="purposes">
      <legend className="sr-only">{t("choose.purposeQuestion")}</legend>
      {PURPOSES.map((p) => {
        const on = value.includes(p);
        return (
          <label key={p} className={on ? "purpose on" : "purpose"}>
            <input
              type="checkbox"
              checked={on}
              onChange={(e) => {
                const next = e.target.checked ? [...value, p] : value.filter((x) => x !== p);
                onChange(next.length ? PURPOSES.filter((x) => next.includes(x)) : value);
              }}
            />
            <span className="purpose-icon" aria-hidden="true">
              {icons[p]}
            </span>
            <span className="purpose-text">
              <strong>{t(`choose.purpose.${p}` as Key)}</strong>
              <span className="muted small">{t(`choose.purposeHint.${p}` as Key)}</span>
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

function Stars({ quality }: { quality: number }) {
  const { t } = useI18n();
  const n = Math.max(1, Math.min(5, Math.round(quality / 2)));
  return (
    <span className="stars" role="img" aria-label={t("choose.quality", { n })} title={t("choose.qualityHint")}>
      {"★".repeat(n)}
      <span className="dim">{"★".repeat(5 - n)}</span>
    </span>
  );
}

function Facts({ s }: { s: Suggestion }) {
  const { t } = useI18n();
  return (
    <ul className="facts">
      <li>
        <span className="muted">{t("choose.qualityLabel")}</span> <Stars quality={s.quality} />
      </li>
      <li>
        <span className="muted">{t("choose.speedLabel")}</span> {t(`choose.speed.${s.speed}` as Key)}
        {s.measured && <span className="muted small"> · {t("choose.measured")}</span>}
      </li>
      <li>
        <span className="muted">{t("choose.sizeLabel")}</span>{" "}
        {s.installed ? t("choose.installed") : t("choose.download", { size: formatBytes(s.download_bytes), min: s.download_minutes })}
      </li>
      <li>
        <span className="muted">{t("choose.memoryLabel")}</span> {t("choose.memory", { size: formatRam(s.ram_bytes) })}
      </li>
    </ul>
  );
}

/** The model being downloaded and started – in plain words. */
function Progress({ id, onDone }: { id: string; onDone: () => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const live = useLive();
  const models = useOp("list_models");
  const m = (models.data ?? []).find((x) => x.id === id);
  const status = m?.status ?? "downloading";
  const done = status === "running" || (status === "ready" && m?.embedding);
  useEffect(() => {
    if (done) onDone();
  }, [done, onDone]);
  if (!m) return <p className="muted">…</p>;
  if (status === "download_failed" || status === "failed" || status === "crashed") {
    return (
      <ErrorNote
        error={new Error(t("choose.failed", { name: m.name, reason: m.failure ?? "" }))}
        action={
          <button type="button" onClick={() => void client.op("retry_download", { model: id }).then(() => refresh("list_models"))}>
            {t("model.retry")}
          </button>
        }
      />
    );
  }
  if (status === "downloading") {
    const pct = live.downloads[id] !== undefined ? live.downloads[id] * 100 : (m.download?.percent ?? 0);
    const d = m.download;
    const rate = d?.bytes_per_sec ?? 0;
    const left = d && d.total && rate > 0 ? Math.max(1, Math.ceil((d.total - d.bytes) / rate / 60)) : null;
    return (
      <div className="progress-box" role="status">
        <p>
          <strong>{t("choose.downloading", { name: m.name })}</strong>
        </p>
        <progress max={100} value={pct} aria-label={t("model.downloading", { pct: Math.round(pct) })} />
        <p className="muted small">
          {Math.round(pct)} %{left !== null ? ` · ${t("choose.minutesLeft", { min: left })}` : ""} · {t("choose.canClose")}
        </p>
      </div>
    );
  }
  return (
    <div className="progress-box" role="status">
      <p>
        <strong>{status === "starting" ? t("choose.starting", { name: m.name }) : t("choose.preparing", { name: m.name })}</strong>
      </p>
      <progress aria-label={t("choose.starting", { name: m.name })} />
    </div>
  );
}

function SearchMore({ onInstall }: { onInstall: (address: string) => Promise<void> }) {
  const { t } = useI18n();
  const client = useClient();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [checked, setChecked] = useState<Record<string, OpOutput<"plan_model"> | Error>>({});
  const [error, setError] = useState<unknown>(null);
  const search = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      setHits(await client.op("search_models", { query, limit: 10 }));
    } catch (err) {
      setError(err);
    }
  };
  const check = async (h: SearchHit) => {
    try {
      const p = await client.op("plan_model", { address: h.address });
      setChecked((c) => ({ ...c, [h.address]: p }));
    } catch (err) {
      setChecked((c) => ({ ...c, [h.address]: err as Error }));
    }
  };
  return (
    <div className="stack">
      <p className="muted small">{t("choose.searchHint")}</p>
      <form className="row" onSubmit={(e) => void search(e)}>
        <input type="text" value={query} onChange={(e) => setQuery(e.target.value)} aria-label={t("choose.searchLabel")} placeholder={t("choose.searchPlaceholder")} />
        <button type="submit" className="secondary" disabled={!query.trim()}>
          {t("choose.search")}
        </button>
      </form>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      {hits?.length === 0 && <p className="muted">{t("choose.noHits")}</p>}
      {hits && hits.length > 0 && (
        <ul className="hits">
          {hits.map((h) => {
            const c = checked[h.address];
            return (
              <li key={h.repo}>
                <span className="hit-name">{h.repo}</span>
                <span className="muted small">{t("choose.downloads", { n: h.downloads.toLocaleString() })}</span>
                <span className="spacer" />
                {!c && (
                  <button type="button" className="secondary" onClick={() => void check(h)}>
                    {t("choose.check")}
                  </button>
                )}
                {c instanceof Error && <span className="outcome bad">{c.message}</span>}
                {c && !(c instanceof Error) && (
                  <>
                    <span className={c.plan.fit === "does_not_fit" ? "outcome bad" : "outcome"}>
                      {formatBytes(c.download_bytes)} · {t(`fit.${c.plan.fit}` as Key)}
                    </span>
                    {c.plan.fit !== "does_not_fit" && (
                      <button type="button" onClick={() => void onInstall(h.address)}>
                        {t("choose.take")}
                      </button>
                    )}
                  </>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/**
 * Finds the model for this machine: what it is for, the best fit (big), two or
 * three alternatives, everything else – and one click to download and start.
 */
export function ModelChoice({
  purposes,
  onReady,
  onStarted,
  next,
}: {
  purposes: Purpose[];
  onReady?: () => void;
  onStarted?: () => void;
  /** In the setup: the way on once the model is ready (instead of "ask now"). */
  next?: { label: string; onClick: () => void };
}) {
  const pro = usePro();
  const { t, lang } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [rec, setRec] = useState<Recommendations | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const key = purposes.join(",");
  useEffect(() => {
    let alive = true;
    setRec(null);
    setError(null);
    // Opening the model choice is the moment to look for a newer list.
    client
      .op("recommend_models", { purposes, refresh: true })
      .then((r) => alive && setRec(r))
      .catch((e) => alive && setError(e));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, key]);

  const install = async (address: string) => {
    setBusy(true);
    setError(null);
    try {
      if (rec?.embedding && purposes.includes("documents")) {
        await client.op("add_model", { address: rec.embedding.address, start: false }, true);
      }
      const m = await client.op("add_model", { address, start: true }, true);
      await refresh("list_models", "hardware_info");
      onStarted?.();
      setInstalling(m.id);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const use = async (s: Suggestion) => {
    if (s.installed) {
      setBusy(true);
      try {
        await client.op("start_model", { model: s.installed });
        await client.op("assign_role", { role: "default", model: s.installed });
        await refresh("list_models");
        onStarted?.();
        setInstalling(s.installed);
      } catch (e) {
        setError(e);
      } finally {
        setBusy(false);
      }
    } else await install(s.address);
  };

  if (ready && next) {
    return (
      <div className="card done-card" role="status" data-testid="model-ready">
        <h2>{t("choose.readyStep")}</h2>
        <div className="row">
          <button type="button" onClick={next.onClick}>
            {next.label}
          </button>
        </div>
      </div>
    );
  }
  if (ready) {
    return (
      <div className="card done-card" role="status" data-testid="model-ready">
        <h2>{t("choose.ready")}</h2>
        <p>{t("choose.readyHint")}</p>
        <div className="row">
          <button
            type="button"
            onClick={() => {
              onReady?.();
              navigate({ view: "chat", id: null });
            }}
          >
            {t("choose.askNow")}
          </button>
        </div>
        <p className="muted small">{t("choose.connectHint")}</p>
        <Connect />
      </div>
    );
  }
  if (installing) return <Progress id={installing} onDone={() => setReady(true)} />;
  if (error && !rec) return <ErrorNote error={error} />;
  if (!rec) return <p className="muted" role="status">{t("choose.looking")}</p>;

  const m = rec.memory;
  const card = (s: Suggestion, big: boolean) => (
    <div key={s.id} className={big ? "card model-card best" : "card model-card"} data-testid={`suggestion-${s.id}`}>
      {big && <span className="badge-best">{t("choose.recommended")}</span>}
      <div className="row">
        <h3 className="model-name">{s.name}</h3>
        <span className="muted small">{s.maker}</span>
        {s.tested && <span className="badge-tested">{t("choose.tested")}</span>}
      </div>
      <p>{lang === "de" ? s.summary.de : s.summary.en}</p>
      <Facts s={s} />
      {s.room === "close_programs" && <p className="note small">{t("choose.closePrograms")}</p>}
      <div className="row">
        <button type="button" className={big ? "" : "secondary"} disabled={busy} onClick={() => void use(s)}>
          {s.installed ? t("choose.useInstalled") : big ? t("choose.downloadStart") : t("choose.take")}
        </button>
      </div>
    </div>
  );
  return (
    <div className="model-choice stack">
      <p className="machine muted">
        {t("choose.machine", { chip: rec.chip, total: formatRam(m.total_bytes), room: formatRam(m.room_now_bytes) })}
      </p>
      {rec.best ? (
        <>
          {card(rec.best, true)}
          {rec.alternatives.length > 0 && (
            <>
              <h3>{t("choose.alternatives")}</h3>
              <div className="model-grid">{rec.alternatives.map((s) => card(s, false))}</div>
            </>
          )}
        </>
      ) : (
        <p className="note">{t("choose.nothingFits", { total: formatRam(m.total_bytes) })}</p>
      )}
      {purposes.includes("documents") && rec.embedding && <p className="muted small">{t("choose.withEmbedding", { size: formatBytes(rec.embedding.download_bytes) })}</p>}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      {rec.more.length > 0 && (
        <details className="section">
          <summary>{t("choose.more", { n: rec.more.length })}</summary>
          <div className="section-body model-grid">{rec.more.map((s) => card(s, false))}</div>
        </details>
      )}
      {rec.too_big > 0 && <p className="muted small">{t("choose.tooBig", { n: rec.too_big })}</p>}
      {pro && (
        <>
          <details className="section">
            <summary>{t("choose.searchTitle")}</summary>
            <div className="section-body">
              <SearchMore onInstall={install} />
            </div>
          </details>
          <details className="section" data-testid="advanced-add">
            <summary>{t("choose.advanced")}</summary>
            <div className="section-body">
              <AddModel />
            </div>
          </details>
        </>
      )}
      <p className="muted small">{t("choose.listFrom", { date: rec.catalog.updated })}</p>
    </div>
  );
}

/** Adding another model later (from the system page). */
export function ModelsPage() {
  const { t } = useI18n();
  const prefs = usePreferences();
  const set = useSetPreferences();
  const purposes = (prefs.data?.purposes?.length ? prefs.data.purposes : ["chat"]) as Purpose[];
  return (
    <div className="page">
      <div className="welcome stack">
        <h1>{t("choose.title")}</h1>
        <PurposeChoice value={purposes} onChange={(p) => void set({ purposes: p })} />
        <ModelChoice purposes={purposes} />
      </div>
    </div>
  );
}
