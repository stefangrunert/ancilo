import { useState } from "react";
import type { components } from "../api/schema";
import { useI18n, type Key } from "../i18n";
import { useClient, useOp, useRefresh } from "../state/store";
import { Dialog, ErrorNote } from "./ui";

type Entry = components["schemas"]["Entry"];

/** Requests that belong together – one search, one download – shown as one line. */
export interface Group {
  first: Entry;
  entries: Entry[];
}

/**
 * Consecutive entries of the same thing (purpose, subject, destination, who
 * started it) within two minutes are one line: a Wikipedia search is several
 * requests, a download with retries too.
 */
export function group(entries: Entry[]): Group[] {
  const out: Group[] = [];
  for (const e of entries) {
    const g = out[out.length - 1];
    const last = g?.entries[g.entries.length - 1];
    if (
      g &&
      last &&
      last.purpose === e.purpose &&
      last.subject === e.subject &&
      last.host === e.host &&
      last.by === e.by &&
      Math.abs(Date.parse(last.at) - Date.parse(e.at)) < 120_000
    ) {
      g.entries.push(e);
    } else {
      out.push({ first: e, entries: [e] });
    }
  }
  return out;
}

function bytes(n: number | null | undefined): string {
  if (n == null) return "–";
  if (n >= 2 ** 30) return `${(n / 2 ** 30).toFixed(1)} GB`;
  if (n >= 2 ** 20) return `${(n / 2 ** 20).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

/** Sent text as it was – JSON readable. */
function shown(sent: string): string {
  try {
    return JSON.stringify(JSON.parse(sent), null, 2);
  } catch {
    return sent;
  }
}

const failed = (e: Entry) => Boolean(e.error) || (e.status != null && e.status >= 400);

/**
 * System › What left this Mac: everything Ancilo sent over the internet –
 * simple by default (what, where, when), every request in detail on a click.
 * Kept 30 days on this Mac only.
 */
export function OutboundLog() {
  const { t, lang } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [all, setAll] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  const [clearing, setClearing] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const summary = useOp("outbound_summary");
  const log = useOp("outbound_log", { limit: all ? 200 : 50 });
  const groups = group(log.data?.entries ?? []);
  const visible = all ? groups : groups.slice(0, 5);
  const count = (m: Record<string, number> | undefined) => Object.values(m ?? {}).reduce((a, b) => a + b, 0);
  const today = count(summary.data?.today);
  const week = count(summary.data?.week);
  const day = (at: string) => new Date(at).toLocaleDateString(lang, { weekday: "short", day: "numeric", month: "short" });
  const time = (at: string) => new Date(at).toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" });
  return (
    <section className="panel wide" aria-labelledby="panel-outbound" data-testid="outbound">
      <header>
        <h2 id="panel-outbound">{t("outbound.title")}</h2>
      </header>
      <p className="hint">{t("outbound.hint")}</p>
      {summary.data && (
        <p data-testid="outbound-summary">
          {week === 0 ? t("outbound.nothing") : t("outbound.counts", { today, week })}
        </p>
      )}
      {visible.length > 0 && (
        <ul className="outbound-list">
          {visible.map((g, i) => {
            const e = g.first;
            const before = visible[i - 1];
            const showDay = !before || day(before.first.at) !== day(e.at);
            const isOpen = open === e.id;
            return (
              <li key={e.id}>
                {showDay && <div className="outbound-day muted small">{day(e.at)}</div>}
                <button
                  type="button"
                  className="outbound-row"
                  aria-expanded={isOpen}
                  onClick={() => setOpen(isOpen ? null : e.id)}
                >
                  <span className="muted small outbound-time">{time(e.at)}</span>
                  <span className="outbound-what">
                    {t(`outbound.purpose.${e.purpose}` as Key, { subject: e.subject })}
                    <span className="muted"> → {e.host}</span>
                    {g.entries.length > 1 && <span className="muted small"> · {t("outbound.requests", { n: g.entries.length })}</span>}
                    {e.by === "ancilo" && <span className="badge">{t("outbound.byAncilo")}</span>}
                    {g.entries.every(failed) && <span className="badge warn">{t("outbound.failed")}</span>}
                  </span>
                </button>
                {isOpen && (
                  <div className="outbound-details" data-testid="outbound-details">
                    {g.entries.map((r) => (
                      <div key={r.id} className="outbound-request">
                        <code className="outbound-url">
                          {r.method} {r.url}
                        </code>
                        {r.redirected_to && (
                          <div className="small muted">
                            {t("outbound.redirected")} <code>{r.redirected_to}</code>
                          </div>
                        )}
                        <div className="small muted">
                          {r.error
                            ? t("outbound.error", { why: r.error })
                            : t("outbound.answer", { status: r.status ?? "–", size: bytes(r.received_bytes) })}
                          {" · "}
                          {t("outbound.sentBytes", { size: bytes(r.sent_bytes) })}
                          {" · "}
                          {r.by === "ancilo" ? t("outbound.byAncilo") : t("outbound.byYou")}
                        </div>
                        {r.sent ? (
                          <details>
                            <summary className="small">{t("outbound.sent")}</summary>
                            <pre className="outbound-sent">{shown(r.sent)}</pre>
                          </details>
                        ) : (
                          <div className="small muted">{t("outbound.onlyAddress")}</div>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      <div className="row">
        {(groups.length > visible.length || (!all && log.data?.more)) && (
          <button type="button" className="secondary" onClick={() => setAll(true)}>
            {t("outbound.all")}
          </button>
        )}
        {groups.length > 0 && (
          <button type="button" className="secondary" onClick={() => setClearing(true)}>
            {t("outbound.clear")}
          </button>
        )}
      </div>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <Dialog open={clearing} title={t("confirm.title")} onClose={() => setClearing(false)}>
        <p>{t("outbound.clearConfirm")}</p>
        <div className="row end">
          <button type="button" className="secondary" onClick={() => setClearing(false)}>
            {t("confirm.no")}
          </button>
          <button
            type="button"
            onClick={async () => {
              setClearing(false);
              try {
                await client.op("clear_outbound_log", {}, true);
                await refresh("outbound_log", "outbound_summary");
              } catch (e) {
                setError(e);
              }
            }}
          >
            {t("outbound.clear")}
          </button>
        </div>
      </Dialog>
    </section>
  );
}
