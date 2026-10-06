import { useEffect, useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { useOp, useRefresh } from "../state/store";
import { Icon } from "./Icon";
import { Dialog, ErrorNote } from "./ui";

type Message = OpOutput<"get_conversation">["messages"][number];
export type Evidence = NonNullable<Message["evidence"]>[number];
type T = (k: Key, v?: Record<string, string | number>) => string;

/** "D3" → 3 (what the reader sees). */
const num = (id: string) => id.replace(/^D/i, "");

/** Where in the document: "page 7", "sheet 2024" – nothing when not known. */
export function placeOf(e: Pick<Evidence, "at">, t: T): string {
  const at = e.at as { page?: number; sheet?: string } | null | undefined;
  if (at?.page !== undefined) return t("sources.page", { n: at.page });
  if (at?.sheet !== undefined) return t("sources.sheet", { name: at.sheet });
  return "";
}

/**
 * "[D3]" in an answer becomes a link the app opens as source 3 – only for
 * marks of passages the answer was given (the daemon took out the others).
 */
export function linkEvidence(text: string, evidence: Evidence[]): string {
  return text.replace(/\[(D\d+)\](?!\()/gi, (all, id: string) => {
    const e = evidence.find((x) => x.id.toUpperCase() === id.toUpperCase());
    return e ? `[${num(e.id)}](#source-${e.id})` : all;
  });
}

/**
 * Below an answer from documents: the passages it names, each opening the
 * text it drew on; what Ancilo took out (a source made up by the model).
 */
export function DocSources({ evidence, dropped, onOpen }: { evidence: Evidence[]; dropped: string[]; onOpen: (id: string) => void }) {
  const { t } = useI18n();
  const cited = evidence.filter((e) => e.cited);
  return (
    <div className="doc-sources" data-testid="doc-sources">
      {cited.length > 0 ? (
        <ol aria-label={t("sources.title")}>
          {cited.map((e) => (
            <li key={e.id} value={Number(num(e.id))}>
              <button type="button" className="link" data-testid={`source-${e.id}`} onClick={() => onOpen(e.id)}>
                <Icon name="doc" size={13} /> {e.document}
                {placeOf(e, t) && <span className="muted"> · {placeOf(e, t)}</span>}
              </button>
            </li>
          ))}
        </ol>
      ) : (
        <p className="reply-meta">{t("sources.none")}</p>
      )}
      {dropped.length > 0 && (
        <p className="reply-meta warn-text" data-testid="dropped-marks">
          {t("sources.dropped", { n: dropped.length })}
        </p>
      )}
    </div>
  );
}

/**
 * A source opened: the passage the answer had – as it stood then – with
 * the text around it, and what to know: the document changed since or is
 * gone, its text was recognized from a scan.
 */
export function SourceDialog({ conversation, mark, onClose }: { conversation: string; mark: string | null; onClose: () => void }) {
  const { t } = useI18n();
  const opened = useOp("open_evidence", mark ? { conversation, mark } : undefined, Boolean(mark));
  const refresh = useRefresh();
  // Opened again: how the document stands now, not how it stood last time.
  useEffect(() => {
    if (mark) void refresh("open_evidence");
  }, [mark, refresh]);
  const o = opened.data;
  const e = o?.evidence;
  const title = e ? `${t("sources.one", { n: num(e.id) })} · ${e.document}` : t("sources.title");
  return (
    <Dialog open={Boolean(mark)} title={title} onClose={onClose}>
      <div className="source-view" data-testid="source-view">
        <ErrorNote error={opened.error} />
        {e && (
          <>
            <p className="muted small" data-testid="source-place">
              {[placeOf(e, t), t("sources.revision", { rev: e.revision.slice(0, 8) })].filter(Boolean).join(" · ")}
            </p>
            {o.now === "changed" && (
              <p className="note warn" role="alert" data-testid="source-changed">
                {t("sources.changed")}
              </p>
            )}
            {o.now === "gone" && (
              <p className="note warn" role="alert" data-testid="source-gone">
                {t("sources.gone")}
              </p>
            )}
            {(e.warnings ?? []).map((w) => (
              <p key={w} className="note" data-testid="source-warning">
                {t(`doc.warning.${w}` as Key)}
              </p>
            ))}
            <blockquote className="source-text">
              {o.before && <span className="muted">{o.before}</span>}
              <mark data-testid="source-excerpt">{e.text}</mark>
              {o.after && <span className="muted">{o.after}</span>}
            </blockquote>
          </>
        )}
        <div className="row end">
          <button type="button" className="secondary" onClick={onClose}>
            {t("sources.close")}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

/** Which source is open in an answer's dialog. */
export function useSource() {
  const [mark, setMark] = useState<string | null>(null);
  return { mark, open: setMark, close: () => setMark(null) };
}
