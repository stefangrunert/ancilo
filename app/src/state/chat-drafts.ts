import { useCallback, useState, useSyncExternalStore } from "react";

// Unsent text belongs to this window. It survives navigation and reloads,
// but is not a cross-chat memory or part of the saved conversation.
const prefix = "ancilo.chat-draft:";
const fallback = new Map<string, string>();
const listeners = new Set<() => void>();
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
};

export function readDraft(key: string): string {
  if (fallback.has(key)) return fallback.get(key)!;
  try { return sessionStorage.getItem(prefix + key) ?? ""; } catch { return ""; }
}

export function writeDraft(key: string, text: string) {
  try {
    if (text) sessionStorage.setItem(prefix + key, text);
    else sessionStorage.removeItem(prefix + key);
    fallback.delete(key);
  } catch { fallback.set(key, text); }
  listeners.forEach((listener) => listener());
}

export function useChatDraft(key: string | undefined, initial: string) {
  const [local, setLocal] = useState(initial);
  const snapshot = useCallback(() => key === undefined ? "" : readDraft(key), [key]);
  const saved = useSyncExternalStore(subscribe, snapshot);
  const set = (update: string | ((text: string) => string)) => {
    if (key === undefined) setLocal(update);
    else writeDraft(key, typeof update === "function" ? update(readDraft(key)) : update);
  };
  return [key === undefined ? local : saved, set] as const;
}
