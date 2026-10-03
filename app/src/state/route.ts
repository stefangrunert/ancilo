import { useEffect, useState } from "react";

/** What a new chat is for (the quick actions). */
export type ChatKind = "chat" | "setup" | "write" | "explain" | "summarize";
const KINDS: ChatKind[] = ["chat", "setup", "write", "explain", "summarize"];

/** Where the app is: the start page, a conversation, a project or a coding session. */
export type Route =
  | { view: "home" }
  | { view: "system" }
  | { view: "models" }
  | { view: "build" }
  | { view: "web" }
  | { view: "tasks" }
  | { view: "folder"; path: string }
  | { view: "add-folder" }
  | { view: "task"; id: string }
  | { view: "task-folder"; path: string }

  | { view: "coding-tasks" }
  | { view: "chat"; id: string | null; kind?: ChatKind }
  | { view: "project"; root: string }
  | { view: "session"; id: string };

export function parseRoute(hash: string): Route {
  const h = hash.replace(/^#\/?/, "");
  const [kind, ...rest] = h.split("/");
  const arg = decodeURIComponent(rest.join("/"));
  switch (kind) {
    case "models":
      return { view: "models" };
    case "system":
      return { view: "system" };
    case "build":
      return { view: "build" };
    case "web":
      return { view: "web" };
    case "tasks":
      return { view: "tasks" };
    case "folder":
      return arg ? { view: "folder", path: arg } : { view: "add-folder" };
    case "add-folder":
      return { view: "add-folder" };
    case "task":
      return arg ? { view: "task", id: arg } : { view: "tasks" };
    case "task-folder":
      return arg ? { view: "task-folder", path: arg } : { view: "tasks" };

    case "coding-tasks":
      return { view: "coding-tasks" };
    case "chat": {
      if (rest[0] === "new" || !arg) {
        const kind = KINDS.find((k) => k === rest[1]);
        return kind && kind !== "chat" ? { view: "chat", id: null, kind } : { view: "chat", id: null };
      }
      return { view: "chat", id: arg };
    }
    case "project":
      return arg ? { view: "project", root: arg } : { view: "home" };
    case "session":
      return arg ? { view: "session", id: arg } : { view: "home" };
    default:
      return { view: "home" };
  }
}

export function hrefOf(r: Route): string {
  switch (r.view) {
    case "home":
      return "#/";
    case "models":
      return "#/models";
    case "system":
      return "#/system";
    case "build":
      return "#/build";
    case "web":
      return "#/web";
    case "tasks":
      return "#/tasks";
    case "folder":
      return `#/folder/${encodeURIComponent(r.path)}`;
    case "add-folder":
      return "#/add-folder";
    case "task":
      return `#/task/${encodeURIComponent(r.id)}`;
    case "task-folder":
      return `#/task-folder/${encodeURIComponent(r.path)}`;

    case "coding-tasks":
      return "#/coding-tasks";
    case "chat":
      return r.id ? `#/chat/${encodeURIComponent(r.id)}` : r.kind && r.kind !== "chat" ? `#/chat/new/${r.kind}` : "#/chat/new";
    case "project":
      return `#/project/${encodeURIComponent(r.root)}`;
    case "session":
      return `#/session/${encodeURIComponent(r.id)}`;
  }
}

/** The token arrives in the hash too (`#token=…`); it is not a route. */
function routeHash(): string {
  const h = window.location.hash;
  return h.startsWith("#token=") ? "" : h;
}

export function navigate(r: Route, replace = false) {
  const href = hrefOf(r);
  if (window.location.hash === href) return;
  if (replace) history.replaceState(null, "", href);
  else history.pushState(null, "", href);
  window.dispatchEvent(new HashChangeEvent("hashchange"));
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseRoute(routeHash()));
  useEffect(() => {
    const on = () => setRoute(parseRoute(routeHash()));
    window.addEventListener("hashchange", on);
    window.addEventListener("popstate", on);
    return () => {
      window.removeEventListener("hashchange", on);
      window.removeEventListener("popstate", on);
    };
  }, []);
  return route;
}

export function sameRoute(a: Route, b: Route): boolean {
  return hrefOf(a) === hrefOf(b);
}
