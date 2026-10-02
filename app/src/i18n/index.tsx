import { createContext, useContext, useMemo, useState, type ReactNode } from "react";
import { de } from "./de";
import { en, type Key } from "./en";

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

interface I18n {
  lang: Lang;
  setLang: (l: Lang) => void;
  t: (key: Key, vars?: Record<string, string | number>) => string;
}

const Ctx = createContext<I18n>({ lang: "en", setLang: () => {}, t: (k, v) => translate("en", k, v) });

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
    }),
    [lang],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export const useI18n = () => useContext(Ctx);
export type { Key };
