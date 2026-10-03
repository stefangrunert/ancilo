import { useMemo, useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { usePro } from "../state/prefs";
import { navigate } from "../state/route";
import { useClient, useLive, useOp, useRefresh } from "../state/store";
import { AccessChoice, Approvals, blocks, Steps } from "./AgentParts";
import { ChatLayout, Composer, Thinking, UserBubble } from "./Chat";
import { nameOf } from "./ChatProjects";
import { Icon } from "./Icon";
import { Markdown } from "./Markdown";
import { Dialog, ErrorNote } from "./ui";

type Session = OpOutput<"get_session">;
type Change = Session["changes"][number];

/** Files chosen for a task before it exists: sent once it is created. */
function PendingFiles({ files, onRemove }: { files: File[]; onRemove: (i: number) => void }) {
  const { t } = useI18n();
  if (files.length === 0) return null;
  return (
    <div className="doc-row" data-testid="task-files">
      {files.map((f, i) => (
        <span key={`${f.name}-${i}`} className="doc-chip">
          <Icon name="doc" size={15} />
          <span className="doc-name">{f.name}</span>
          <button type="button" className="icon" aria-label={t("doc.remove", { name: f.name })} onClick={() => onRemove(i)}>
            <Icon name="close" size={12} />
          </button>
        </span>
      ))}
    </div>
  );
}

/** Starts a task in a folder: the agent works in a copy of it; the files
 * given go into that copy. */
function useStartTask() {
  const client = useClient();
  const refresh = useRefresh();
  return async (text: string, folder: string, files: File[]) => {
    const title = (text.trim().split("\n")[0] ?? "").slice(0, 60);
    const s = await client.op("create_task", { folder, title });
    for (const f of files) await client.addTaskFile(s.id, f.name, f);
    await client.op("send_message", { session: s.id, text });
    await refresh("list_sessions", "list_projects");
    navigate({ view: "task", id: s.id });
  };
}

/** Asks for a folder: the system's dialog – or, where there is none, its path. */
function useChooseFolder() {
  const { t } = useI18n();
  const client = useClient();
  return async (): Promise<string | null> => {
    try {
      const r = await client.op("choose_folder", { prompt: t("tasks.chooseFolderPrompt") });
      return r.path ?? null;
    } catch {
      const typed = window.prompt(t("tasks.typeFolder"));
      return typed?.trim() || null;
    }
  };
}

const EXAMPLES: Key[] = ["tasksArea.example1", "tasksArea.example2", "tasksArea.example3"];

/** The Tasks area's start, in two steps: first the folder, then what to do
 * (with examples to start from). `preset`: a folder's own page – fixed. */
export function TasksAreaPage({ folder: preset = null }: { folder?: string | null }) {
  const { t } = useI18n();
  const start = useStartTask();
  const choose = useChooseFolder();
  const [folder, setFolder] = useState<string | null>(preset);
  const [files, setFiles] = useState<File[]>([]);
  // A chosen example fills the input (a new key starts it with that text).
  const [draft, setDraft] = useState({ text: "", n: 0 });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const pick = async () => {
    setError(null);
    try {
      const p = await choose();
      if (p) setFolder(p);
      return p;
    } catch (e) {
      setError(e);
      return null;
    }
  };
  return (
    <div className="page" data-testid="tasks-area">
      <div className="build stack">
        <h1 className="page-title">
          <Icon name="computer" size={24} /> {t("tasks.new")}
        </h1>
        <p className="muted">{t("tasksArea.intro")}</p>
        <h2 className="task-step">
          <span className="task-step-n">1</span> {t("tasks.stepFolder")}
        </h2>
        {folder ? (
          <div className="task-folder-row" data-testid="task-folder-chosen">
            <Icon name="folder" size={20} />
            <div className="grow">
              <strong title={folder}>{nameOf(folder)}</strong>
              <div className="muted small">{t("tasks.folderSafe")}</div>
            </div>
            {!preset && (
              <button type="button" className="secondary" onClick={() => void pick()}>
                {t("tasks.changeFolder")}
              </button>
            )}
          </div>
        ) : (
          <div className="task-folder-pick">
            <Icon name="folder" size={24} />
            <p>{t("tasks.folderSafe")}</p>
            <p className="muted">{t("tasks.folderPlease")}</p>
            <button type="button" onClick={() => void pick()}>
              {t("tasks.chooseFolder")}
            </button>
          </div>
        )}
        <h2 className={folder ? "task-step" : "task-step off"}>
          <span className="task-step-n">2</span> {t("tasks.ask")}
        </h2>
        <Composer
          key={draft.n}
          initial={draft.text}
          label={t("tasks.ask")}
          placeholder={t(folder ? "tasks.askFolderHint" : "tasks.folderFirst")}
          busy={busy}
          disabled={!folder}
          autoFocus={!!folder}
          onFiles={folder ? (f) => setFiles((x) => [...x, ...f]) : undefined}
          above={<PendingFiles files={files} onRemove={(i) => setFiles((x) => x.filter((_, j) => j !== i))} />}
          onSend={async (text) => {
            if (!folder) return;
            setError(null);
            setBusy(true);
            try {
              await start(text, folder, files);
            } catch (e) {
              setError(e);
              throw e;
            } finally {
              setBusy(false);
            }
          }}
        />
        <div className="task-examples" role="group" aria-label={t("tasks.examples")}>
          <span className="muted small">{t("tasks.examples")}</span>
          {EXAMPLES.map((k) => (
            <button
              key={k}
              type="button"
              className="chip"
              onClick={async () => {
                // No folder yet? It comes first – then the example.
                if (!folder && !(await pick())) return;
                setDraft((d) => ({ text: t(k), n: d.n + 1 }));
              }}
            >
              {t(k)}
            </button>
          ))}
        </div>
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    </div>
  );
}

/** A folder Ancilo worked in: a new task there, and its tasks. */
export function TaskFolderPage({ path }: { path: string }) {
  const { t } = useI18n();
  const sessions = useOp("list_sessions", {});
  const mine = (sessions.data ?? []).filter((s) => s.project === path && s.kind === "task" && !s.free);
  return (
    <div className="stack" data-testid="task-folder">
      <TasksAreaPage folder={path} />
      {mine.length > 0 && (
        <section className="build stack">
          <h2>{t("tasks.inFolder")}</h2>
          <ul className="plain-list">
            {mine.map((s) => (
              <li key={s.id}>
                <button type="button" className="link" onClick={() => navigate({ view: "task", id: s.id })}>
                  {s.title}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}

const CHANGE_LABEL: Record<string, Key> = {
  added: "task.change.added",
  modified: "task.change.modified",
  deleted: "task.change.deleted",
  renamed: "task.change.renamed",
};

function ChangeLine({ c }: { c: Change }) {
  const { t } = useI18n();
  const kind = c.change ?? "modified";
  return (
    <li className={`task-change ${kind}`} data-testid="task-change">
      <span className="badge">{t(CHANGE_LABEL[kind] ?? "task.change.modified")}</span>
      <code>{c.path}</code>
      {c.from && <span className="muted small">{t("task.change.from", { from: c.from })}</span>}
    </li>
  );
}

/** What the task changed in the copy: keep it (into the folder) or drop it;
 * after keeping, take it back. */
function TaskChanges({ s, onError }: { s: Session; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [dropping, setDropping] = useState(false);
  const [undoing, setUndoing] = useState(false);
  const act = async (f: () => Promise<unknown>) => {
    try {
      await f();
      await refresh("get_session", "list_sessions");
    } catch (e) {
      onError(e);
    }
  };
  const running = s.status === "running";
  if (s.changes.length === 0) {
    if (!s.applied || running) return null;
    return (
      <div className="card changes-card" data-testid="task-applied">
        <p>{t("task.applied", { n: s.applied.changes.length, folder: nameOf(s.project) })}</p>
        <div className="row">
          <button type="button" className="secondary" onClick={() => setUndoing(true)}>
            {t("task.undo")}
          </button>
        </div>
        <Dialog open={undoing} title={t("confirm.title")} onClose={() => setUndoing(false)}>
          <p>{t("task.undoConfirm", { n: s.applied.changes.length })}</p>
          <div className="row end">
            <button type="button" className="secondary" onClick={() => setUndoing(false)}>
              {t("confirm.no")}
            </button>
            <button
              type="button"
              onClick={() => {
                setUndoing(false);
                void act(() => client.op("undo_apply", { session: s.id }, true));
              }}
            >
              {t("task.undo")}
            </button>
          </div>
        </Dialog>
      </div>
    );
  }
  if (running) return null;
  return (
    <div className="card changes-card" data-testid="task-changes">
      <strong>{t("task.changes", { n: s.changes.length })}</strong>
      <p className="muted small">{t("task.notYet", { folder: nameOf(s.project) })}</p>
      <ul className="task-change-list">
        {s.changes.map((c) => (
          <ChangeLine key={c.path} c={c} />
        ))}
      </ul>
      <div className="row">
        <button type="button" onClick={() => void act(() => client.op("apply_changes", { session: s.id, paths: null, version: s.changes_version ?? null }, true))}>
          {t("task.keep")}
        </button>
        <button type="button" className="secondary" onClick={() => setDropping(true)}>
          {t("task.drop")}
        </button>
      </div>
      <Dialog open={dropping} title={t("confirm.title")} onClose={() => setDropping(false)}>
        <p>{t("task.dropConfirm", { n: s.changes.length })}</p>
        <div className="row end">
          <button type="button" className="secondary" onClick={() => setDropping(false)}>
            {t("confirm.no")}
          </button>
          <button
            type="button"
            onClick={() => {
              setDropping(false);
              void act(() => client.op("discard_changes", { session: s.id, paths: null }));
            }}
          >
            {t("task.drop")}
          </button>
        </div>
      </Dialog>
    </div>
  );
}

/** A free task's results: new files to save – into Documents with one
 * click, or elsewhere; then open them or show them in the Finder. */
function TaskResults({ s, onError }: { s: Session; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const choose = useChooseFolder();
  const act = async (f: () => Promise<unknown>) => {
    try {
      await f();
      await refresh("get_session", "list_sessions");
    } catch (e) {
      onError(e);
    }
  };
  if (s.status === "running") return null;
  if (s.changes.length === 0) {
    if (!s.saved) return null;
    const dir = nameOf(s.saved.dir);
    return (
      <div className="card changes-card" data-testid="task-saved">
        <p>{t("results.saved", { n: s.saved.files.length, dir })}</p>
        <ul className="task-change-list">
          {s.saved.files.map((f) => (
            <li key={f} className="task-change">
              <code>{nameOf(f)}</code>
              <button type="button" className="link" onClick={() => void act(() => client.op("open_document", { path: f }))}>
                {t("results.open")}
              </button>
            </li>
          ))}
        </ul>
        <div className="row">
          <button type="button" className="secondary" onClick={() => void act(() => client.op("show_in_finder", { path: s.saved!.files[0] ?? s.saved!.dir }))}>
            {t("results.show")}
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className="card changes-card" data-testid="task-results">
      <strong>{t("results.title", { n: s.changes.length })}</strong>
      <ul className="task-change-list">
        {s.changes.map((c) => (
          <li key={c.path} className="task-change added">
            <Icon name="doc" size={14} />
            <code>{c.path}</code>
          </li>
        ))}
      </ul>
      <div className="row">
        <button type="button" onClick={() => void act(() => client.op("save_results", { session: s.id }))}>
          {t("results.save")}
        </button>
        <button
          type="button"
          className="secondary"
          onClick={async () => {
            const dir = await choose();
            if (dir) await act(() => client.op("save_results", { session: s.id, dir }));
          }}
        >
          {t("results.elsewhere")}
        </button>
      </div>
    </div>
  );
}

/** A task: the conversation with the agent, what it changed, keep or drop. */
export function TaskView({ id }: { id: string }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const live = useLive();
  const pro = usePro();
  const session = useOp("get_session", { session: id });
  const [error, setError] = useState<unknown>(null);
  const s = session.data;
  const parts = useMemo(() => blocks(s?.messages ?? []), [s?.messages]);
  if (session.error) return <ErrorNote error={session.error} />;
  if (!s) return <p className="muted" style={{ padding: 20 }}>…</p>;
  const running = s.status === "running";
  const activity = (live.activity[s.id] ?? []).slice(-3);
  const lastSteps = parts.map((p) => p.kind).lastIndexOf("steps");
  const send = async (text: string) => {
    setError(null);
    try {
      await client.op("send_message", { session: s.id, text });
      await refresh("get_session", "list_sessions");
    } catch (e) {
      setError(e);
      throw e;
    }
  };
  const addFiles = async (files: File[]) => {
    setError(null);
    try {
      for (const f of files) await client.addTaskFile(s.id, f.name, f);
      await refresh("get_session");
    } catch (e) {
      setError(e);
    }
  };
  return (
    <div className="session-view" data-testid="task" data-status={s.status}>
      <section className="session-chat" aria-label={s.title}>
        <header className="session-head">
          <h2>{s.title}</h2>
          {!s.free && (
            <button type="button" className="ghost project-path" title={s.project} onClick={() => navigate({ view: "task-folder", path: s.project })}>
              <Icon name="folder" size={14} /> {nameOf(s.project)}
            </button>
          )}
          <span className="spacer" />
        </header>
        <ChatLayout
          follow={`${parts.length}:${s.status}:${activity.length}:${s.approvals.length}:${s.changes.length}`}
          composer={
            <Composer
              label={t("tasks.next")}
              placeholder={t("tasks.nextHint")}
              onSend={send}
              busy={running}
              onStop={() => void client.op("cancel_turn", { session: s.id }).catch(setError)}
              onFiles={(f) => void addFiles(f)}
              extra={<AccessChoice s={s} task disabled={running} onChange={(permission) => void client.op("update_session", { session: s.id, permission }).then(() => refresh("get_session")).catch(setError)} />}
              autoFocus
            />
          }
        >
          <p className="local-note">
            <Icon name="lock" size={13} />
            {t(s.free ? "tasksArea.safeFree" : "tasksArea.safe")}
          </p>
          <div className="stack" data-testid="messages" aria-live="polite">
            {parts.map((p, i) =>
              p.kind === "user" ? (
                <UserBubble key={i} text={p.text} />
              ) : p.kind === "say" ? (
                <Markdown key={i} text={p.text} />
              ) : pro ? (
                <Steps key={i} steps={p.steps} open={running && i === lastSteps} />
              ) : (
                <p key={i} className="muted small">
                  {t("code.worked", { n: p.steps.length })}
                </p>
              ),
            )}
            {running && <Thinking label={t("tasks.working")} lines={pro ? activity : []} />}
            {s.status === "interrupted" && <p className="muted">{t("code.interrupted")}</p>}
          </div>
          <Approvals s={s} onError={setError} />
          {s.free ? <TaskResults s={s} onError={setError} /> : <TaskChanges s={s} onError={setError} />}
          <ErrorNote error={error} onDismiss={() => setError(null)} />
        </ChatLayout>
      </section>
    </div>
  );
}
