import { useState } from "react";
import type { AttachmentView } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { useClient } from "../state/store";
import { Icon } from "./Icon";

/** A document on its way into a chat: being read, read, or not readable. */
export type Pending = { key: string; name: string; view?: AttachmentView; error?: string };

/** What a document is, in a few words: "PDF · 12 pages", "Excel · 3 sheets". */
function facts(v: AttachmentView, t: (k: Key, p?: Record<string, string | number>) => string): string {
  const kind = t(`doc.kind.${v.kind}` as Key);
  if (v.pages) return `${kind} · ${t("doc.pages", { n: v.pages })}`;
  if (v.sheets) return `${kind} · ${t("doc.sheets", { n: v.sheets })}`;
  return kind;
}

/** One document as a small card with its name, what it is and what to know. */
export function DocChip({ name, view, error, reading = false, onRemove }: { name: string; view?: AttachmentView; error?: string; reading?: boolean; onRemove?: () => void }) {
  const { t } = useI18n();
  const warnings = (view?.warnings ?? []).map((w) => t(`doc.warning.${w}` as Key));
  return (
    <span className={error ? "doc-chip error" : "doc-chip"} data-testid="doc-chip" title={error ?? warnings.join(" · ")}>
      <Icon name="doc" size={15} />
      <span className="doc-name">{name}</span>
      <span className="doc-facts">{reading ? t("doc.reading") : error ? t("doc.unreadable") : view ? facts(view, t) : ""}</span>
      {warnings.length > 0 && !error && <span className="doc-warn">{warnings[0]}</span>}
      {onRemove && (
        <button type="button" className="icon" aria-label={t("doc.remove", { name })} title={t("doc.remove", { name })} onClick={onRemove}>
          <Icon name="close" size={12} />
        </button>
      )}
    </span>
  );
}

/** Documents chosen for the next message: read at once, sent with it. */
export function usePendingDocs() {
  const client = useClient();
  const [docs, setDocs] = useState<Pending[]>([]);
  const add = (files: File[]) => {
    for (const f of files) {
      const key = `${f.name}-${Math.random().toString(36).slice(2)}`;
      setDocs((d) => [...d, { key, name: f.name }]);
      client
        .attach(f.name, f)
        .then((view) => setDocs((d) => d.map((x) => (x.key === key ? { ...x, view } : x))))
        .catch((e: unknown) => setDocs((d) => d.map((x) => (x.key === key ? { ...x, error: e instanceof Error ? e.message : String(e) } : x))));
    }
  };
  const remove = (key: string) => {
    const doc = docs.find((d) => d.key === key);
    if (doc?.view) void client.op("remove_attachment", { id: doc.view.id }).catch(() => {});
    setDocs((d) => d.filter((x) => x.key !== key));
  };
  return {
    docs,
    add,
    remove,
    reading: docs.some((d) => !d.view && !d.error),
    ids: docs.flatMap((d) => (d.view ? [d.view.id] : [])),
    clear: () => setDocs([]),
  };
}

/** The documents above the input. */
export function PendingDocs({ docs, onRemove }: { docs: Pending[]; onRemove: (key: string) => void }) {
  if (docs.length === 0) return null;
  return (
    <div className="doc-row" data-testid="pending-docs">
      {docs.map((d) => (
        <DocChip key={d.key} name={d.name} view={d.view} error={d.error} reading={!d.view && !d.error} onRemove={() => onRemove(d.key)} />
      ))}
    </div>
  );
}

/** Says that a conversation with documents stays on this computer. */
export function LocalNote() {
  const { t } = useI18n();
  return (
    <p className="local-note" data-testid="local-note">
      <Icon name="lock" size={13} />
      {t("doc.local")}
    </p>
  );
}
