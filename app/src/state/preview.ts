// Looking at a task's result before keeping it (FPL-03): which view a file
// gets, and the states a preview goes through.
//
// Ported from Jan (janhq/jan b7f4f641efdcacaeca7201506c52900e7539f374,
// web-app/src/lib/coworkPreview.ts – Apache-2.0, Copyright 2025 Menlo
// Research; "This product includes software developed by Menlo Research
// (https://menlo.ai)"; license: third_party/ported/jan/LICENSE). Taken over:
// the preview state machine (`idle → loading → ready | unsupported | failed`,
// reload re-enters `loading`), the rule that a changed file is offered for
// reloading instead of being swapped under the reader, and the reading of a
// file's kind from its name (`extensionOf`, `basenameOf`, `previewKindFor`).
// Changed for Ancilo: the kinds are what Ancilo shows of a task's results
// (tables, Word documents, text – read by the daemon in its sandboxed
// reader, never by the web view), "changed" is a newer version of the
// task's changes, and a ready preview carries the version it shows.

/** How a result is shown. `file` is listed only (opened elsewhere). */
export type PreviewKind = "table" | "document" | "text" | "file";

/**
 * Preview states. `stale` is a flag on `ready`, not an automatic reload:
 * the task changed its results since – swapping what someone is checking
 * under their eyes is worse than offering the newer version.
 */
export type PreviewState<T> =
  | { status: "idle" }
  | { status: "loading"; path: string }
  | { status: "ready"; path: string; kind: Exclude<PreviewKind, "file">; data: T; version: string; stale: boolean }
  | { status: "unsupported"; path: string }
  | { status: "failed"; path: string; reason: unknown };

const EXT_KIND: Record<string, PreviewKind> = {
  xlsx: "table",
  xlsm: "table",
  xls: "table",
  ods: "table",
  csv: "table",
  tsv: "table",
  docx: "document",
  txt: "text",
  md: "text",
  markdown: "text",
};

export function extensionOf(path: string): string {
  const base = path.split(/[/\\]/).pop() ?? "";
  const dot = base.lastIndexOf(".");
  // A leading dot is a dotfile, not an extension (`.gitignore`).
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
}

/** Which view a path maps to. Unknown and other binary types: `file`. */
export function previewKindFor(path: string): PreviewKind {
  return EXT_KIND[extensionOf(path)] ?? "file";
}

export function basenameOf(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

/** The state after a load: unsupported kinds never load. */
export function loaded<T>(path: string, data: T, version: string): PreviewState<T> {
  const kind = previewKindFor(path);
  if (kind === "file") return { status: "unsupported", path };
  return { status: "ready", path, kind, data, version, stale: false };
}

/** A newer version of the results: a ready preview is marked, not replaced. */
export function markStale<T>(state: PreviewState<T>, current: string | null | undefined): PreviewState<T> {
  if (state.status !== "ready" || !current || current === state.version || state.stale) return state;
  return { ...state, stale: true };
}
