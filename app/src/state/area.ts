import { useEffect, useState } from "react";
import { hrefOf, navigate, parseRoute, type Route } from "./route";

/** The three areas (decision `2026-10-03-drei-bereiche`): what Ancilo may do
 * there – chat reads, tasks change things (in a copy), code builds. */
export type Area = "chat" | "tasks" | "code";
export const AREAS: Area[] = ["chat", "tasks", "code"];

const AREA_KEY = "ancilo.area";
const LAST_KEY = "ancilo.area.last";

/** The area a page belongs to; `null` for pages of the whole app (setup, system). */
export function areaOf(r: Route): Area | null {
  switch (r.view) {
    case "chat":
    case "folder":
    case "add-folder":
      return "chat";
    case "tasks":
    case "task":
    case "task-folder":
    case "add-task-folder":
      return "tasks";
    case "project":
    case "session":
    case "build":
    case "coding-tasks":
      return "code";
    default:
      return null;
  }
}

/** Where an area opens when nothing of it was open before. */
function start(a: Area): Route {
  switch (a) {
    case "chat":
      return { view: "chat", id: null };
    case "tasks":
      return { view: "tasks" };
    case "code":
      return { view: "build" };
  }
}

// Per-viewer conveniences: storage may be missing (private window, tests).
function read<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}

function write(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // not kept – fine
  }
}

/** The current area: follows the page; the setup and system pages keep it.
 * Each area remembers what was open last. */
export function useArea(route: Route): [Area, (a: Area) => void] {
  const [area, setArea] = useState<Area>(() => areaOf(route) ?? read<Area>(AREA_KEY, "chat"));
  useEffect(() => {
    const a = areaOf(route);
    if (!a) return;
    setArea(a);
    write(AREA_KEY, a);
    write(LAST_KEY, { ...read<Record<string, string>>(LAST_KEY, {}), [a]: hrefOf(route) });
  }, [route]);
  const open = (a: Area) => {
    const last = read<Record<string, string>>(LAST_KEY, {})[a];
    const r = last ? parseRoute(last) : start(a);
    setArea(a);
    write(AREA_KEY, a);
    navigate(areaOf(r) === a ? r : start(a));
  };
  return [area, open];
}
