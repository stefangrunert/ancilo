import { QueryClient, useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { Client, type AncEvent, type OpInput, type OpName, type OpOutput } from "../api/client";

const ClientCtx = createContext<Client | null>(null);

export function useClient(): Client {
  const c = useContext(ClientCtx);
  if (!c) throw new Error("no client");
  return c;
}

/** Which queries an event makes stale. */
export function queriesFor(kind: string): string[][] {
  const [area] = kind.split(".");
  switch (area) {
    case "model":
    case "instance":
    case "download":
    case "role":
      return [["list_models"], ["hardware_info"], ["recommendations"], ["resource_status"], ["system_health"]];
    case "preferences":
      return [["get_preferences"]];
    case "web":
      return [["get_web_search"]];
    case "resources":
      return [["resource_status"], ["recommend_models"], ["system_health"]];
    case "system":
      return [["system_health"], ["resource_status"]];
    case "task":
      return [["list_tasks"]];
    case "compare":
      return [["list_comparisons"], ["comparison_report"], ["comparison_status"], ["recommendations"]];
    case "ab":
      return [["ab_status"]];
    case "route":
      return [["list_routes"]];
    case "recommendation":
      return [["recommendations"]];
    case "settings":
      return [["get_permissions"]];
    case "session":
      return [["get_session"], ["list_sessions"], ["session_diff"], ["list_projects"]];
    case "project":
      return [["list_projects"], ["list_sessions"]];
    case "conversation":
      return [["list_conversations"], ["get_conversation"]];
    case "agent":
      // Coding sessions show changes while the agent works.
      return kind === "agent.file_changed" ? [["get_session"], ["session_diff"]] : [];
    case "terminal":
      return [["list_terminals"]];
    case "library":
      return [["folder_documents"]];
    case "outbound":
      return [["outbound_summary"], ["outbound_log"]];
    default:
      return [];
  }
}

export interface Live {
  online: boolean;
  /** Download progress per model id: 0–1. */
  downloads: Record<string, number>;
  /** Recent activity lines per subject (task id, comparison id). */
  activity: Record<string, string[]>;
  /** The current turn per subject: what the agent said on the way, and how
   * many steps it took so far – shown while it works. */
  turns: Record<string, LiveTurn>;
  /** Models being loaded right now (a request waits for them). */
  loading: Record<string, true>;
  /** What a chat does right now: searching the web, writing the answer. */
  phase: Record<string, ChatPhase>;
}

export type ChatPhase = { step: "web"; query: string } | { step: "answer" };

export interface LiveTurn {
  notes: string[];
  steps: number;
}

const LiveCtx = createContext<Live>({ online: true, downloads: {}, activity: {}, turns: {}, loading: {}, phase: {} });
export const useLive = () => useContext(LiveCtx);

function activityLine(e: AncEvent): string | null {
  const d = e.data as Record<string, unknown>;
  switch (e.kind) {
    case "agent.tool_called":
      return `${String(d.name)} ${JSON.stringify(d.arguments ?? {})}`.slice(0, 160);
    case "agent.file_changed":
      return `${String(d.kind)} ${String(d.path)}`;
    case "agent.message":
      return String(d.text ?? "").slice(0, 160);
    case "assistant.web_search":
      return `web: ${String(d.query ?? "")}`.slice(0, 160);
    case "task.finished":
    case "task.failed":
    case "task.cancelled":
      return e.kind.replace("task.", "");
    default:
      return null;
  }
}

/** Events that start a new turn: what was shown of the previous one goes. */
const TURN_STARTS = ["assistant.thinking", "session.turn_started"];

/** Models loading and chat phases after an event (`null`: nothing changed). */
export function nextProgress(l: Pick<Live, "loading" | "phase">, e: AncEvent): Pick<Live, "loading" | "phase"> | null {
  const s = e.subject;
  if (!s) return null;
  if (e.kind === "instance.starting") return { ...l, loading: { ...l.loading, [s]: true } };
  if (["instance.ready", "instance.failed", "instance.stopped"].includes(e.kind) && l.loading[s]) {
    const loading = { ...l.loading };
    delete loading[s];
    return { ...l, loading };
  }
  if (e.kind === "assistant.web_search") {
    const query = String((e.data as { query?: unknown }).query ?? "");
    return { ...l, phase: { ...l.phase, [s]: { step: "web", query } } };
  }
  if (e.kind === "assistant.answering") return { ...l, phase: { ...l.phase, [s]: { step: "answer" } } };
  if ((e.kind === "assistant.answer" || TURN_STARTS.includes(e.kind)) && l.phase[s]) {
    const phase = { ...l.phase };
    delete phase[s];
    return { ...l, phase };
  }
  return null;
}

/** The current turn per subject after an event (`null`: the event is not about it). */
export function nextTurn(turns: Record<string, LiveTurn>, e: AncEvent): Record<string, LiveTurn> | null {
  if (!e.subject) return null;
  if (TURN_STARTS.includes(e.kind)) return { ...turns, [e.subject]: { notes: [], steps: 0 } };
  const t = turns[e.subject] ?? { notes: [], steps: 0 };
  if (e.kind === "agent.note") {
    const text = String((e.data as { text?: unknown }).text ?? "").trim();
    return text ? { ...turns, [e.subject]: { ...t, notes: [...t.notes, text].slice(-30) } } : null;
  }
  if (e.kind === "agent.tool_called") return { ...turns, [e.subject]: { ...t, steps: t.steps + 1 } };
  return null;
}

/** The activity lines per subject after an event (only the current turn's, the last 50). */
export function nextActivity(activity: Record<string, string[]>, e: AncEvent): Record<string, string[]> {
  if (!e.subject) return activity;
  if (TURN_STARTS.includes(e.kind)) return { ...activity, [e.subject]: [] };
  const line = activityLine(e);
  if (!line) return activity;
  return { ...activity, [e.subject]: [...(activity[e.subject] ?? []), line].slice(-50) };
}

export function ClientProvider({ client, queryClient, children }: { client: Client; queryClient: QueryClient; children: ReactNode }) {
  const [live, setLive] = useState<Live>({ online: true, downloads: {}, activity: {}, turns: {}, loading: {}, phase: {} });
  const qc = queryClient;
  const pending = useRef(new Set<string>());
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    const flush = () => {
      for (const key of pending.current) void qc.invalidateQueries({ queryKey: JSON.parse(key) as string[] });
      pending.current.clear();
      timer.current = undefined;
    };
    const stop = client.events(
      (e) => {
        for (const q of queriesFor(e.kind)) pending.current.add(JSON.stringify(q));
        if (pending.current.size && timer.current === undefined) timer.current = window.setTimeout(flush, 100);
        if (e.kind === "download.progress" && e.subject) {
          const d = e.data as { bytes?: number; total?: number };
          const pct = d.total ? Math.min(1, (d.bytes ?? 0) / d.total) : 0;
          setLive((l) => ({ ...l, downloads: { ...l.downloads, [e.subject as string]: pct } }));
        }
        if ((e.kind === "download.completed" || e.kind === "download.failed") && e.subject) {
          setLive((l) => {
            const downloads = { ...l.downloads };
            delete downloads[e.subject as string];
            return { ...l, downloads };
          });
        }
        if (e.subject && (TURN_STARTS.includes(e.kind) || activityLine(e))) {
          setLive((l) => ({ ...l, activity: nextActivity(l.activity, e) }));
        }
        if (e.subject && (e.kind.startsWith("instance.") || TURN_STARTS.includes(e.kind) || e.kind.startsWith("assistant."))) {
          setLive((l) => {
            const next = nextProgress(l, e);
            return next ? { ...l, ...next } : l;
          });
        }
        if (e.subject && (TURN_STARTS.includes(e.kind) || e.kind === "agent.note" || e.kind === "agent.tool_called")) {
          setLive((l) => {
            const turns = nextTurn(l.turns, e);
            return turns ? { ...l, turns } : l;
          });
        }
      },
      (up) => {
        setLive((l) => (l.online === up ? l : { ...l, online: up }));
        if (up) void qc.invalidateQueries();
      },
    );
    return () => {
      stop();
      if (timer.current) window.clearTimeout(timer.current);
    };
  }, [client, qc]);
  return (
    <ClientCtx.Provider value={client}>
      <LiveCtx.Provider value={live}>{children}</LiveCtx.Provider>
    </ClientCtx.Provider>
  );
}

/** An operation as a query (re-fetched when events make it stale). */
export function useOp<N extends OpName>(name: N, input?: OpInput<N>, enabled = true) {
  const client = useClient();
  return useQuery<OpOutput<N>>({
    queryKey: input ? [name, input] : [name],
    queryFn: () => client.op(name, input),
    enabled,
    retry: false,
  });
}

export function useRefresh() {
  const qc = useQueryClient();
  return (...names: OpName[]) => Promise.all(names.map((n) => qc.invalidateQueries({ queryKey: [n] })));
}
