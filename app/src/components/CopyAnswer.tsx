import { useEffect, useState } from "react";
import { useI18n } from "../i18n";

export function CopyAnswer({ text }: { text: string }) {
  const { t } = useI18n();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    setState("idle");
  }, [text]);
  useEffect(() => {
    if (state !== "copied") return;
    const timer = window.setTimeout(() => setState("idle"), 2000);
    return () => window.clearTimeout(timer);
  }, [state]);
  return (
    <div className="reply-meta">
      <button type="button" className="ghost" onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setState("copied");
        } catch { setState("failed"); }
      }}>{t("chat.copyAnswer")}</button>
      <span role="status">{state === "copied" ? t("markdown.copied") : state === "failed" ? t("chat.copyFailed") : ""}</span>
    </div>
  );
}
