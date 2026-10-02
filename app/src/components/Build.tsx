import { useState, type FormEvent } from "react";
import { useI18n } from "../i18n";
import { navigate } from "../state/route";
import { useClient, useRefresh } from "../state/store";
import { BackToChat } from "./QuickActions";
import { ErrorNote } from "./ui";

/**
 * "Build something": a new project needs only a name – Ancilo creates the
 * folder. An existing folder can be opened too.
 */
export function BuildPage() {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [error, setError] = useState<unknown>(null);
  const opened = async (root: string) => {
    await refresh("list_projects");
    navigate({ view: "project", root });
  };
  const create = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      const p = await client.op("create_project", { name });
      await opened(p.root);
    } catch (err) {
      setError(err);
    }
  };
  const open = async (p: string, e?: FormEvent) => {
    e?.preventDefault();
    setError(null);
    try {
      const r = await client.op("open_project", { path: p.trim() });
      await opened(r.root);
    } catch (err) {
      setError(err);
    }
  };
  return (
    <div className="page">
      <div className="build stack">
        <div>
          <BackToChat />
        </div>
        <h1>{t("build.title")}</h1>
        <p className="muted">{t("build.what")}</p>
        <form className="card stack" onSubmit={(e) => void create(e)}>
          <label htmlFor="project-name">
            <strong>{t("build.newLabel")}</strong>
          </label>
          <div className="row">
            <input id="project-name" type="text" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("build.namePlaceholder")} autoFocus />
            <button type="submit" disabled={!name.trim()}>
              {t("build.create")}
            </button>
          </div>
          <p className="muted small">{t("build.where")}</p>
        </form>
        <form className="card stack" onSubmit={(e) => void open(path, e)}>
          <strong>{t("build.openLabel")}</strong>
          <div className="row">
            <button
              type="button"
              className="secondary"
              onClick={async () => {
                setError(null);
                try {
                  const r = await client.op("choose_folder", { prompt: t("code.chooseTitle") });
                  if (r.path) await open(r.path);
                } catch (err) {
                  setError(err);
                }
              }}
            >
              {t("code.choose")}
            </button>
          </div>
          <details className="section bare">
            <summary>{t("build.typePath")}</summary>
            <div className="section-body row">
              <label htmlFor="project-path" className="sr-only">
                {t("code.project")}
              </label>
              <input id="project-path" type="text" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/Users/me/project" spellCheck={false} />
              <button type="submit" className="secondary" disabled={!path.trim()}>
                {t("code.open")}
              </button>
            </div>
          </details>
        </form>
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    </div>
  );
}
