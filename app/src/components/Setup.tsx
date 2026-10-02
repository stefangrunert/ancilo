import { useState, type ReactNode } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { usePreferences, useSetPreferences, type Step } from "../state/prefs";
import { navigate } from "../state/route";
import { useClient, useOp, useRefresh } from "../state/store";
import { useResources } from "./Cockpit";
import { ModelChoice, PurposeChoice, type Purpose } from "./ModelChoice";
import { Connect } from "./Settings";
import { ErrorNote, StatusDot } from "./ui";

const STEPS: Step[] = ["purpose", "model", "resources", "connect", "projects"];
const OPTIONAL: Step[] = ["connect", "projects"];
const LEVELS = ["eco", "balanced", "performance", "max"] as const;

type Model = OpOutput<"list_models">[number];

/** One step: what it is, why it matters, and how to do it. */
function StepFrame({ step, children, actions }: { step: Step; children: ReactNode; actions: ReactNode }) {
  const { t } = useI18n();
  const n = STEPS.indexOf(step) + 1;
  return (
    <section className="setup-step stack" aria-labelledby={`step-${step}`} data-testid={`step-${step}`}>
      <div className="progress-steps" aria-label={t("setup.progress", { n, of: STEPS.length })}>
        {STEPS.map((s, i) => (
          <span key={s} className={i < n - 1 ? "pstep done" : i === n - 1 ? "pstep current" : "pstep"} />
        ))}
        <span className="muted small">{t("setup.progress", { n, of: STEPS.length })}</span>
      </div>
      <h2 id={`step-${step}`}>{t(`setup.${step}.title` as Key)}</h2>
      <p className="why">{t(`setup.${step}.why` as Key)}</p>
      {children}
      <div className="row step-actions">{actions}</div>
    </section>
  );
}

function PurposeStep({ onDone }: { onDone: () => void }) {
  const { t } = useI18n();
  const prefs = usePreferences();
  const set = useSetPreferences();
  const [value, setValue] = useState<Purpose[] | null>(null);
  const purposes = value ?? ((prefs.data?.purposes?.length ? prefs.data.purposes : ["chat"]) as Purpose[]);
  return (
    <StepFrame
      step="purpose"
      actions={
        <button
          type="button"
          onClick={async () => {
            await set({ purposes, step: "purpose", state: "done" });
            onDone();
          }}
        >
          {t("setup.next")}
        </button>
      }
    >
      <PurposeChoice value={purposes} onChange={setValue} />
    </StepFrame>
  );
}

function ModelStep({ models, onDone, onBack }: { models: Model[]; onDone: () => void; onBack: () => void }) {
  const { t } = useI18n();
  const prefs = usePreferences();
  const set = useSetPreferences();
  const chat = models.filter((m) => !m.embedding);
  const [choosing, setChoosing] = useState(chat.length === 0);
  const purposes = (prefs.data?.purposes?.length ? prefs.data.purposes : ["chat"]) as Purpose[];
  const done = async () => {
    await set({ step: "model", state: "done" });
    onDone();
  };
  return (
    <StepFrame
      step="model"
      actions={
        <>
          <button type="button" className="secondary" onClick={onBack}>
            {t("setup.back")}
          </button>
          {chat.length > 0 && !choosing && (
            <button type="button" onClick={() => void done()}>
              {t("setup.next")}
            </button>
          )}
        </>
      }
    >
      {chat.length > 0 && !choosing ? (
        <div className="card stack">
          <p>{t("setup.model.have")}</p>
          {chat.map((m) => (
            <div key={m.id} className="row">
              <StatusDot status={m.status} />
              <strong>{m.name}</strong>
              <span className="muted small">{t(`model.status.${m.status}` as Key)}</span>
            </div>
          ))}
          <div className="row">
            <button type="button" className="secondary" onClick={() => setChoosing(true)}>
              {t("setup.model.other")}
            </button>
          </div>
        </div>
      ) : (
        <ModelChoice purposes={purposes} next={{ label: t("setup.next"), onClick: () => void done() }} />
      )}
    </StepFrame>
  );
}

function ResourcesStep({ onDone, onBack }: { onDone: () => void; onBack: () => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const set = useSetPreferences();
  const status = useResources();
  const level = status.data?.settings.level ?? "balanced";
  return (
    <StepFrame
      step="resources"
      actions={
        <>
          <button type="button" className="secondary" onClick={onBack}>
            {t("setup.back")}
          </button>
          <button
            type="button"
            onClick={async () => {
              await set({ step: "resources", state: "done" });
              onDone();
            }}
          >
            {t("setup.next")}
          </button>
        </>
      }
    >
      <div className="levels" role="radiogroup" aria-label={t("cockpit.title")}>
        {LEVELS.map((l) => (
          <label key={l} className={level === l ? "level on" : "level"}>
            <input
              type="radio"
              name="level"
              checked={level === l}
              onChange={async () => {
                await client.op("set_resources", { level: l });
                await refresh("resource_status", "recommend_models");
              }}
            />
            <strong>
              {t(`cockpit.level.${l}` as Key)}
              {l === "balanced" && <span className="muted small"> · {t("setup.recommended")}</span>}
            </strong>
            <span className="muted small">{t(`cockpit.describe.${l}` as Key)}</span>
          </label>
        ))}
      </div>
      <p className="muted small">{t("setup.resources.later")}</p>
    </StepFrame>
  );
}

function OptionalActions({ step, onDone, onBack, done }: { step: Step; onDone: () => void; onBack: () => void; done: boolean }) {
  const { t } = useI18n();
  const set = useSetPreferences();
  return (
    <>
      <button type="button" className="secondary" onClick={onBack}>
        {t("setup.back")}
      </button>
      <button
        type="button"
        className="secondary"
        onClick={async () => {
          await set({ step, state: "skipped" });
          onDone();
        }}
      >
        {t("setup.skip")}
      </button>
      <button
        type="button"
        disabled={!done}
        onClick={async () => {
          await set({ step, state: "done" });
          onDone();
        }}
      >
        {t("setup.next")}
      </button>
    </>
  );
}

function ConnectStep({ onDone, onBack }: { onDone: () => void; onBack: () => void }) {
  const { t } = useI18n();
  const connections = useOp("connections");
  const connected = (connections.data ?? []).some((c) => c.connected);
  return (
    <StepFrame step="connect" actions={<OptionalActions step="connect" onDone={onDone} onBack={onBack} done={connected} />}>
      <p className="muted small">{t("setup.connect.none")}</p>
      <Connect />
    </StepFrame>
  );
}

function ProjectsStep({ onDone, onBack }: { onDone: () => void; onBack: () => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const set = useSetPreferences();
  const prefs = usePreferences();
  const projects = useOp("list_projects");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const documents = prefs.data?.documents ?? [];
  const something = (projects.data ?? []).length > 0 || documents.length > 0;
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <StepFrame step="projects" actions={<OptionalActions step="projects" onDone={onDone} onBack={onBack} done={something} />}>
      <div className="card stack">
        <strong>{t("setup.projects.buildTitle")}</strong>
        <span className="muted small">{t("setup.projects.buildWhy")}</span>
        <form
          className="row"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              await client.op("create_project", { name });
              setName("");
              await refresh("list_projects");
            });
          }}
        >
          <input type="text" aria-label={t("build.newLabel")} value={name} onChange={(e) => setName(e.target.value)} placeholder={t("build.namePlaceholder")} />
          <button type="submit" className="secondary" disabled={busy || !name.trim()}>
            {t("build.create")}
          </button>
        </form>
        {(projects.data ?? []).length > 0 && (
          <p className="small">{t("setup.projects.have", { names: (projects.data ?? []).map((p) => p.name).join(", ") })}</p>
        )}
      </div>
      <div className="card stack">
        <strong>{t("setup.projects.docsTitle")}</strong>
        <span className="muted small">{t("setup.projects.docsWhy")}</span>
        <div className="row">
          <button
            type="button"
            className="secondary"
            disabled={busy}
            onClick={() =>
              void run(async () => {
                const r = await client.op("choose_folder", { prompt: t("setup.projects.docsChoose") });
                if (!r.path) return;
                await client.op("index_project", { cwd: r.path });
                await set({ add_documents: r.path });
              })
            }
          >
            {t("setup.projects.docsChoose")}
          </button>
        </div>
        {documents.map((d) => (
          <p key={d} className="small">
            ✓ {d}
          </p>
        ))}
      </div>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </StepFrame>
  );
}

/** What a finished step says in the checklist. */
function useStatusLines(models: Model[]): Record<Step, string> {
  const { t } = useI18n();
  const prefs = usePreferences();
  const resources = useResources();
  const connections = useOp("connections");
  const projects = useOp("list_projects");
  const chat = models.filter((m) => !m.embedding);
  const purposes = prefs.data?.purposes ?? [];
  const connected = (connections.data ?? []).filter((c) => c.connected).map((c) => t(`connect.${c.client}` as Key));
  const docs = prefs.data?.documents ?? [];
  const p = (projects.data ?? []).length;
  return {
    purpose: purposes.map((x) => t(`choose.purpose.${x}` as Key)).join(", "),
    model: chat.map((m) => m.name).join(", ") || t("setup.none"),
    resources: resources.data ? t(`cockpit.level.${resources.data.settings.level}` as Key) : "",
    connect: connected.join(", ") || t("setup.none"),
    projects: [p ? t("setup.projects.count", { n: p }) : "", docs.length ? t("setup.projects.docsCount", { n: docs.length }) : ""].filter(Boolean).join(" · ") || t("setup.none"),
  };
}

/**
 * The start page: setting Ancilo up step by step – what it is for, the AI
 * that suits this computer, how much of the computer it may take, Claude
 * Code/Codex, projects and documents. Afterwards a checklist to change any
 * step.
 */
export function SetupPage() {
  const { t } = useI18n();
  const prefs = usePreferences();
  const models = useOp("list_models");
  const [editing, setEditing] = useState<Step | null>(null);
  const status = useStatusLines(models.data ?? []);
  const web = useOp("get_web_search").data?.provider ?? "off";
  if (!prefs.data || !models.data) return null;
  const setup = prefs.data.setup ?? {};
  const hasChat = models.data.some((m) => !m.embedding);
  const complete = (s: Step) => (s === "model" ? hasChat && setup[s] === "done" : setup[s] === "done" || setup[s] === "skipped");
  const firstOpen = STEPS.find((s) => !complete(s)) ?? null;
  const current = editing ?? firstOpen;
  const go = (s: Step | null) => setEditing(s);
  const nextOf = (s: Step) => {
    if (editing) return () => go(null);
    const i = STEPS.indexOf(s);
    return () => go(STEPS.slice(i + 1).find((x) => !complete(x)) ?? null);
  };
  const backOf = (s: Step) => () => go(STEPS[Math.max(0, STEPS.indexOf(s) - 1)] ?? null);

  let body: ReactNode;
  switch (current) {
    case "purpose":
      body = <PurposeStep onDone={nextOf("purpose")} />;
      break;
    case "model":
      body = <ModelStep models={models.data} onDone={nextOf("model")} onBack={backOf("model")} />;
      break;
    case "resources":
      body = <ResourcesStep onDone={nextOf("resources")} onBack={backOf("resources")} />;
      break;
    case "connect":
      body = <ConnectStep onDone={nextOf("connect")} onBack={backOf("connect")} />;
      break;
    case "projects":
      body = <ProjectsStep onDone={nextOf("projects")} onBack={backOf("projects")} />;
      break;
    default:
      body = (
        <section className="stack" data-testid="setup-checklist">
          <div className="card done-card stack">
            <h2>{t("setup.doneTitle")}</h2>
            <p>{t("setup.doneHint")}</p>
            <div className="row">
              <button type="button" onClick={() => navigate({ view: "chat", id: null })}>
                {t("choose.askNow")}
              </button>
            </div>
          </div>
          <ul className="checklist">
            {STEPS.map((s) => (
              <li key={s}>
                <span className={setup[s] === "skipped" ? "check skipped" : "check"} aria-hidden="true">
                  {setup[s] === "skipped" ? "–" : "✓"}
                </span>
                <span className="check-text">
                  <strong>{t(`setup.${s}.title` as Key)}</strong>
                  <span className="muted small">{setup[s] === "skipped" ? t("setup.skipped") : status[s]}</span>
                </span>
                <button type="button" className="ghost" onClick={() => go(s)} aria-label={t("setup.changeStep", { step: t(`setup.${s}.title` as Key) })}>
                  {t("setup.change")}
                </button>
              </li>
            ))}
            <li>
              <span className={web === "off" ? "check skipped" : "check"} aria-hidden="true">
                {web === "off" ? "–" : "✓"}
              </span>
              <span className="check-text">
                <strong>{t("web.title")}</strong>
                <span className="muted small">{t(`web.status.${web}` as Key)}</span>
              </span>
              <button type="button" className="ghost" onClick={() => navigate({ view: "web" })} aria-label={t("setup.changeStep", { step: t("web.title") })}>
                {t("setup.change")}
              </button>
            </li>
          </ul>
        </section>
      );
  }
  return (
    <div className="page">
      <div className="setup stack" data-testid="setup">
        <div className="welcome-head">
          <h1>{t("setup.title")}</h1>
          {current && !editing && STEPS.indexOf(current) === 0 && <p>{t("welcome.what")}</p>}
        </div>
        {body}
        {current && OPTIONAL.includes(current) && <p className="muted small">{t("setup.optional")}</p>}
      </div>
    </div>
  );
}
