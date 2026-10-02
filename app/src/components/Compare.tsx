import { useState, type FormEvent } from "react";
import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { useClient, useOp, useRefresh } from "../state/store";
import { ErrorNote, Section } from "./ui";

type Model = OpOutput<"list_models">[number];
type Report = OpOutput<"comparison_report">;

function ReportView({ id }: { id: string }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const report = useOp("comparison_report", { id });
  const [error, setError] = useState<unknown>(null);
  const r = report.data as Report | undefined;
  if (!r) return null;
  const rate = async (best: string) => {
    try {
      await client.op("rate_comparison", { id, best });
      await refresh("comparison_report", "list_comparisons");
    } catch (e) {
      setError(e);
    }
  };
  return (
    <div className="report" data-testid={`report-${id}`}>
      <p className="muted">
        {r.status} · {r.progress}
      </p>
      <table>
        <tbody>
          {r.ranking.map((a) => (
            <tr key={a.label}>
              <th scope="row">{a.label}</th>
              <td>{a.model ?? "?"}</td>
              <td>{Math.round(a.success.rate * 100)} %</td>
              <td>{a.duration_p50_ms != null ? `${(a.duration_p50_ms / 1000).toFixed(1)} s` : "–"}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p>{r.verdict}</p>
      {r.status === "done" && r.blind && !r.revealed && (
        <div className="row" role="group" aria-label={t("compare.rate")}>
          <span className="label">{t("compare.best")}</span>
          {r.ranking.map((a) => (
            <button key={a.label} type="button" className="secondary" onClick={() => void rate(a.label)}>
              {a.label}
            </button>
          ))}
          <button type="button" className="secondary" onClick={() => void rate("tie")}>
            {t("compare.tie")}
          </button>
        </div>
      )}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </div>
  );
}

export function CompareSection({ models }: { models: Model[] }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const list = useOp("list_comparisons", { limit: 10 });
  const ab = useOp("ab_status");
  const chat = models.filter((m) => !m.embedding && !m.cloud);
  const [task, setTask] = useState("");
  const [cwd, setCwd] = useState("");
  const [check, setCheck] = useState("");
  const [blind, setBlind] = useState(false);
  const [chosen, setChosen] = useState<string[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [open, setOpen] = useState<string | null>(null);
  async function start(e: FormEvent) {
    e.preventDefault();
    setError(null);
    try {
      const r = await client.op("compare_models", { task, cwd, models: chosen, check: check || null, blind });
      setOpen(r.id);
      await refresh("list_comparisons");
    } catch (err) {
      setError(err);
    }
  }
  return (
    <Section title={t("sections.compare")} testId="section-compare">
      <form onSubmit={start} className="compare-form">
        <label>
          {t("compare.task")}
          <input type="text" value={task} onChange={(e) => setTask(e.target.value)} />
        </label>
        <label>
          {t("compare.cwd")}
          <input type="text" value={cwd} onChange={(e) => setCwd(e.target.value)} spellCheck={false} />
        </label>
        <fieldset className="row">
          <legend className="label">{t("compare.models")}</legend>
          {chat.map((m) => (
            <label key={m.id}>
              <input
                type="checkbox"
                checked={chosen.includes(m.id)}
                onChange={(e) => setChosen((c) => (e.target.checked ? [...c, m.id] : c.filter((x) => x !== m.id)))}
              />
              {m.name}
            </label>
          ))}
        </fieldset>
        <label>
          {t("compare.check")}
          <input type="text" value={check} onChange={(e) => setCheck(e.target.value)} spellCheck={false} />
        </label>
        <label>
          <input type="checkbox" checked={blind} onChange={(e) => setBlind(e.target.checked)} />
          {t("compare.blind")}
        </label>
        <button type="submit" disabled={!task || !cwd || chosen.length < 2}>
          {t("compare.start")}
        </button>
      </form>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      {(list.data ?? []).length === 0 && <p className="muted">{t("compare.none")}</p>}
      <ul className="comparisons">
        {(list.data ?? []).map((c) => (
          <li key={c.id}>
            <button type="button" className="link" aria-expanded={open === c.id} onClick={() => setOpen(open === c.id ? null : c.id)}>
              {c.title} · {c.status}
            </button>
            {open === c.id && <ReportView id={c.id} />}
          </li>
        ))}
      </ul>
      {(ab.data ?? []).length > 0 && (
        <>
          <h3>{t("compare.ab")}</h3>
          <ul data-testid="ab-tests">
            {(ab.data ?? []).map((a) => (
              <li key={a.id}>
                {a.role}: {a.model_a} ↔ {a.model_b} ({Math.round(a.share * 100)} %) · {a.status}
              </li>
            ))}
          </ul>
        </>
      )}
    </Section>
  );
}
