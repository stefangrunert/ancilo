import { forwardRef, useState, type HTMLAttributes, type ReactNode } from "react";
import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { AREAS, type Area } from "../state/area";
import { navigate, type Route } from "../state/route";
import { usePreferences, usePro } from "../state/prefs";
import { useClient, useOp, useRefresh } from "../state/store";
import { Icon, type IconName } from "./Icon";
import { useReorder } from "./reorder";
import { Dialog, ErrorNote, StatusDot } from "./ui";

type SessionInfo = OpOutput<"list_sessions">[number];
type ChatInfo = OpOutput<"list_conversations">[number];

function NavButton({ icon, label, current, onClick }: { icon: IconName; label: string; current?: boolean; onClick: () => void }) {
  return (
    <button type="button" className="nav-item" aria-current={current ? "page" : undefined} onClick={onClick}>
      <span className="glyph">
        <Icon name={icon} />
      </span>
      <span className="text">{label}</span>
    </button>
  );
}

/** A confirmation before something is deleted. */
function Confirm({ text, onYes, onClose }: { text: string | null; onYes: () => void; onClose: () => void }) {
  const { t } = useI18n();
  return (
    <Dialog open={text !== null} title={t("confirm.title")} onClose={onClose}>
      <p>{text}</p>
      <div className="row end">
        <button type="button" className="secondary" onClick={onClose}>
          {t("confirm.no")}
        </button>
        <button
          type="button"
          onClick={() => {
            onClose();
            onYes();
          }}
        >
          {t("confirm.yes")}
        </button>
      </div>
    </Dialog>
  );
}

type ItemProps = { active: boolean; dragging?: boolean; children: ReactNode; actions?: ReactNode; below?: ReactNode } & Omit<HTMLAttributes<HTMLLIElement>, "children">;

const Item = forwardRef<HTMLLIElement, ItemProps>(function Item({ active, dragging, children, actions, below, ...rest }, ref) {
  return (
    <li ref={ref} className={dragging ? "dragging" : undefined} {...rest}>
      <div className={active ? "nav-row active" : "nav-row"}>
        {children}
        {actions && <span className="item-actions">{actions}</span>}
      </div>
      {below}
    </li>
  );
});

type Renaming = { id: string; title: string };

/** The inline field that replaces an item's name while it is renamed. */
function RenameField({ value, onChange, onDone, onCancel }: { value: string; onChange: (v: string) => void; onDone: () => void; onCancel: () => void }) {
  const { t } = useI18n();
  return (
    <form
      className="rename"
      onSubmit={(e) => {
        e.preventDefault();
        onDone();
      }}
    >
      <input
        type="text"
        aria-label={t("nav.newTitle")}
        value={value}
        autoFocus
        onFocus={(e) => e.target.select()}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && onCancel()}
        onBlur={onCancel}
      />
    </form>
  );
}

function RenameButton({ title, onClick }: { title: string; onClick: () => void }) {
  const { t } = useI18n();
  return (
    <button type="button" className="icon" aria-label={t("nav.rename", { title })} title={t("nav.rename", { title })} onClick={onClick}>
      <Icon name="edit" size={14} />
    </button>
  );
}

/** A project's sessions, sorted by hand. */
function ProjectSessions({
  project,
  sessions,
  route,
  renaming,
  setRenaming,
  act,
  saveOrder,
  onDelete,
}: {
  project: { root: string; name: string };
  sessions: SessionInfo[];
  route: Route;
  renaming: Renaming | null;
  setRenaming: (r: Renaming | null) => void;
  act: (f: () => Promise<unknown>) => Promise<void>;
  saveOrder: (ids: string[]) => Promise<unknown>;
  onDelete: (s: SessionInfo) => void;
}) {
  const { t } = useI18n();
  const client = useClient();
  const byId = new Map(sessions.map((s) => [s.id, s]));
  const sort = useReorder(sessions.map((s) => s.id), saveOrder);
  return (
    <ul className="nav-list nested" aria-label={t("code.sessionsOf", { name: project.name })}>
      {sort.order.map((id) => {
        const s = byId.get(id);
        if (!s) return null;
        const current = route.view === "session" && route.id === s.id;
        return (
          <Item
            key={s.id}
            active={current}
            dragging={sort.dragging === s.id}
            {...sort.item(s.id)}
            actions={
              renaming?.id === s.id ? undefined : (
                <>
                  <RenameButton title={s.title} onClick={() => setRenaming({ id: s.id, title: s.title })} />
                  <button type="button" className="icon" aria-label={t("code.delete", { title: s.title })} title={t("code.delete", { title: s.title })} onClick={() => onDelete(s)}>
                    <Icon name="close" size={14} />
                  </button>
                </>
              )
            }
          >
            {renaming?.id === s.id ? (
              <RenameField
                value={renaming.title}
                onChange={(title) => setRenaming({ id: s.id, title })}
                onCancel={() => setRenaming(null)}
                onDone={() => {
                  const title = renaming.title.trim();
                  setRenaming(null);
                  if (title && title !== s.title) void act(() => client.op("update_session", { session: s.id, title }));
                }}
              />
            ) : (
              <button
                type="button"
                className="nav-item"
                aria-current={current ? "page" : undefined}
                {...sort.keys}
                onKeyDown={sort.onKey(s.id)}
                onClick={() => navigate({ view: "session", id: s.id })}
              >
                <StatusDot status={sessionDot(s)} />
                <span className="text">{s.title}</span>
              </button>
            )}
          </Item>
        );
      })}
    </ul>
  );
}

function sessionDot(s: SessionInfo) {
  return s.status === "running" ? "starting" : s.approvals?.length ? "queued" : s.changes.length ? "running" : "idle";
}

const AREA_ICON: Record<Area, IconName> = { chat: "message", tasks: "computer", code: "code" };

/** The three areas on top of the left column – symbol and name; only the
 * symbol when the column is narrow. */
function AreaTabs({ area, onArea, showCode }: { area: Area; onArea: (a: Area) => void; showCode: boolean }) {
  const { t } = useI18n();
  const shown = AREAS.filter((a) => a !== "code" || showCode);
  const move = (e: React.KeyboardEvent, a: Area) => {
    const i = shown.indexOf(a);
    const next = e.key === "ArrowRight" ? shown[(i + 1) % shown.length] : e.key === "ArrowLeft" ? shown[(i + shown.length - 1) % shown.length] : null;
    if (!next) return;
    e.preventDefault();
    onArea(next);
    document.getElementById(`area-${next}`)?.focus();
  };
  return (
    <div className="area-tabs" role="tablist" aria-label={t("area.label")}>
      {shown.map((a) => (
        <button
          key={a}
          id={`area-${a}`}
          type="button"
          role="tab"
          aria-selected={a === area}
          tabIndex={a === area ? 0 : -1}
          title={t(`area.${a}.hint`)}
          onClick={() => onArea(a)}
          onKeyDown={(e) => move(e, a)}
        >
          <Icon name={AREA_ICON[a]} size={17} />
          <span className="text">{t(`area.${a}`)}</span>
        </button>
      ))}
    </div>
  );
}

/** The left column: the three areas on top, below what belongs to the open one. */
export function Sidebar({ route, area, onArea, showCode }: { route: Route; area: Area; onArea: (a: Area) => void; showCode: boolean }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const pro = usePro();
  const prefs = usePreferences();
  const projects = useOp("list_projects");
  const sessions = useOp("list_sessions", {});
  const conversations = useOp("list_conversations");
  const [open, setOpen] = useState<Record<string, boolean>>({});
  const [renaming, setRenaming] = useState<Renaming | null>(null);
  const [confirm, setConfirm] = useState<{ text: string; run: () => Promise<unknown> } | null>(null);
  const [error, setError] = useState<unknown>(null);

  const all = sessions.data ?? [];
  const currentSession = route.view === "session" ? all.find((s) => s.id === route.id) : undefined;
  const currentProject = route.view === "project" ? route.root : currentSession?.project;
  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
      await refresh("list_projects", "list_sessions", "list_conversations");
    } catch (e) {
      setError(e);
    }
  };
  // Saving an order: the error shows, and the list falls back to the saved order.
  const saveOrder = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(e);
      throw e;
    } finally {
      await refresh("list_projects", "list_sessions");
    }
  };
  // Code projects only – the Tasks area has its own folders.
  const projectList = (projects.data ?? []).filter((p) => p.area === "code");
  const projectsByRoot = new Map(projectList.map((p) => [p.root, p]));
  const sortProjects = useReorder(
    projectList.map((p) => p.root),
    (paths) => saveOrder(() => client.op("reorder_projects", { paths })),
  );

  const renderChat = (c: ChatInfo) => (
    <Item
        key={c.id}
        active={route.view === "chat" && route.id === c.id}
        actions={
          renaming?.id === c.id ? undefined : (
            <>
              <RenameButton title={c.title} onClick={() => setRenaming({ id: c.id, title: c.title })} />
              <button
                type="button"
                className="icon"
                aria-label={t("nav.deleteChat", { title: c.title })}
                title={t("nav.deleteChat", { title: c.title })}
                onClick={() =>
                  setConfirm({
                    text: t("nav.deleteChatConfirm"),
                    run: async () => {
                      await client.op("delete_conversation", { id: c.id });
                      if (route.view === "chat" && route.id === c.id) navigate({ view: "chat", id: null }, true);
                    },
                  })
                }
              >
                <Icon name="trash" size={14} />
              </button>
            </>
          )
        }
      >
        {renaming?.id === c.id ? (
          <RenameField
            value={renaming.title}
            onChange={(title) => setRenaming({ id: c.id, title })}
            onCancel={() => setRenaming(null)}
            onDone={() => {
              const title = renaming.title.trim();
              setRenaming(null);
              if (title && title !== c.title) void act(() => client.op("rename_conversation", { id: c.id, title }));
            }}
          />
        ) : (
          <button type="button" className="nav-item" aria-current={route.view === "chat" && route.id === c.id ? "page" : undefined} onClick={() => navigate({ view: "chat", id: c.id })}>
            <span className="text">{c.title}</span>
          </button>
        )}
      </Item>
  );
  const folders = prefs.data?.documents ?? [];
  // Tasks: free ones (in a folder of their own) and the Tasks area's folders.
  const freeRoots = new Set((projects.data ?? []).filter((p) => p.area === "task").map((p) => p.root));
  const taskFolders = (projects.data ?? []).filter((p) => p.area === "tasks");
  const freeTasks = all.filter((x) => x.kind === "task" && (x.free || freeRoots.has(x.project)));
  const currentTaskFolder = route.view === "task-folder" ? route.path : route.view === "task" ? all.find((x) => x.id === route.id)?.project : undefined;
  const renderTask = (x: SessionInfo) => (
    <Item
      key={x.id}
      active={route.view === "task" && route.id === x.id}
      actions={
        <button
          type="button"
          className="icon"
          aria-label={t("code.delete", { title: x.title })}
          title={t("code.delete", { title: x.title })}
          onClick={() =>
            setConfirm({
              text: t("code.deleteConfirm"),
              run: async () => {
                await client.op("delete_session", { session: x.id }, true);
                if (route.view === "task" && route.id === x.id) navigate({ view: "tasks" }, true);
              },
            })
          }
        >
          <Icon name="close" size={14} />
        </button>
      }
    >
      <button type="button" className="nav-item" aria-current={route.view === "task" && route.id === x.id ? "page" : undefined} onClick={() => navigate({ view: "task", id: x.id })}>
        <StatusDot status={sessionDot(x)} />
        <span className="text">{x.title}</span>
      </button>
    </Item>
  );
  const chatList = conversations.data ?? [];
  const plainChats = chatList.filter((c) => !c.folder || !folders.includes(c.folder));
  const currentFolder = route.view === "folder" ? route.path : route.view === "chat" && route.id ? chatList.find((c) => c.id === route.id)?.folder : undefined;
  return (
    <nav className="sidebar" aria-label={t("nav.label")}>
      <AreaTabs area={area} onArea={onArea} showCode={showCode} />
      <div className="sidebar-scroll" role="tabpanel" aria-labelledby={`area-${area}`}>
        {area === "chat" && (
          <NavButton icon="edit" label={t("nav.newChat")} current={route.view === "chat" && route.id === null} onClick={() => navigate({ view: "chat", id: null })} />
        )}
        {area === "tasks" && <NavButton icon="plus" label={t("nav.newTask")} current={route.view === "tasks"} onClick={() => navigate({ view: "tasks" })} />}
        {area === "code" && pro && (
          <NavButton icon="tool" label="Coding Tasks" current={route.view === "coding-tasks"} onClick={() => navigate({ view: "coding-tasks" })} />
        )}

        {area === "code" && (
        <div className="nav-group">
          <div className="nav-heading">
            <span>{t("nav.projects")}</span>
            <span className="spacer" />
            <button type="button" className="icon" aria-label={t("nav.addProject")} title={t("nav.addProject")} onClick={() => navigate({ view: "build" })}>
              <Icon name="plus" />
            </button>
          </div>
          <ul className="nav-list" aria-label={t("nav.projects")}>
            {sortProjects.order.map((root) => {
              const p = projectsByRoot.get(root);
              if (!p) return null;
              const mine = all.filter((s) => s.project === p.root && s.kind !== "task");
              const expanded = (open[p.root] ?? p.root === currentProject) && sortProjects.dragging !== p.root;
              return (
                <Item
                  key={p.root}
                  active={route.view === "project" && route.root === p.root}
                  dragging={sortProjects.dragging === p.root}
                  {...sortProjects.item(p.root)}
                  actions={
                    renaming?.id === p.root ? undefined : (
                      <>
                        <button
                          type="button"
                          className="icon"
                          aria-label={t("nav.newChatIn", { name: p.name })}
                          title={t("nav.newChatIn", { name: p.name })}
                          onClick={() => navigate({ view: "project", root: p.root })}
                        >
                          <Icon name="plus" size={14} />
                        </button>
                        <RenameButton title={p.name} onClick={() => setRenaming({ id: p.root, title: p.name })} />
                        <button
                          type="button"
                          className="icon"
                          aria-label={t("nav.removeProject", { name: p.name })}
                          title={t("nav.removeProject", { name: p.name })}
                          onClick={() => setConfirm({ text: t("nav.removeProjectConfirm", { name: p.name }), run: () => client.op("remove_project", { path: p.root }, true) })}
                        >
                          <Icon name="close" size={14} />
                        </button>
                      </>
                    )
                  }
                  below={
                    expanded &&
                    mine.length > 0 && (
                      <ProjectSessions
                        project={p}
                        sessions={mine}
                        route={route}
                        renaming={renaming}
                        setRenaming={setRenaming}
                        act={act}
                        saveOrder={(sessions) => saveOrder(() => client.op("reorder_sessions", { sessions }))}
                        onDelete={(s) =>
                          setConfirm({
                            text: t("code.deleteConfirm"),
                            run: async () => {
                              await client.op("delete_session", { session: s.id }, true);
                              if (route.view === "session" && route.id === s.id) navigate({ view: "project", root: p.root }, true);
                            },
                          })
                        }
                      />
                    )
                  }
                >
                  {renaming?.id === p.root ? (
                    <RenameField
                      value={renaming.title}
                      onChange={(title) => setRenaming({ id: p.root, title })}
                      onCancel={() => setRenaming(null)}
                      onDone={() => {
                        const name = renaming.title.trim();
                        setRenaming(null);
                        if (name !== p.name) void act(() => client.op("rename_project", { path: p.root, name }));
                      }}
                    />
                  ) : (
                    <button
                      type="button"
                      className="nav-item"
                      title={p.root}
                      aria-expanded={mine.length > 0 ? expanded : undefined}
                      {...sortProjects.keys}
                      onKeyDown={sortProjects.onKey(p.root)}
                      onClick={() => {
                        setOpen((o) => ({ ...o, [p.root]: route.view === "project" && route.root === p.root ? !expanded : true }));
                        navigate({ view: "project", root: p.root });
                      }}
                    >
                      <span className="glyph">
                        <Icon name="folder" />
                      </span>
                      <span className="text">{p.name}</span>
                    </button>
                  )}
                </Item>
              );
            })}
          </ul>
          {(projects.data ?? []).length === 0 && <p className="nav-empty">{t("nav.noProjects")}</p>}
        </div>
        )}

        {area === "tasks" && (
          <>
            <div className="nav-group">
              <div className="nav-heading">
                <span>{t("tasks.free")}</span>
              </div>
              <ul className="nav-list" aria-label={t("tasks.free")}>
                {freeTasks.map(renderTask)}
              </ul>
              {freeTasks.length === 0 && <p className="nav-empty">{t("tasks.noTasks")}</p>}
            </div>
            <div className="nav-group">
              <div className="nav-heading">
                <span>{t("tasks.projects")}</span>
              </div>
              <ul className="nav-list" aria-label={t("tasks.projects")}>
                {taskFolders.map((f) => {
                  const mine = all.filter((x) => x.project === f.root && x.kind === "task" && !x.free);
                  const expanded = (open[f.root] ?? f.root === currentTaskFolder) && mine.length > 0;
                  return (
                    <Item
                      key={f.root}
                      active={route.view === "task-folder" && route.path === f.root}
                      actions={
                        <>
                          <button type="button" className="icon" aria-label={t("nav.newTaskIn", { name: f.name })} title={t("nav.newTaskIn", { name: f.name })} onClick={() => navigate({ view: "task-folder", path: f.root })}>
                            <Icon name="plus" size={14} />
                          </button>
                          <button
                            type="button"
                            className="icon"
                            aria-label={t("nav.removeProject", { name: f.name })}
                            title={t("nav.removeProject", { name: f.name })}
                            onClick={() => setConfirm({ text: t("nav.removeProjectConfirm", { name: f.name }), run: () => client.op("remove_project", { path: f.root }, true) })}
                          >
                            <Icon name="close" size={14} />
                          </button>
                        </>
                      }
                      below={
                        expanded && (
                          <ul className="nav-list nested" aria-label={t("nav.tasksIn", { name: f.name })}>
                            {mine.map(renderTask)}
                          </ul>
                        )
                      }
                    >
                      <button
                        type="button"
                        className="nav-item"
                        title={f.root}
                        aria-expanded={mine.length > 0 ? expanded : undefined}
                        onClick={() => {
                          setOpen((o) => ({ ...o, [f.root]: route.view === "task-folder" && route.path === f.root ? !expanded : true }));
                          navigate({ view: "task-folder", path: f.root });
                        }}
                      >
                        <span className="glyph">
                          <Icon name="folder" />
                        </span>
                        <span className="text">{f.name}</span>
                      </button>
                    </Item>
                  );
                })}
              </ul>
              {taskFolders.length === 0 && <p className="nav-empty">{t("tasks.noFolders")}</p>}
            </div>
          </>
        )}

        {area === "chat" && (
          <div className="nav-group">
            <div className="nav-heading">
              <span>{t("nav.chatProjects")}</span>
              <span className="spacer" />
              <button type="button" className="icon" aria-label={t("nav.addFolder")} title={t("nav.addFolder")} onClick={() => navigate({ view: "add-folder" })}>
                <Icon name="plus" />
              </button>
            </div>
            <ul className="nav-list" aria-label={t("nav.chatProjects")}>
              {folders.map((f) => {
                const name = f.split("/").filter(Boolean).pop() ?? f;
                const mine = chatList.filter((c) => c.folder === f);
                const expanded = (open[f] ?? f === currentFolder) && mine.length > 0;
                return (
                  <Item
                    key={f}
                    active={route.view === "folder" && route.path === f}
                    actions={
                      <>
                        <button type="button" className="icon" aria-label={t("nav.newChatIn", { name })} title={t("nav.newChatIn", { name })} onClick={() => navigate({ view: "folder", path: f })}>
                          <Icon name="plus" size={14} />
                        </button>
                        <button
                          type="button"
                          className="icon"
                          aria-label={t("nav.removeFolder", { name })}
                          title={t("nav.removeFolder", { name })}
                          onClick={() =>
                            setConfirm({
                              text: t("nav.removeFolderConfirm", { name }),
                              run: async () => {
                                await client.op("set_preferences", { remove_documents: f });
                                await refresh("get_preferences");
                                if (route.view === "folder" && route.path === f) navigate({ view: "chat", id: null }, true);
                              },
                            })
                          }
                        >
                          <Icon name="close" size={14} />
                        </button>
                      </>
                    }
                    below={
                      expanded && (
                        <ul className="nav-list nested" aria-label={t("nav.chatsIn", { name })}>
                          {mine.map(renderChat)}
                        </ul>
                      )
                    }
                  >
                    <button
                      type="button"
                      className="nav-item"
                      title={f}
                      aria-expanded={mine.length > 0 ? expanded : undefined}
                      onClick={() => {
                        setOpen((o) => ({ ...o, [f]: route.view === "folder" && route.path === f ? !expanded : true }));
                        navigate({ view: "folder", path: f });
                      }}
                    >
                      <span className="glyph">
                        <Icon name="folder" />
                      </span>
                      <span className="text">{name}</span>
                    </button>
                  </Item>
                );
              })}
            </ul>
            {folders.length === 0 && <p className="nav-empty">{t("nav.noFolders")}</p>}
          </div>
        )}

        {area === "chat" && (
        <div className="nav-group">
          <div className="nav-heading">
            <span>{t("nav.chats")}</span>
          </div>
          <ul className="nav-list" aria-label={t("nav.chats")}>
            {plainChats.map(renderChat)}
          </ul>
          {plainChats.length === 0 && <p className="nav-empty">{t("nav.noChats")}</p>}
        </div>
        )}
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
      <Confirm text={confirm?.text ?? null} onClose={() => setConfirm(null)} onYes={() => confirm && void act(confirm.run)} />
    </nav>
  );
}
