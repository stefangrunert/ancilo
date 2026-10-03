import { lazy, Suspense, useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { useI18n } from "./i18n";
import { Boundary } from "./components/ui";
import { useLive, useOp } from "./state/store";
import { usePreferences, usePro } from "./state/prefs";
import { useArea } from "./state/area";
import { hrefOf, navigate, useRoute, type Route } from "./state/route";
import { CodingTasksPage } from "./components/Tasks";
import { TasksAreaPage } from "./components/TasksArea";
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

/** The left column's width: draggable, remembered, reset by a double click. */
const WIDTH = { min: 200, max: 420, start: 264 };
const WIDTH_KEY = "ancilo.sidebar.width";

function savedWidth(): number {
  try {
    const w = Number(localStorage.getItem(WIDTH_KEY));
    return w >= WIDTH.min && w <= WIDTH.max ? w : WIDTH.start;
  } catch {
    return WIDTH.start;
  }
}

function useSidebarWidth(): [number, (w: number) => void] {
  const [width, setWidth] = useState(savedWidth);
  const set = (w: number) => {
    const clamped = Math.round(Math.min(WIDTH.max, Math.max(WIDTH.min, w)));
    setWidth(clamped);
    try {
      localStorage.setItem(WIDTH_KEY, String(clamped));
    } catch {
      // not kept – fine
    }
  };
  return [width, set];
}

/** The handle between the left column and the page. */
function Resizer({ width, onChange }: { width: number; onChange: (w: number) => void }) {
  const { t } = useI18n();
  const drag = useRef<{ x: number; w: number } | null>(null);
  const down = (e: PointerEvent<HTMLDivElement>) => {
    drag.current = { x: e.clientX, w: width };
    // Keeps following the pointer outside the handle (not in every test DOM).
    e.currentTarget.setPointerCapture?.(e.pointerId);
  };
  const move = (e: PointerEvent<HTMLDivElement>) => {
    if (drag.current) onChange(drag.current.w + e.clientX - drag.current.x);
  };
  const key = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 40 : 10;
    if (e.key === "ArrowLeft") onChange(width - step);
    else if (e.key === "ArrowRight") onChange(width + step);
    else if (e.key === "Home") onChange(WIDTH.min);
    else if (e.key === "End") onChange(WIDTH.max);
    else return;
    e.preventDefault();
  };
  return (
    <div
      className="resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label={t("nav.resize")}
      aria-valuemin={WIDTH.min}
      aria-valuemax={WIDTH.max}
      aria-valuenow={width}
      tabIndex={0}
      title={t("nav.resizeHint")}
      onPointerDown={down}
      onPointerMove={move}
      onPointerUp={() => (drag.current = null)}
      onDoubleClick={() => onChange(WIDTH.start)}
      onKeyDown={key}
    />
  );
}

/** The header – in the app it is the window's title bar (the window buttons
 * sit on its left): the sidebar switch, setup and system. */
function AppHeader({ route, collapsed, onToggle }: { route: Route; collapsed: boolean; onToggle: () => void }) {
  const { t } = useI18n();
  return (
    <header className={window.__ANCILO__?.app ? "app-header in-app" : "app-header"} data-tauri-drag-region>
      <button type="button" className="icon" aria-label={t(collapsed ? "nav.show" : "nav.hide")} title={t(collapsed ? "nav.show" : "nav.hide")} onClick={onToggle}>
        <Icon name="sidebar" />
      </button>
      <button type="button" className="header-link" aria-current={route.view === "home" ? "page" : undefined} onClick={() => navigate({ view: "home" })}>
        <Icon name="gear" />
        <span>{t("setup.title")}</span>
      </button>
      <button type="button" className="header-link" aria-current={route.view === "system" ? "page" : undefined} onClick={() => navigate({ view: "system" })}>
        <Icon name="pulse" />
        <span>{t("nav.system")}</span>
      </button>
      <span className="spacer" data-tauri-drag-region />
    </header>
  );
}

/** Like a chat app: navigation on the left; the start page, a conversation, a project or a session on the right. */
export function App() {
  const { t } = useI18n();
  const route = useRoute();
  const live = useLive();
  const models = useOp("list_models");
  const list = models.data ?? [];
  const [collapsed, setCollapsed] = useState(narrow);
  const [width, setWidth] = useSidebarWidth();
  const [area, openArea] = useArea(route);
  const pro = usePro();
  const prefs = usePreferences();
  const projects = useOp("list_projects");
  // The Code area is for those who program: chosen in the setup, used
  // already, or the expert view.
  const showCode = pro || (prefs.data?.purposes ?? []).includes("code") || (projects.data ?? []).length > 0 || area === "code";
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
    case "tasks":
      content = <TasksAreaPage />;
      break;
    case "coding-tasks":
      content = <CodingTasksPage />;
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
      <AppHeader route={route} collapsed={collapsed} onToggle={() => setCollapsed((c) => !c)} />
      <div className={collapsed ? "shell collapsed" : "shell"} style={{ ["--sidebar-w" as string]: `${width}px` }}>
        <Sidebar route={route} area={area} onArea={openArea} showCode={showCode} />
        {!collapsed && <Resizer width={width} onChange={setWidth} />}
        <main className="main">
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
