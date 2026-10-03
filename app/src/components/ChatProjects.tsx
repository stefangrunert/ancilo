import { useState } from "react";
import { useI18n } from "../i18n";
import { navigate } from "../state/route";
import { useClient, useOp, useRefresh } from "../state/store";
import { Composer, Thinking } from "./Chat";
import { Icon } from "./Icon";
import { ErrorNote } from "./ui";

/** The last part of a path: a folder's name. */
export function nameOf(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

/** What Ancilo has read of a folder – and what it could not read, and why. */
export function FolderStatus({ path }: { path: string }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  // While Ancilo reads, the progress comes as events; this keeps it current anyway.
  const status = useOp("folder_documents", { folder: path });
  const s = status.data;
  if (!s) return null;
  const notRead = s.not_read ?? [];
  return (
    <div className="folder-status stack" data-testid="folder-status">
      <p className="row">
        <Icon name="doc" />
        <span>
          {s.reading
            ? t("folder.reading", { n: s.read, left: s.pending })
            : s.read > 0
              ? t("folder.read", { n: s.read })
              : t("folder.none")}
        </span>
        <span className="spacer" />
        {!s.reading && (
          <button
            type="button"
            className="secondary small"
            onClick={() => void client.op("read_folder", { folder: path }).then(() => refresh("folder_documents"))}
          >
            {t("folder.readAgain")}
          </button>
        )}
      </p>
      {s.too_many && <p className="note">{t("folder.tooMany")}</p>}
      {notRead.length > 0 && (
        <details className="section bare" data-testid="not-read">
          <summary>{t("folder.notRead", { n: notRead.length })}</summary>
          <ul className="not-read">
            {notRead.map((n) => (
              <li key={n.path}>
                <code>{n.path}</code> – <span className="muted">{n.reason}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}

/** A chat project: a folder Ancilo only reads. Ask about its documents; its chats. */
export function FolderPage({ path }: { path: string }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const conversations = useOp("list_conversations");
  const [asking, setAsking] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const chats = (conversations.data ?? []).filter((c) => c.folder === path);
  const ask = async (prompt: string) => {
    setError(null);
    setAsking(prompt);
    try {
      const r = await client.op("ask", { prompt, remember: true, kind: "chat", folder: path });
      await refresh("list_conversations");
      if (r.conversation) navigate({ view: "chat", id: r.conversation });
    } catch (e) {
      setError(e);
      throw e;
    } finally {
      setAsking(null);
    }
  };
  return (
    <div className="page" data-testid="folder-page">
      <div className="build stack">
        <h1 className="page-title">
          <Icon name="folder" size={24} /> {nameOf(path)}
        </h1>
        <p className="muted small">
          <code>{path}</code>
        </p>
        <p className="local-note">
          <Icon name="lock" size={13} />
          {t("folder.local")}
        </p>
        <FolderStatus path={path} />
        <Composer label={t("folder.ask")} placeholder={t("folder.askHint")} onSend={ask} busy={asking !== null} autoFocus />
        {asking && <Thinking label={t("assistant.thinking")} />}
        <ErrorNote error={error} onDismiss={() => setError(null)} />
        {chats.length > 0 && (
          <section className="stack">
            <h2>{t("folder.chats")}</h2>
            <ul className="plain-list">
              {chats.map((c) => (
                <li key={c.id}>
                  <button type="button" className="link" onClick={() => navigate({ view: "chat", id: c.id })}>
                    {c.title}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </div>
  );
}

/** Adds a folder as a chat project (read only): chosen in the system
 * dialog, or typed. */
export function AddFolderPage() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const prefs = useOp("get_preferences");
  const [path, setPath] = useState("");
  const [error, setError] = useState<unknown>(null);
  const add = async (dir: string) => {
    setError(null);
    try {
      const before = new Set(prefs.data?.documents ?? []);
      const p = await client.op("set_preferences", { add_documents: dir });
      await refresh("get_preferences");
      // The folder as Ancilo keeps it (its full, resolved path): the new
      // one – or, added before, the same.
      const added = p.documents.find((d) => !before.has(d)) ?? p.documents.find((d) => d === dir.replace(/\/+$/, "")) ?? dir;
      navigate({ view: "folder", path: added }, true);
    } catch (e) {
      setError(e);
    }
  };
  return (
    <div className="page" data-testid="add-folder">
      <div className="build stack">
        <h1 className="page-title">
          <Icon name="folder" size={24} /> {t("addFolder.title")}
        </h1>
        <p>{t("addFolder.intro")}</p>
        <div className="row">
          <button
            type="button"
            onClick={async () => {
              setError(null);
              try {
                const r = await client.op("choose_folder", { prompt: t("addFolder.choose") });
                if (r.path) await add(r.path);
              } catch (e) {
                setError(e);
              }
            }}
          >
            {t("addFolder.choose")}
          </button>
        </div>
        <details className="section bare">
          <summary>{t("build.typePath")}</summary>
          <form
            className="section-body row"
            onSubmit={(e) => {
              e.preventDefault();
              if (path.trim()) void add(path.trim());
            }}
          >
            <label htmlFor="folder-path" className="sr-only">
              {t("addFolder.path")}
            </label>
            <input id="folder-path" type="text" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/Users/me/Documents/Contracts" spellCheck={false} />
            <button type="submit" className="secondary" disabled={!path.trim()}>
              {t("addFolder.add")}
            </button>
          </form>
        </details>
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    </div>
  );
}
