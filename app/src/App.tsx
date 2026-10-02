import { lazy, Suspense, useEffect, useState } from "react";
import { useI18n } from "./i18n";
import { Boundary } from "./components/ui";
import { useLive, useOp } from "./state/store";
import { hrefOf, useRoute } from "./state/route";
import { ConversationView } from "./components/Conversation";
import { BuildPage } from "./components/Build";
import { SystemPage } from "./components/Home";
import { SetupPage } from "./components/Setup";
import { ModelsPage } from "./components/ModelChoice";
import { Icon } from "./components/Icon";
import { Sidebar } from "./components/Sidebar";
import { WebSearchPage } from "./components/WebSearch";
import { MonitorAlerts, StatusBar } from "./components/SystemMonitor";

// The coding views (with the terminal) load only when opened.
const SessionView = lazy(() => import("./components/Code").then((m) => ({ default: m.SessionView })));
const ProjectView = lazy(() => import("./components/Code").then((m) => ({ default: m.ProjectView })));

const narrow = () => window.innerWidth <= 900;

/** Like a chat app: navigation on the left; the start page, a conversation, a project or a session on the right. */
export function App() {
  const { t } = useI18n();
  const route = useRoute();
  const live = useLive();
  const models = useOp("list_models");
  const list = models.data ?? [];
  const [collapsed, setCollapsed] = useState(narrow);
  // In a narrow window the navigation overlays the page: it closes after a choice.
  useEffect(() => {
    if (narrow()) setCollapsed(true);
  }, [route]);

  let content;
  switch (route.view) {
    case "home":
      content = <SetupPage />;
      break;
    case "system":
      content = (
        <div className="page">
          <SystemPage models={list} loaded={models.data !== undefined} />
        </div>
      );
      break;
    case "models":
      content = <ModelsPage />;
      break;
    case "build":
      content = <BuildPage />;
      break;
    case "web":
      content = <WebSearchPage />;
      break;
    case "chat":
      content = <ConversationView key={route.id ?? `new-${route.kind ?? "chat"}`} id={route.id} kind={route.kind} />;
      break;
    case "project":
      content = <ProjectView key={route.root} root={route.root} models={list} />;
      break;
    case "session":
      content = <SessionView key={route.id} id={route.id} models={list} />;
      break;
  }
  return (
    <div className="app">
      <div className={collapsed ? "shell collapsed" : "shell"}>
        <Sidebar route={route} onHide={() => setCollapsed(true)} />
        <main className="main">
          {collapsed && (
            <div className="topbar">
              <button type="button" className="icon" aria-label={t("nav.show")} title={t("nav.show")} onClick={() => setCollapsed(false)}>
                <Icon name="sidebar" />
              </button>
              <span className="title">{t("app.title")}</span>
            </div>
          )}
          {!live.online && (
            <div role="alert" className="note error offline" data-testid="offline">
              <strong>{t("status.offline")}</strong>
              <span>{t("status.offlineHint")}</span>
            </div>
          )}
          <Boundary key={hrefOf(route)} label={t("error.title")}>
            <Suspense fallback={<p className="muted">…</p>}>{content}</Suspense>
          </Boundary>
        </main>
      </div>
      <StatusBar />
      <MonitorAlerts />
    </div>
  );
}
