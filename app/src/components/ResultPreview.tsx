import { useEffect, useMemo, useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { basenameOf, loaded, markStale, previewKindFor, type PreviewState } from "../state/preview";
import { useClient, useOp, useRefresh } from "../state/store";
import { Icon } from "./Icon";
import { Dialog, ErrorNote } from "./ui";

type Preview = OpOutput<"preview_result">;
type Checks = OpOutput<"check_results">;
type Check = Checks["files"][number];
type Finding = Preview["findings"][number];
type Layout = NonNullable<Preview["layout"]>;
type Limit = NonNullable<Layout["limits"]>[number];
type Block = NonNullable<Layout["blocks"]>[number];

/**
 * The checks of a task's results – for the version of its changes they were
 * made on (keep or save with `version`: nothing newer goes out). Checked
 * again whenever the task changes its results.
 */
export function useChecks(session: string, version: string | null | undefined, enabled: boolean) {
  const checks = useOp("check_results", { session }, enabled);
  const refresh = useRefresh();
  useEffect(() => {
    if (enabled && version && checks.data && checks.data.version !== version) void refresh("check_results");
  }, [enabled, version, checks.data, refresh]);
  return checks;
}

/** How a result's checks came out, in a word. */
export function CheckBadge({ check }: { check: Check | undefined }) {
  const { t } = useI18n();
  if (!check) return null;
  if (check.errors > 0) {
    return (
      <span className="check-badge error" data-testid="check-badge">
        <Icon name="close" size={11} /> {t("check.errors", { n: check.errors })}
      </span>
    );
  }
  if (check.warnings > 0) {
    return (
      <span className="check-badge warning" data-testid="check-badge">
        ! {t("check.warnings", { n: check.warnings })}
      </span>
    );
  }
  return (
    <span className="check-badge ok" data-testid="check-badge">
      ✓ {t("check.ok")}
    </span>
  );
}

function limitText(l: Limit, t: (k: Key, v?: Record<string, string | number>) => string): string {
  switch (l.kind) {
    case "rows_cut":
      return t("check.limit.rows_cut", { sheet: l.sheet, shown: l.shown, total: l.total });
    case "blocks_cut":
      return t("check.limit.blocks_cut", { shown: l.shown, total: l.total });
    case "formulas_saved":
      return t("check.limit.formulas_saved", { count: l.count });
    case "no_formatting":
      return t("check.limit.no_formatting");
    default:
      return t("check.limit.not_shown");
  }
}

const AREAS = ["readable", "complete", "numbers"] as const;

/** What the checks found, by question: does it open, is it complete, do the numbers add up. */
function Findings({ findings }: { findings: Finding[] }) {
  const { t, says } = useI18n();
  return (
    <div className="check-findings" data-testid="check-findings">
      {AREAS.map((area) => {
        const f = findings.filter((x) => x.area === area);
        if (f.length === 0) return null;
        return (
          <div key={area} className="check-area">
            <strong>{t(`check.area.${area}` as Key)}</strong>
            <ul>
              {f.map((x, i) => (
                <li key={i} className={`check-${x.level}`} data-testid={`finding-${x.level}`}>
                  <span className="check-mark" aria-hidden="true">
                    {x.level === "ok" ? "✓" : x.level === "warning" ? "!" : "✗"}
                  </span>
                  <span>
                    {says(x.message)}
                    {x.place && <code className="check-place">{x.place}</code>}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </div>
  );
}

const letter = (i: number): string => (i < 26 ? String.fromCharCode(65 + i) : letter(Math.floor(i / 26) - 1) + String.fromCharCode(65 + (i % 26)));

/** A sheet as a table: row numbers and column letters as in the program, found cells marked. */
function SheetTable({ sheet, marked }: { sheet: NonNullable<Layout["sheets"]>[number]; marked: Set<string> }) {
  const cols = Math.max(0, ...sheet.rows.map((r) => r.cells.length));
  return (
    <div className="preview-table-wrap">
      <table className="preview-table">
        <thead>
          <tr>
            <th />
            {Array.from({ length: cols }, (_, i) => (
              <th key={i}>{letter(i)}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sheet.rows.map((r) => (
            <tr key={r.number}>
              <th>{r.number}</th>
              {Array.from({ length: cols }, (_, i) => {
                const at = `${letter(i)}${r.number}`;
                const hit = marked.has(`${sheet.name}!${at}`) || marked.has(at);
                return (
                  <td key={i} className={hit ? "marked" : undefined} data-testid={hit ? "marked-cell" : undefined}>
                    {r.cells[i] ?? ""}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function DocBlocks({ blocks, marked }: { blocks: Block[]; marked: Set<string> }) {
  let table = 0;
  return (
    <div className="preview-doc">
      {blocks.map((b, i) => {
        if (b.kind === "heading") return b.level <= 1 ? <h3 key={i}>{b.text}</h3> : <h4 key={i}>{b.text}</h4>;
        if (b.kind === "paragraph") return <p key={i}>{b.text}</p>;
        table += 1;
        const name = `table ${table}`;
        return (
          <SheetTable
            key={i}
            marked={marked}
            sheet={{ name, total_rows: b.rows.length, rows: b.rows.map((cells, n) => ({ number: n + 1, cells })) }}
          />
        );
      })}
    </div>
  );
}

/**
 * A task's result looked at before keeping it: the file as it is inside,
 * what the checks found, and what the preview leaves out. It shows exactly
 * one version of the results; when the task changed them since, it says so
 * and offers the new one (it never swaps them under the reader's eyes).
 */
export function ResultPreview({ session, path, current, onClose }: { session: string; path: string | null; current: string | null | undefined; onClose: () => void }) {
  const { t } = useI18n();
  const client = useClient();
  const [state, setState] = useState<PreviewState<Preview>>({ status: "idle" });
  const [nonce, setNonce] = useState(0);
  const [sheet, setSheet] = useState(0);
  useEffect(() => {
    if (!path) {
      setState({ status: "idle" });
      return;
    }
    if (previewKindFor(path) === "file") {
      setState({ status: "unsupported", path });
      return;
    }
    let alive = true;
    setState({ status: "loading", path });
    client
      .op("preview_result", { session, path })
      .then((p) => alive && setState(loaded(path, p, p.version)))
      .catch((e: unknown) => alive && setState({ status: "failed", path, reason: e }));
    return () => {
      alive = false;
    };
  }, [client, session, path, nonce]);
  const shown = markStale(state, current);
  const p = shown.status === "ready" ? shown.data : null;
  const marked = useMemo(() => new Set((p?.findings ?? []).map((f) => f.place).filter((x): x is string => Boolean(x))), [p]);
  const sheets = p?.layout?.sheets ?? [];
  return (
    <Dialog open={Boolean(path)} title={path ? basenameOf(path) : ""} onClose={onClose}>
      <div className="result-preview" data-testid="result-preview">
        {shown.status === "loading" && <p className="muted">{t("check.checking")}</p>}
        {shown.status === "unsupported" && <p className="note">{t("check.limit.not_shown")}</p>}
        {shown.status === "failed" && <ErrorNote error={shown.reason} />}
        {shown.status === "ready" && p && (
          <>
            {shown.stale && (
              <div className="note warn row" role="alert" data-testid="preview-stale">
                <span>{t("check.stale")}</span>
                <button type="button" className="secondary" onClick={() => setNonce((n) => n + 1)}>
                  {t("check.reload")}
                </button>
              </div>
            )}
            <Findings findings={p.findings} />
            {(p.layout?.limits ?? []).length > 0 && (
              <ul className="preview-limits muted small" data-testid="preview-limits">
                {(p.layout?.limits ?? []).map((l, i) => (
                  <li key={i}>{limitText(l, t)}</li>
                ))}
              </ul>
            )}
            {sheets.length > 1 && (
              <div className="row tabs" role="tablist">
                {sheets.map((s, i) => (
                  <button key={s.name} type="button" role="tab" aria-selected={i === sheet} className={i === sheet ? "tab active" : "tab"} onClick={() => setSheet(i)}>
                    {s.name}
                  </button>
                ))}
              </div>
            )}
            {sheets[sheet] && <SheetTable sheet={sheets[sheet]} marked={marked} />}
            {(p.layout?.blocks ?? []).length > 0 && <DocBlocks blocks={p.layout?.blocks ?? []} marked={marked} />}
            <p className="muted small">
              {t("check.disclaimer")} · {t("check.version", { v: p.file.slice(0, 8) })}
            </p>
          </>
        )}
        <div className="row end">
          <button type="button" className="secondary" onClick={onClose}>
            <Icon name="close" size={13} /> {t("sources.close")}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

/** Errors the checks found, over all results. */
export function problems(checks: Checks | undefined): number {
  return (checks?.files ?? []).filter((f) => f.errors > 0).length;
}
