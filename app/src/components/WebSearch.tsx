import { useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { navigate } from "../state/route";
import { useClient, useOp, useRefresh } from "../state/store";
import { Icon } from "./Icon";
import { ErrorNote } from "./ui";

type View = OpOutput<"get_web_search">;
type Provider = View["provider"];

const PROVIDERS: Provider[] = ["off", "wikipedia", "serper"];
const SERPER = "https://serper.dev/";

/** The web search switch beside the input (chats and coding): on – Ancilo
 * searches without asking when it needs to; off – it asks before every search.
 * Only there once web search is set up. */
export function WebSwitch({ onError }: { onError?: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const view = useOp("get_web_search").data;
  if (!view || view.provider === "off") return null;
  const on = view.mode === "auto";
  const flip = async () => {
    try {
      await client.op("set_web_search", { mode: on ? "ask" : "auto" });
      await refresh("get_web_search");
    } catch (e) {
      onError?.(e);
    }
  };
  return (
    <button type="button" role="switch" aria-checked={on} className={on ? "web-toggle on" : "web-toggle"} title={t(on ? "web.switch.onHint" : "web.switch.offHint")} onClick={() => void flip()}>
      <Icon name="globe" size={14} />
      <span>{t("web.switch")}</span>
    </button>
  );
}

/** The Serper key: how to get one, then check and save it. */
function SerperSetup({ view, onSaved }: { view: View; onSaved: () => Promise<void> }) {
  const { t } = useI18n();
  const client = useClient();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [ok, setOk] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [replacing, setReplacing] = useState(false);
  const save = async () => {
    setBusy(true);
    setError(null);
    setOk(null);
    try {
      // A real search first: a wrong key is caught before it is saved.
      const tested = await client.op("test_web_search", { provider: "serper", serper_key: key.trim() });
      await client.op("set_web_search", { serper_key: key.trim(), provider: "serper" });
      setKey("");
      setReplacing(false);
      setOk(t("web.serper.works", { s: (tested.took_ms / 1000).toFixed(1) }));
      await onSaved();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  if (view.serper_key && !replacing) {
    return (
      <div className="card stack" data-testid="serper-key">
        <p>
          {t("web.serper.saved", { key: view.serper_key })}
        </p>
        {ok && <p className="outcome good">{ok}</p>}
        <div className="row">
          <button type="button" className="secondary" onClick={() => setReplacing(true)}>
            {t("web.serper.replace")}
          </button>
          <button
            type="button"
            className="ghost"
            onClick={async () => {
              setError(null);
              try {
                await client.op("set_web_search", { serper_key: "" });
                await onSaved();
              } catch (e) {
                setError(e);
              }
            }}
          >
            {t("web.serper.remove")}
          </button>
        </div>
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    );
  }
  return (
    <div className="card stack" data-testid="serper-setup">
      <h3>{t("web.serper.setupTitle")}</h3>
      <ol className="steps-list">
        <li>
          {t("web.serper.step1")}{" "}
          <a href={SERPER} target="_blank" rel="noopener noreferrer">
            serper.dev
          </a>
        </li>
        <li>{t("web.serper.step2")}</li>
        <li>{t("web.serper.step3")}</li>
      </ol>
      <p className="muted small">{t("web.serper.costs")}</p>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) void save();
        }}
      >
        <label className="grow">
          <span className="sr-only">{t("web.serper.key")}</span>
          <input type="password" autoComplete="off" spellCheck={false} placeholder={t("web.serper.key")} aria-label={t("web.serper.key")} value={key} onChange={(e) => setKey(e.target.value)} />
        </label>
        <button type="submit" disabled={!key.trim() || busy}>
          {busy ? t("web.serper.checking") : t("web.serper.save")}
        </button>
      </form>
      <p className="muted small">{t("web.serper.keychain")}</p>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </div>
  );
}

/**
 * Web search: off until the user picks a provider – Wikipedia (no account)
 * or Google through Serper (the user's key) – and when chats may search.
 * Says plainly what goes out.
 */
export function WebSearchPage() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const web = useOp("get_web_search");
  const [chosen, setChosen] = useState<Provider | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [tested, setTested] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const view = web.data;
  if (!view) return web.error ? <ErrorNote error={web.error} /> : null;
  // Serper chosen but without a key yet: its setup shows, the setting stays.
  const shown = chosen ?? view.provider;
  const set = async (input: Record<string, unknown>) => {
    setError(null);
    setTested(null);
    try {
      await client.op("set_web_search", input);
      await refresh("get_web_search");
    } catch (e) {
      setError(e);
    }
  };
  const pick = async (p: Provider) => {
    setChosen(p);
    if (p !== "serper" || view.serper_key) {
      await set({ provider: p });
      setChosen(null);
    }
  };
  return (
    <div className="page">
      <div className="setup stack" data-testid="web-search">
        <div className="welcome-head">
          <h1>{t("web.title")}</h1>
          <p>{t("web.intro")}</p>
        </div>
        <div className="card stack">
          <h2>{t("web.what")}</h2>
          <ul className="plain-list">
            <li>{t("web.what.query")}</li>
            <li>{t("web.what.pages")}</li>
            <li>{t("web.what.local")}</li>
          </ul>
        </div>
        <h2>{t("web.provider")}</h2>
        <div className="levels" role="radiogroup" aria-label={t("web.provider")}>
          {PROVIDERS.map((p) => (
            <label key={p} className={shown === p ? "level on" : "level"}>
              <input type="radio" name="provider" checked={shown === p} onChange={() => void pick(p)} />
              <strong>
                {t(`web.provider.${p}` as Key)}
                {p === "wikipedia" && <span className="muted small"> · {t("web.free")}</span>}
              </strong>
              <span className="muted small">{t(`web.provider.${p}.hint` as Key)}</span>
            </label>
          ))}
        </div>
        {shown === "serper" && (
          <SerperSetup
            view={view}
            onSaved={async () => {
              setChosen(null);
              await refresh("get_web_search");
            }}
          />
        )}
        {view.provider !== "off" && (
          <>
            <h2>{t("web.mode")}</h2>
            <div className="levels" role="radiogroup" aria-label={t("web.mode")}>
              {(["ask", "auto"] as const).map((m) => (
                <label key={m} className={view.mode === m ? "level on" : "level"}>
                  <input type="radio" name="mode" checked={view.mode === m} onChange={() => void set({ mode: m })} />
                  <strong>
                    {t(`web.mode.${m}` as Key)}
                    {m === "ask" && <span className="muted small"> · {t("setup.recommended")}</span>}
                  </strong>
                  <span className="muted small">{t(`web.mode.${m}.hint` as Key)}</span>
                </label>
              ))}
            </div>
            <div className="row">
              <button
                type="button"
                className="secondary"
                disabled={testing}
                onClick={async () => {
                  setTesting(true);
                  setError(null);
                  setTested(null);
                  try {
                    const r = await client.op("test_web_search", {});
                    setTested(t("web.tested", { n: r.sources, s: (r.took_ms / 1000).toFixed(1) }));
                  } catch (e) {
                    setError(e);
                  } finally {
                    setTesting(false);
                  }
                }}
              >
                {testing ? t("web.serper.checking") : t("web.test")}
              </button>
              <button type="button" onClick={() => navigate({ view: "chat", id: null })}>
                {t("choose.askNow")}
              </button>
            </div>
            {tested && <p className="outcome good">{tested}</p>}
          </>
        )}
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    </div>
  );
}
