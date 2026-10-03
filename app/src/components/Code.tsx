import { useEffect, useMemo, useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { usePro } from "../state/prefs";
import { navigate } from "../state/route";
import { useClient, useLive, useOp, useRefresh } from "../state/store";
import { ChatLayout, Composer, UserBubble } from "./Chat";
import { Markdown } from "./Markdown";
import { TerminalView } from "./Terminal";
import { Dialog, ErrorNote, StatusDot } from "./ui";
import { WebSwitch } from "./WebSearch";
import { AccessChoice, Approvals, blocks, LiveWork, Steps, type Access } from "./AgentParts";

export { blocks };

type Model = OpOutput<"list_models">[number];
type Session = OpOutput<"get_session">;

/** Colored unified diff. */
export function Patch({ text }: { text: string }) {
  const lines = text.split("\n");
  return (
    <pre className="patch" data-testid="patch">
      {lines.map((l, i) => {
        const cls =
          l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ") || l.startsWith("index ")
            ? "meta"
            : l.startsWith("+")
              ? "add"
              : l.startsWith("-")
                ? "del"
                : l.startsWith("@@")
                  ? "hunk"
                  : "";
        return (
          <span key={i} className={cls}>
            {l}
            {"\n"}
          </span>
        );
      })}
    </pre>
  );
}

function DiffOf({ session, variant, path }: { session: string; variant?: string; path?: string }) {
  const diff = useOp("session_diff", { session, variant: variant ?? null, path: path ?? null });
  if (diff.isLoading) return <p className="muted">…</p>;
  if (diff.error) return <ErrorNote error={diff.error} />;
  return <Patch text={diff.data?.patch ?? ""} />;
}

function useAct(onError: (e: unknown) => void) {
  const refresh = useRefresh();
  return async (f: () => Promise<unknown>) => {
    try {
      await f();
      await refresh("get_session", "session_diff", "list_sessions", "list_projects");
    } catch (e) {
      onError(e);
    }
  };
}

function Changes({ s, onError }: { s: Session; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const act = useAct(onError);
  const [selected, setSelected] = useState<string[]>([]);
  const [shown, setShown] = useState<string | null>(null);
  const [discarding, setDiscarding] = useState(false);
  const files = s.changes;
  const paths = files.map((f) => f.path).join("\n");
  useEffect(() => {
    // Selection follows the files that still have changes.
    setSelected((sel) => sel.filter((p) => paths.split("\n").includes(p)));
  }, [paths]);
  if (files.length === 0) return <p className="muted" data-testid="no-changes">{t("code.noChanges")}</p>;
  const running = s.status === "running";
  const some = selected.length > 0 ? selected : null;
  return (
    <div className="changes stack" data-testid="changes">
      <p className="muted small">{t("code.notYetInProject")}</p>
      <KeepNote s={s} />
      <ul>
        {files.map((f) => (
          <li key={f.path}>
            <label>
              <input
                type="checkbox"
                checked={selected.includes(f.path)}
                onChange={(e) => setSelected((c) => (e.target.checked ? [...c, f.path] : c.filter((x) => x !== f.path)))}
                aria-label={t("code.select", { path: f.path })}
              />
            </label>
            <button type="button" className="link file" aria-expanded={shown === f.path} onClick={() => setShown(shown === f.path ? null : f.path)}>
              {f.path}
            </button>
            {f.runs && (
              <span className="badge warn" title={t("code.runsHint")}>
                {t("code.runs")}
              </span>
            )}
            <span className="stat">
              <span className="add">+{f.added}</span> <span className="del">−{f.removed}</span>
            </span>
          </li>
        ))}
      </ul>
      {shown && <DiffOf session={s.id} path={shown} />}
      <div className="row">
        <button type="button" disabled={running} onClick={() => void act(() => client.op("apply_changes", { session: s.id, paths: some }, true))}>
          {some ? t("code.applySelected", { n: some.length }) : t("code.applyAll")}
        </button>
        <button type="button" className="secondary" disabled={running} onClick={() => setDiscarding(true)}>
          {some ? t("code.discardSelected", { n: some.length }) : t("code.discardAll")}
        </button>
      </div>
      <Dialog open={discarding} title={t("confirm.title")} onClose={() => setDiscarding(false)}>
        <p>{t("code.discardConfirm", { n: some?.length ?? files.length })}</p>
        <div className="row end">
          <button type="button" className="secondary" onClick={() => setDiscarding(false)}>
            {t("confirm.no")}
          </button>
          <button
            type="button"
            onClick={() => {
              setDiscarding(false);
              void act(() => client.op("discard_changes", { session: s.id, paths: some }));
            }}
          >
            {t("code.discard")}
          </button>
        </div>
      </Dialog>
    </div>
  );
}

function Variants({ s, models, onError }: { s: Session; models: Model[]; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const act = useAct(onError);
  const others = models.filter((m) => m.id !== s.model);
  const [model, setModel] = useState("");
  const pick = model || others[0]?.id || "";
  return (
    <div className="variants">
      {s.can_retry && others.length > 0 && (
        <div className="row">
          <label className="row">
            <span>{t("code.retryWith")}</span>
            <select value={pick} onChange={(e) => setModel(e.target.value)}>
              {others.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          </label>
          <button type="button" className="secondary" onClick={() => void act(() => client.op("retry_with_model", { session: s.id, model: pick }))}>
            {t("code.retry")}
          </button>
        </div>
      )}
      {s.variants.length > 0 && (
        <div className="side-by-side" data-testid="variants">
          <div className="variant">
            <h3>{t("code.current", { model: s.model })}</h3>
            <DiffOf session={s.id} />
          </div>
          {s.variants.map((v) => (
            <div className="variant" key={v.id} data-testid={`variant-${v.model}`}>
              <h3>
                <StatusDot status={v.status === "running" ? "starting" : "running"} /> {v.model}
              </h3>
              {v.status === "running" ? (
                <p className="muted" role="status">
                  {t("code.working")}
                </p>
              ) : (
                <>
                  {v.summary && <Markdown text={v.summary} />}
                  <DiffOf session={s.id} variant={v.id} />
                  <div className="row">
                    <button type="button" onClick={() => void act(() => client.op("apply_changes", { session: s.id, variant: v.id }, true))}>
                      {t("code.takeVariant")}
                    </button>
                    <button type="button" className="secondary" onClick={() => void act(() => client.op("discard_changes", { session: s.id, variant: v.id }))}>
                      {t("code.dropVariant")}
                    </button>
                  </div>
                </>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** Before keeping: changes that run when the project is built or installed,
 * and sessions that read web pages, deserve a closer look. */
function KeepNote({ s }: { s: Session }) {
  const { t } = useI18n();
  const runs = s.changes.filter((f) => f.runs).map((f) => f.path);
  if (runs.length === 0 && !s.web_used) return null;
  return (
    <div className="note" data-testid="keep-note">
      {runs.length > 0 && <p>{t("code.runsNote", { files: runs.slice(0, 3).join(", ") + (runs.length > 3 ? " …" : "") })}</p>}
      {s.web_used && <p>{t("code.webNote")}</p>}
    </div>
  );
}

/** After a turn: what changed, and the way into the project. */
function ChangesCard({ s, onReview, onError, pro }: { s: Session; onReview: () => void; onError: (e: unknown) => void; pro: boolean }) {
  const { t } = useI18n();
  const client = useClient();
  const act = useAct(onError);
  const [undoing, setUndoing] = useState(false);
  if (s.changes.length === 0 || s.status === "running") return null;
  const added = s.changes.reduce((n, f) => n + f.added, 0);
  const removed = s.changes.reduce((n, f) => n + f.removed, 0);
  return (
    <div className="card changes-card" data-testid="changes-card">
      <div className="row">
        <strong>{t("code.changedFiles", { n: s.changes.length })}</strong>
        <span className="stat">
          <span className="add">+{added}</span> <span className="del">−{removed}</span>
        </span>
      </div>
      <p className="muted small">{t("code.notYetInProject")}</p>
      <KeepNote s={s} />
      <div className="row">
        <button type="button" onClick={() => void act(() => client.op("apply_changes", { session: s.id, paths: null }, true))}>
          {pro ? t("code.applyToProject") : t("code.keep")}
        </button>
        {pro ? (
          <button type="button" className="secondary" onClick={onReview}>
            {t("code.review")}
          </button>
        ) : (
          <button type="button" className="secondary" onClick={() => setUndoing(true)}>
            {t("code.undo")}
          </button>
        )}
      </div>
      <Dialog open={undoing} title={t("confirm.title")} onClose={() => setUndoing(false)}>
        <p>{t("code.undoConfirm")}</p>
        <div className="row end">
          <button type="button" className="secondary" onClick={() => setUndoing(false)}>
            {t("confirm.no")}
          </button>
          <button
            type="button"
            onClick={() => {
              setUndoing(false);
              void act(() => client.op("discard_changes", { session: s.id, paths: null }));
            }}
          >
            {t("code.undo")}
          </button>
        </div>
      </Dialog>
    </div>
  );
}

function Terminals({ s, onError }: { s: Session; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const list = useOp("list_terminals");
  const mine = (list.data ?? []).filter((x) => x.session === s.id);
  const [active, setActive] = useState<string | null>(null);
  const current = mine.find((x) => x.id === active)?.id ?? mine[mine.length - 1]?.id ?? null;
  const open = async () => {
    try {
      const r = await client.op("open_terminal", { session: s.id });
      setActive(r.terminal.id);
      await refresh("list_terminals");
    } catch (e) {
      onError(e);
    }
  };
  const close = async (id: string) => {
    try {
      await client.op("close_terminal", { terminal: id });
      await refresh("list_terminals");
    } catch (e) {
      onError(e);
    }
  };
  return (
    <div className="terminals">
      <p className="muted small">{t("terminal.where")}</p>
      <div className="row">
        {mine.length > 0 && (
          <div className="row tabs" role="tablist" aria-label={t("terminal.label")}>
            {mine.map((x, i) => (
              <button key={x.id} type="button" role="tab" aria-selected={x.id === current} className="tab-button" onClick={() => setActive(x.id)}>
                {t("terminal.name", { n: i + 1 })}
              </button>
            ))}
          </div>
        )}
        {current && (
          <button type="button" className="link" onClick={() => void close(current)}>
            {t("terminal.close", { n: mine.findIndex((x) => x.id === current) + 1 })}
          </button>
        )}
        <span className="spacer" />
        <button type="button" className="secondary" onClick={() => void open()}>
          {t("terminal.open")}
        </button>
      </div>
      {current && <TerminalView key={current} id={current} />}
    </div>
  );
}

const wide = () => typeof window === "undefined" || window.innerWidth > 1100;

/** A coding session: the conversation with the agent, and its changes and terminals beside it. */
export function SessionView({ id, models }: { id: string; models: Model[] }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const live = useLive();
  const session = useOp("get_session", { session: id });
  const [error, setError] = useState<unknown>(null);
  const [tab, setTab] = useState<"changes" | "terminal">("changes");
  const [panelOpen, setPanel] = useState(wide);
  const pro = usePro();
  // The changes panel (diffs, terminal, variants) is for experts.
  const panel = pro && panelOpen;
  const local = useMemo(() => models.filter((m) => !m.embedding && !m.cloud), [models]);
  const s = session.data;
  const parts = useMemo(() => blocks(s?.messages ?? []), [s?.messages]);
  if (session.error) return <ErrorNote error={session.error} />;
  if (!s) return <p className="muted" style={{ padding: 20 }}>…</p>;
  const running = s.status === "running";
  const update = async (input: { model?: string; permission?: Access }) => {
    try {
      await client.op("update_session", { session: s.id, ...input });
      await refresh("get_session", "list_sessions");
    } catch (e) {
      setError(e);
    }
  };
  const send = async (text: string) => {
    setError(null);
    try {
      await client.op("send_message", { session: s.id, text });
      await refresh("get_session", "list_sessions", "list_projects");
    } catch (e) {
      setError(e);
      throw e;
    }
  };
  const known = local.some((m) => m.id === s.model);
  const lastSteps = parts.map((p) => p.kind).lastIndexOf("steps");
  const project = s.project.split("/").pop() ?? s.project;
  const modelChoice = (
    <label className="row">
      <span className="sr-only">{t("code.model")}</span>
      <select value={known ? s.model : ""} onChange={(e) => void update({ model: e.target.value })} disabled={running} aria-label={t("code.model")}>
        {!known && <option value="">{s.model}</option>}
        {local.map((m) => (
          <option key={m.id} value={m.id}>
            {m.name}
          </option>
        ))}
      </select>
    </label>
  );
  return (
    <div className={panel ? "session-view with-panel" : "session-view"} data-testid="session" data-status={s.status}>
      <section className="session-chat" aria-label={s.title}>
        <header className="session-head">
          <h2>{s.title}</h2>
          <button type="button" className="ghost project-path" title={s.project} onClick={() => navigate({ view: "project", root: s.project })}>
            {project}
          </button>
          <span className="spacer" />
          {pro && (
            <button type="button" className="secondary" aria-pressed={panel} onClick={() => setPanel((p) => !p)}>
              {panel ? t("code.hidePanel") : t("code.showPanel", { n: s.changes.length })}
            </button>
          )}
        </header>
        <ChatLayout
          follow={`${parts.length}:${s.status}:${live.turns[s.id]?.notes.length ?? 0}:${live.activity[s.id]?.length ?? 0}:${s.approvals.length}:${s.changes.length}`}
          composer={
            <Composer
              label={t("code.placeholder")}
              placeholder={t("code.placeholderLong", { project })}
              onSend={send}
              busy={running}
              onStop={() => void client.op("cancel_turn", { session: s.id }).catch(setError)}
              extra={
                <>
                  <AccessChoice s={s} disabled={running} onChange={(permission) => void update({ permission })} />
                  <WebSwitch onError={setError} />
                  {pro && modelChoice}
                </>
              }
              autoFocus
            />
          }
        >
          {!s.isolated && <p className="note">{t("code.noGit")}</p>}
          <div className="stack" data-testid="messages" aria-live="polite">
            {parts.length === 0 && (
              <div className="empty muted">
                <p>{t("code.start")}</p>
                <p className="small">{t("code.copyExplained", { project: s.project })}</p>
              </div>
            )}
            {parts.map((p, i) =>
              p.kind === "user" ? (
                <UserBubble key={i} text={p.text} />
              ) : p.kind === "say" ? (
                <Markdown key={i} text={p.text} />
              ) : (
                pro ? (
                  <Steps key={i} steps={p.steps} open={running && i === lastSteps} />
                ) : (
                  <p key={i} className="muted small">
                    {t("code.worked", { n: p.steps.length })}
                  </p>
                )
              ),
            )}
            {running && <LiveWork id={s.id} label={t("code.working")} />}
            {s.status === "interrupted" && <p className="muted">{t("code.interrupted")}</p>}
          </div>
          <Approvals s={s} onError={setError} />
          <ChangesCard
            pro={pro}
            s={s}
            onError={setError}
            onReview={() => {
              setTab("changes");
              setPanel(true);
            }}
          />
          <ErrorNote error={error} onDismiss={() => setError(null)} />
        </ChatLayout>
      </section>
      {panel && (
        <aside className="work-panel" aria-label={t("code.work")}>
          <div className="row tabs" role="tablist">
            <button type="button" role="tab" aria-selected={tab === "changes"} className="tab-button" onClick={() => setTab("changes")}>
              {t("code.changes", { n: s.changes.length })}
            </button>
            <button type="button" role="tab" aria-selected={tab === "terminal"} className="tab-button" onClick={() => setTab("terminal")}>
              {t("terminal.label")}
            </button>
          </div>
          <div className="work-body">
            {tab === "changes" ? (
              <>
                <Changes s={s} onError={setError} />
                <Variants s={s} models={local} onError={setError} />
              </>
            ) : (
              <Terminals s={s} onError={setError} />
            )}
          </div>
        </aside>
      )}
    </div>
  );
}

function when(iso: string, lang: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(lang, { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
}

/** A project: start a session with a first task, or continue one. */
export function ProjectView({ root, models }: { root: string; models: Model[] }) {
  const { t, lang } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [error, setError] = useState<unknown>(null);
  const project = useOp("open_project", { path: root });
  const sessions = useOp("list_sessions", { project: project.data?.root ?? root });
  const local = models.filter((m) => !m.embedding && !m.cloud);
  const p = project.data;
  const start = async (text?: string) => {
    if (!p) return;
    setError(null);
    try {
      const s = await client.op("create_session", { cwd: p.root });
      if (text) await client.op("send_message", { session: s.id, text });
      await refresh("list_sessions", "list_projects");
      navigate({ view: "session", id: s.id });
    } catch (e) {
      setError(e);
      throw e;
    }
  };
  if (project.error) {
    return (
      <div className="project">
        <ErrorNote error={project.error} />
      </div>
    );
  }
  const list = sessions.data ?? [];
  return (
    <div className="page">
      <div className="project">
        <div className="project-head">
          <h1>{p?.name ?? root.split("/").pop()}</h1>
          <span className="project-path">{p?.root ?? root}</span>
          {p && !p.git && <p className="muted small">{t("code.noGitProject")}</p>}
        </div>
        {local.length === 0 ? (
          <p className="note">{t("code.noModel")}</p>
        ) : (
          <>
            <Composer label={t("code.placeholder")} placeholder={t("code.firstTask", { project: p?.name ?? "" })} onSend={(text) => start(text)} disabled={!p} autoFocus />
            <div className="row">
              <button type="button" className="secondary" disabled={!p} onClick={() => void start().catch(() => {})}>
                {t("code.newSession")}
              </button>
              <span className="muted small">{t("code.copyExplained", { project: p?.root ?? root })}</span>
            </div>
          </>
        )}
        <ErrorNote error={error} onDismiss={() => setError(null)} />
        {list.length > 0 && (
          <section aria-label={t("code.sessions")}>
            <h3>{t("code.sessions")}</h3>
            <ul className="session-list">
              {list.map((s) => (
                <li key={s.id}>
                  <button type="button" className="nav-item" onClick={() => navigate({ view: "session", id: s.id })}>
                    <StatusDot status={s.status === "running" ? "starting" : s.changes.length ? "running" : "idle"} />
                    <span className="text">{s.title}</span>
                    {s.changes.length > 0 && <span className="muted small">{t("code.changedFiles", { n: s.changes.length })}</span>}
                  </button>
                  <span className="when">{when(s.created_at, lang)}</span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </div>
  );
}
