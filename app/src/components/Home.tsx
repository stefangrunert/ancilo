import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { useOp } from "../state/store";
import { usePro } from "../state/prefs";
import { navigate } from "../state/route";
import { AddModel } from "./AddModel";
import { Cockpit } from "./Cockpit";
import { CompareSection } from "./Compare";
import { ModelsSection, PrimaryModels } from "./Models";
import { Connect, Permissions, Updates } from "./Settings";
import { TasksSection } from "./Tasks";
import { StatusDot } from "./ui";

type Model = OpOutput<"list_models">[number];

/** Web search: a line like a model's – on or off, where, when – and the way to change it. */
function WebSearchPanel() {
  const { t } = useI18n();
  const web = useOp("get_web_search");
  const p = web.data?.provider ?? "off";
  return (
    <section className="panel" aria-labelledby="panel-web">
      <header>
        <h2 id="panel-web">{t("web.title")}</h2>
      </header>
      <div className="model-line" data-testid="web-status">
        <div className="row">
          <StatusDot status={p === "off" ? "idle" : "running"} />
          <strong>{t(`web.provider.${p}` as Key)}</strong>
          <span className="muted">{p === "off" ? t("web.short.off") : t(`web.short.${web.data?.mode ?? "ask"}` as Key)}</span>
          <span className="spacer" />
          <button type="button" className={p === "off" ? undefined : "secondary"} onClick={() => navigate({ view: "web" })}>
            {p === "off" ? t("web.setUp") : t("web.change")}
          </button>
        </div>
      </div>
    </section>
  );
}

/**
 * The system page: the AI, connections and how much Ancilo takes – in the
 * simple view only what everyone needs; the expert view adds roles,
 * comparisons, tasks, permissions and the expert ways to add models.
 */
export function SystemPage({ models, loaded = true }: { models: Model[]; loaded?: boolean }) {
  const { t } = useI18n();
  const pro = usePro();
  const chat = models.filter((m) => !m.embedding);
  if (!loaded) return null;
  return (
    <div className="home">
      <h1>{t("nav.system")}</h1>
      <div className="grid">
        <section className="panel" aria-labelledby="panel-models">
          <header>
            <h2 id="panel-models">{t("home.models")}</h2>
          </header>
          {chat.length === 0 ? (
            <div className="empty">
              <p>{t("system.noModel")}</p>
              <button type="button" onClick={() => navigate({ view: "home" })}>
                {t("setup.title")}
              </button>
            </div>
          ) : (
            <PrimaryModels models={models} />
          )}
          <div className="row">
            <button type="button" className="secondary" onClick={() => navigate({ view: "models" })}>
              {t("choose.another")}
            </button>
          </div>
          {pro && (
            <details className="section bare">
              <summary>{t("choose.advanced")}</summary>
              <div className="section-body">
                <AddModel />
              </div>
            </details>
          )}
        </section>
        <section className="panel" aria-labelledby="panel-connect">
          <header>
            <h2 id="panel-connect">{t("home.connect")}</h2>
          </header>
          <p className="hint">{t("home.connectHint")}</p>
          <Connect />
          {pro && <Permissions />}
          <Updates />
        </section>
        <WebSearchPanel />
        <Cockpit />
        {pro && chat.length > 1 && (
          <div className="wide">
            <ModelsSection models={models} />
          </div>
        )}
        {pro && (
          <div className="wide">
            <TasksSection />
          </div>
        )}
        {pro && chat.length > 1 && (
          <div className="wide">
            <CompareSection models={models} />
          </div>
        )}
      </div>
    </div>
  );
}
