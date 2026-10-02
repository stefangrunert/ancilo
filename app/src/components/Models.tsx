import { useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { useClient, useOp, useRefresh } from "../state/store";
import { Dialog, ErrorNote, Section, StatusDot, formatMem } from "./ui";

type Model = OpOutput<"list_models">[number];

const ROLES = ["default", "delegation", "coding", "assistant"] as const;

function ModelLine({ m }: { m: Model }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      await refresh("list_models", "hardware_info");
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  const pct = m.download?.percent ?? null;
  const tps = m.instance?.tokens_per_sec;
  return (
    <div className="model-line" data-testid={`model-${m.id}`}>
      <div className="row">
        <StatusDot status={m.status} />
        <strong>{m.name}</strong>
        <span className="muted" data-testid="model-status">
          {t(`model.status.${m.status}` as "model.status.ready")}
          {m.status === "running" && tps ? ` · ${t("model.tokens", { tps: Math.round(tps) })}` : ""}
        </span>
        <span className="spacer" />
        {m.status === "running" && !m.embedding && (
          <button type="button" className="secondary" disabled={busy} onClick={() => void run(() => client.op("stop_model", { model: m.id }))}>
            {t("model.stop")}
          </button>
        )}
        {m.status === "ready" && !m.embedding && (
          <button type="button" className="secondary" disabled={busy} onClick={() => void run(() => client.op("start_model", { model: m.id }))}>
            {t("model.start")}
          </button>
        )}
      </div>
      {m.status === "downloading" && (
        <progress max={100} value={pct ?? undefined} aria-label={t("model.downloading", { pct: Math.round(pct ?? 0) })}>
          {Math.round(pct ?? 0)} %
        </progress>
      )}
      {m.status === "download_failed" && (
        <ErrorNote
          error={new Error(`${t("model.status.download_failed")}: ${m.failure ?? ""}`)}
          action={
            <button type="button" onClick={() => void run(() => client.op("retry_download", { model: m.id }))}>
              {t("model.retry")}
            </button>
          }
        />
      )}
      {(m.status === "crashed" || m.status === "failed") && m.failure && <ErrorNote error={new Error(m.failure)} />}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </div>
  );
}

/** The model(s) in use – on the main screen. */
export function PrimaryModels({ models }: { models: Model[] }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const chat = models.filter((m) => !m.embedding);
  // The default model, and every model that is not just resting.
  const shown = chat.filter((m) => m.roles.includes("default") || m.status !== "ready");
  if (models.length === 0) {
    return (
      <div className="empty">
        <p>{t("model.none")}</p>
        <button
          type="button"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await client.op("setup", {}, true);
              await refresh("list_models");
            } catch (e) {
              setError(e);
            } finally {
              setBusy(false);
            }
          }}
        >
          {t("model.setup")}
        </button>
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    );
  }
  return (
    <div className="primary-models">
      {(shown.length ? shown : chat.slice(0, 1)).map((m) => (
        <ModelLine key={m.id} m={m} />
      ))}
    </div>
  );
}

/** All models, roles, memory and recommendations – only with more than one chat model. */
export function ModelsSection({ models }: { models: Model[] }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const hw = useOp("hardware_info");
  const recs = useOp("recommendations", { all: false });
  const [error, setError] = useState<unknown>(null);
  const [removing, setRemoving] = useState<Model | null>(null);
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      await refresh("list_models", "recommendations", "list_routes", "hardware_info");
    } catch (e) {
      setError(e);
    }
  };
  return (
    <Section title={t("sections.models", { n: models.length })} testId="section-models">
      {hw.data && (
        <p className="muted">
          {t("models.memory", { used: formatMem(hw.data.used_bytes), total: formatMem(hw.data.model_budget_bytes) })}
        </p>
      )}
      <table className="models">
        <thead>
          <tr>
            <th scope="col">{t("model.label")}</th>
            <th scope="col">{t("models.role")}</th>
            <th scope="col">{t("models.assign")}</th>
            <th scope="col">
              <span className="sr-only">{t("model.remove")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {models.map((m) => (
            <tr key={m.id}>
              <td>
                <ModelLine m={m} />
              </td>
              <td data-testid={`roles-${m.id}`}>{m.roles.join(", ") || "–"}</td>
              <td>
                {!m.embedding && (
                  <select
                    aria-label={`${t("models.assign")} ${m.name}`}
                    value=""
                    onChange={(e) => {
                      const role = e.target.value;
                      if (role) void act(() => client.op("assign_role", { role, model: m.id }));
                    }}
                  >
                    <option value="">…</option>
                    {ROLES.filter((r) => !m.roles.includes(r)).map((r) => (
                      <option key={r} value={r}>
                        {r}
                      </option>
                    ))}
                  </select>
                )}
              </td>
              <td>
                <button type="button" className="link" onClick={() => setRemoving(m)}>
                  {t("model.remove")}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {recs.data && recs.data.length > 0 && (
        <div className="recs">
          <h3>{t("models.recommendations")}</h3>
          <ul>
            {recs.data.map((r) => (
              <li key={r.id}>
                <span>{r.rationale}</span>
                <button type="button" onClick={() => void act(() => client.op("apply_recommendation", { id: r.id }, true))}>
                  {t("models.apply")}
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
      <Dialog open={removing !== null} title={t("confirm.title")} onClose={() => setRemoving(null)}>
        <p>{t("confirm.remove", { model: removing?.name ?? "" })}</p>
        <div className="row end">
          <button type="button" className="secondary" onClick={() => setRemoving(null)}>
            {t("confirm.no")}
          </button>
          <button
            type="button"
            onClick={() => {
              const m = removing;
              setRemoving(null);
              if (m) void act(() => client.op("remove_model", { model: m.id }, true));
            }}
          >
            {t("confirm.yes")}
          </button>
        </div>
      </Dialog>
    </Section>
  );
}
