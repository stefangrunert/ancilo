// The app talks to the daemon only through its HTTP API: operations
// (`POST /api/v1/ops/<name>`) and events (SSE) – the same surface as CLI, MCP
// and the assistant.
import type { components, paths } from "./schema";

/** A document read for a chat (its text stays on this computer). */
export type AttachmentView = components["schemas"]["AttachmentView"];

type OpPath = Extract<keyof paths, `/api/v1/ops/${string}`>;
export type OpName = OpPath extends `/api/v1/ops/${infer N}` ? N : never;
type Post<N extends OpName> = paths[`/api/v1/ops/${N}`] extends { post: infer P } ? P : never;
export type OpInput<N extends OpName> = Post<N> extends {
  requestBody?: { content: { "application/json": infer I } };
}
  ? I
  : Record<string, never>;
export type OpOutput<N extends OpName> = Post<N> extends {
  responses: { 200: { content: { "application/json": infer O } } };
}
  ? O
  : unknown;

export interface AncEvent {
  seq: number;
  ts: string;
  kind: string;
  subject?: string | null;
  data: Record<string, unknown>;
}

export class ApiError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

/** The daemon is not reachable (not running, or the network is gone). */
export class OfflineError extends Error {}

declare global {
  interface Window {
    /** Set by the native shell (Tauri) before the page loads. */
    __ANCILO__?: { token?: string; url?: string; app?: boolean };
    /** The native shell's commands (Tauri IPC) – only in the desktop app. */
    __TAURI_INTERNALS__?: { invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown> };
  }
}

const TOKEN_KEY = "ancilo.token";

/** Token from the native shell, the URL fragment (`#token=…`) or this session. */
export function readToken(): string | null {
  const injected = window.__ANCILO__?.token;
  if (injected) return injected;
  const hash = new URLSearchParams(window.location.hash.slice(1));
  const fromHash = hash.get("token");
  if (fromHash) {
    try {
      sessionStorage.setItem(TOKEN_KEY, fromHash);
    } catch {
      /* storage may be unavailable */
    }
    hash.delete("token");
    const rest = hash.toString();
    history.replaceState(null, "", window.location.pathname + window.location.search + (rest ? `#${rest}` : ""));
    return fromHash;
  }
  try {
    return sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export class Client {
  constructor(
    readonly base: string,
    readonly token: string,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {}

  /** Calls an operation. `confirm` for consequential operations – only after an explicit user action. */
  async op<N extends OpName>(name: N, input?: OpInput<N>, confirm = false): Promise<OpOutput<N>> {
    let res: Response;
    try {
      res = await this.fetchImpl(`${this.base}/api/v1/ops/${name}`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${this.token}`,
          ...(confirm ? { "x-ancilo-confirm": "true" } : {}),
        },
        body: JSON.stringify(input ?? {}),
      });
    } catch {
      throw new OfflineError("Ancilo is not reachable");
    }
    const body = await res.json().catch(() => null);
    if (!res.ok) {
      const err = body?.error ?? {};
      throw new ApiError(err.code ?? "internal", err.message ?? `HTTP ${res.status}`, res.status);
    }
    return body as OpOutput<N>;
  }

  /** Sends a document to read (PDF, Word, Excel, CSV, text): only its text is kept. */
  async attach(name: string, file: Blob): Promise<AttachmentView> {
    let res: Response;
    try {
      res = await this.fetchImpl(`${this.base}/api/v1/attachments?name=${encodeURIComponent(name)}`, {
        method: "POST",
        headers: { authorization: `Bearer ${this.token}`, "content-type": "application/octet-stream" },
        body: file,
      });
    } catch {
      throw new OfflineError("Ancilo is not reachable");
    }
    const body = await res.json().catch(() => null);
    if (!res.ok) {
      const err = body?.error ?? {};
      throw new ApiError(err.code ?? "internal", err.message ?? `HTTP ${res.status}`, res.status);
    }
    return body as AttachmentView;
  }

  /** Puts a file into a task's copy; returns the name it got there. */
  async addTaskFile(session: string, name: string, file: Blob): Promise<{ name: string }> {
    let res: Response;
    try {
      res = await this.fetchImpl(`${this.base}/api/v1/sessions/${encodeURIComponent(session)}/files?name=${encodeURIComponent(name)}`, {
        method: "POST",
        headers: { authorization: `Bearer ${this.token}`, "content-type": "application/octet-stream" },
        body: file,
      });
    } catch {
      throw new OfflineError("Ancilo is not reachable");
    }
    const body = await res.json().catch(() => null);
    if (!res.ok) {
      const err = body?.error ?? {};
      throw new ApiError(err.code ?? "internal", err.message ?? `HTTP ${res.status}`, res.status);
    }
    return body as { name: string };
  }

  async health(): Promise<boolean> {
    try {
      const r = await this.fetchImpl(`${this.base}/api/v1/health`);
      return r.ok;
    } catch {
      return false;
    }
  }

  /**
   * Follows the event stream. Reconnects with `after=<last seq>` so nothing
   * is lost; `onState` reports whether the stream is up.
   */
  events(onEvent: (e: AncEvent) => void, onState: (up: boolean) => void): () => void {
    let stopped = false;
    let last = 0;
    let controller: AbortController | null = null;
    const run = async () => {
      let delay = 500;
      while (!stopped) {
        controller = new AbortController();
        try {
          const url = `${this.base}/api/v1/events${last ? `?after=${last}` : ""}`;
          const res = await this.fetchImpl(url, {
            headers: { authorization: `Bearer ${this.token}`, accept: "text/event-stream" },
            signal: controller.signal,
          });
          if (!res.ok || !res.body) throw new Error(`HTTP ${res.status}`);
          onState(true);
          delay = 500;
          const reader = res.body.getReader();
          const decoder = new TextDecoder();
          let buffer = "";
          for (;;) {
            const { value, done } = await reader.read();
            if (done) break;
            buffer += decoder.decode(value, { stream: true });
            let cut: number;
            while ((cut = buffer.indexOf("\n\n")) >= 0) {
              const block = buffer.slice(0, cut);
              buffer = buffer.slice(cut + 2);
              const data = block
                .split("\n")
                .filter((l) => l.startsWith("data:"))
                .map((l) => l.slice(5).trimStart())
                .join("\n");
              if (!data) continue;
              try {
                const e = JSON.parse(data) as AncEvent;
                if (typeof e.seq === "number") last = Math.max(last, e.seq);
                onEvent(e);
              } catch {
                /* keep-alive or partial */
              }
            }
          }
        } catch {
          /* reconnect below */
        }
        if (stopped) break;
        onState(false);
        await new Promise((r) => setTimeout(r, delay));
        delay = Math.min(delay * 2, 5000);
      }
    };
    void run();
    return () => {
      stopped = true;
      controller?.abort();
    };
  }
}
