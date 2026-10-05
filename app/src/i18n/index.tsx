import { createContext, useContext, useMemo, useState, type ReactNode } from "react";
import { de } from "./de";
import { en, type Key } from "./en";
import { messagesEn } from "./messages.gen";

export type Lang = "de" | "en";
const dictionaries: Record<Lang, Record<Key, string>> = { en, de };

export function detectLang(): Lang {
  try {
    const saved = localStorage.getItem("ancilo.lang");
    if (saved === "de" || saved === "en") return saved;
  } catch {
    /* ignore */
  }
  return navigator.language?.toLowerCase().startsWith("de") ? "de" : "en";
}

export function translate(lang: Lang, key: Key, vars: Record<string, string | number> = {}): string {
  let s: string = dictionaries[lang][key] ?? en[key];
  for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
  return s;
}

/** The daemon's message templates as patterns: a placeholder matches any
 * text; the longest templates are tried first (the most specific). */
const PATTERNS = (Object.entries(messagesEn) as [Key, string][])
  .map(([key, template]) => {
    const names: string[] = [];
    const source = template
      .split(/(\{[a-z_]+\})/)
      .map((part) => {
        const slot = /^\{([a-z_]+)\}$/.exec(part);
        if (slot) {
          names.push(slot[1] ?? "");
          return "([\\s\\S]*?)";
        }
        return part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      })
      .join("");
    return { key, names, re: new RegExp(`^${source}$`) };
  })
  .sort((a, b) => b.re.source.length - a.re.source.length);

/** A message from Ancilo (an error, a failed answer) in the user's language:
 * recognized by its template, its values – which may be such messages
 * themselves – carried over. Anything else stays as it is. */
export function localize(lang: Lang, text: string, depth = 0): string {
  if (lang === "en" || depth > 5) return text;
  for (const p of PATTERNS) {
    const m = p.re.exec(text);
    if (!m) continue;
    const vars: Record<string, string> = {};
    p.names.forEach((name, i) => {
      vars[name] = localize(lang, m[i + 1] ?? "", depth + 1);
    });
    return translate(lang, p.key, vars);
  }
  return text;
}

interface I18n {
  lang: Lang;
  setLang: (l: Lang) => void;
  t: (key: Key, vars?: Record<string, string | number>) => string;
  /** A message from Ancilo in the user's language (see [`localize`]). */
  says: (text: string) => string;
}

const Ctx = createContext<I18n>({ lang: "en", setLang: () => {}, t: (k, v) => translate("en", k, v), says: (s) => s });

export function I18nProvider({ children, initial }: { children: ReactNode; initial?: Lang }) {
  const [lang, setLangState] = useState<Lang>(initial ?? detectLang());
  const value = useMemo<I18n>(
    () => ({
      lang,
      setLang: (l) => {
        setLangState(l);
        try {
          localStorage.setItem("ancilo.lang", l);
        } catch {
          /* ignore */
        }
        document.documentElement.lang = l;
      },
      t: (k, v) => translate(lang, k, v),
      says: (text) => localize(lang, text),
    }),
    [lang],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export const useI18n = () => useContext(Ctx);
export type { Key };
