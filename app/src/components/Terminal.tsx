import { FitAddon } from "@xterm/addon-fit";
import { Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { useClient } from "../state/store";

/**
 * A terminal living in the daemon, shown with xterm.js. Connects over a
 * WebSocket with a one-time ticket and reconnects on its own – the terminal
 * itself survives reloads and closed windows.
 */
export function TerminalView({ id }: { id: string }) {
  const { t } = useI18n();
  const client = useClient();
  const host = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<"connecting" | "open" | "closed">("connecting");

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const css = getComputedStyle(document.documentElement);
    const term = new XTerm({
      fontSize: 12,
      fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
      cursorBlink: true,
      scrollback: 5000,
      theme: {
        background: css.getPropertyValue("--term-bg").trim() || "#16171b",
        foreground: css.getPropertyValue("--term-fg").trim() || "#e6e8ee",
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    let ws: WebSocket | null = null;
    let disposed = false;
    let retry: number | undefined;
    let attempts = 0;
    const encoder = new TextEncoder();
    const sendSize = () => {
      if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ resize: [term.cols, term.rows] }));
    };
    const refit = () => {
      try {
        fit.fit();
      } catch {
        /* not visible yet */
      }
      sendSize();
    };
    const connect = async () => {
      setState("connecting");
      try {
        const ticket = await client.op("terminal_ticket", { terminal: id });
        if (disposed) return;
        const base = client.base.replace(/^http/, "ws");
        const socket = new WebSocket(base + ticket.path);
        socket.binaryType = "arraybuffer";
        ws = socket;
        socket.onopen = () => {
          attempts = 0;
          // The daemon sends the scrollback first: start from a clean screen.
          term.reset();
          setState("open");
          refit();
        };
        socket.onmessage = (e) => {
          if (e.data instanceof ArrayBuffer) term.write(new Uint8Array(e.data));
          else term.write(String(e.data));
        };
        socket.onclose = () => {
          if (disposed) return;
          setState("closed");
          schedule();
        };
      } catch {
        if (!disposed) {
          setState("closed");
          schedule();
        }
      }
    };
    const schedule = () => {
      attempts += 1;
      if (attempts > 8) return;
      retry = window.setTimeout(() => void connect(), Math.min(500 * 2 ** attempts, 5000));
    };
    const input = term.onData((d) => {
      if (ws?.readyState === WebSocket.OPEN) ws.send(encoder.encode(d));
    });
    const observer = new ResizeObserver(() => refit());
    observer.observe(el);
    void connect();
    return () => {
      disposed = true;
      window.clearTimeout(retry);
      observer.disconnect();
      input.dispose();
      ws?.close();
      term.dispose();
    };
  }, [client, id]);

  return (
    <div className="terminal" data-testid={`terminal-${id}`} data-state={state}>
      {state !== "open" && (
        <p className="muted terminal-state" role="status">
          {t(state === "connecting" ? "terminal.connecting" : "terminal.disconnected")}
        </p>
      )}
      <div ref={host} className="terminal-host" aria-label={t("terminal.label")} />
    </div>
  );
}
