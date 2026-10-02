import { Component, useEffect, useRef, type ErrorInfo, type ReactNode } from "react";
import { ApiError, OfflineError } from "../api/client";
import { useI18n } from "../i18n";

export function formatBytes(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${Math.round(bytes / 1e6)} MB`;
  return `${Math.round(bytes / 1e3)} kB`;
}

export function formatMem(bytes: number): string {
  return `${(bytes / 2 ** 30).toFixed(1)} GB`;
}

/** Working memory as people know it from their computer ("16 GB"). */
export function formatRam(bytes: number): string {
  const gib = bytes / 2 ** 30;
  return gib >= 10 ? `${Math.round(gib)} GB` : `${gib.toFixed(1)} GB`;
}

/** A human message for any error. */
export function errorText(e: unknown): string {
  if (e instanceof ApiError || e instanceof OfflineError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function ErrorNote({ error, onDismiss, action }: { error: unknown; onDismiss?: () => void; action?: ReactNode }) {
  const { t } = useI18n();
  if (!error) return null;
  // Not enough memory: said plainly, with the way out; the details below.
  const memory = error instanceof ApiError && error.code === "insufficient_resources";
  return (
    <div role="alert" className="note error">
      {memory && <strong>{t("error.memory")}</strong>}
      <span className={memory ? "small" : undefined}>{errorText(error)}</span>
      {action}
      {onDismiss && (
        <button type="button" className="link" onClick={onDismiss}>
          {t("error.dismiss")}
        </button>
      )}
    </div>
  );
}

/** A collapsible area – closed until needed. */
export function Section({ title, badge, children, open, testId }: { title: string; badge?: string; children: ReactNode; open?: boolean; testId?: string }) {
  return (
    <details className="section" open={open} data-testid={testId}>
      <summary>
        <span>{title}</span>
        {badge && <span className="badge">{badge}</span>}
      </summary>
      <div className="section-body">{children}</div>
    </details>
  );
}

/** Accessible modal dialog (native <dialog>: focus trap, Escape to close). */
export function Dialog({ open, title, children, onClose }: { open: boolean; title: string; children: ReactNode; onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      if (typeof d.showModal === "function") d.showModal();
      else d.setAttribute("open", "");
    }
    if (!open && d.open) {
      if (typeof d.close === "function") d.close();
      else d.removeAttribute("open");
    }
  }, [open]);
  return (
    <dialog ref={ref} aria-labelledby="dialog-title" onClose={onClose} onCancel={onClose} className="dialog">
      <h2 id="dialog-title">{title}</h2>
      {open && children}
    </dialog>
  );
}

export function StatusDot({ status }: { status: string }) {
  return <span className={`dot dot-${status}`} aria-hidden="true" />;
}

/** Keeps one broken area from taking the whole app down. */
export class Boundary extends Component<{ children: ReactNode; label: string }, { error: unknown }> {
  state = { error: null as unknown };
  static getDerivedStateFromError(error: unknown) {
    return { error };
  }
  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error(error, info.componentStack);
  }
  render() {
    if (this.state.error)
      return (
        <div role="alert" className="note error" data-testid="boundary">
          <strong>{this.props.label}</strong>
          <span>{errorText(this.state.error)}</span>
          <button type="button" className="link" onClick={() => this.setState({ error: null })}>
            ↻
          </button>
        </div>
      );
    return this.props.children;
  }
}
