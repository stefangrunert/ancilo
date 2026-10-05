import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import { Dialog, ErrorNote } from "./ui";

type Outcome = { problems: string[] };
type Step = "ask" | "removing" | { done: Outcome } | { failed: string };

/**
 * Only in the desktop app: removes Ancilo from this Mac – the app, the
 * background service, the connections to Claude Code and Codex and (unless
 * kept) models, conversations, settings and keys. The native shell does it
 * (`remove_ancilo`); the app quits afterwards.
 */
export function RemoveAncilo() {
  const { t, says } = useI18n();
  const [open, setOpen] = useState(false);
  const [keep, setKeep] = useState(false);
  const [step, setStep] = useState<Step>("ask");
  const native = window.__TAURI_INTERNALS__;
  const done = typeof step === "object" && "done" in step ? step.done : null;
  // Nothing left to read: the app closes by itself.
  useEffect(() => {
    if (!done || done.problems.length > 0) return;
    const timer = setTimeout(() => void native?.invoke("quit"), 4000);
    return () => clearTimeout(timer);
  }, [done, native]);
  if (!window.__ANCILO__?.app || !native) return null;
  const remove = async () => {
    setStep("removing");
    try {
      const outcome = (await native.invoke("remove_ancilo", { keepData: keep })) as Outcome;
      setStep({ done: outcome });
    } catch (e) {
      setStep({ failed: String(e) });
    }
  };
  return (
    <section className="panel" aria-labelledby="panel-remove">
      <header>
        <h2 id="panel-remove">{t("remove.title")}</h2>
      </header>
      <p className="hint">{t("remove.hint")}</p>
      <div className="row">
        <button type="button" className="secondary" onClick={() => setOpen(true)} data-testid="remove-ancilo">
          {t("remove.button")}
        </button>
      </div>
      <Dialog
        open={open}
        title={t("remove.confirmTitle")}
        onClose={() => {
          if (step === "ask" || (typeof step === "object" && "failed" in step)) {
            setOpen(false);
            setStep("ask");
          }
        }}
      >
        {done ? (
          <>
            <p role="status">{t(done.problems.length ? "remove.doneLeft" : "remove.done")}</p>
            {done.problems.length > 0 && (
              <ul>
                {done.problems.map((p) => (
                  <li key={p}>{says(p)}</li>
                ))}
              </ul>
            )}
            <div className="row end">
              <button type="button" onClick={() => void native.invoke("quit")}>
                {t("remove.quit")}
              </button>
            </div>
          </>
        ) : (
          <>
            <p>{t("remove.what")}</p>
            <p>{t("remove.stays")}</p>
            <div className="row">
              <label>
                <input type="checkbox" checked={keep} disabled={step === "removing"} onChange={(e) => setKeep(e.target.checked)} />
                {t("remove.keep")}
              </label>
            </div>
            {typeof step === "object" && "failed" in step && (
              <ErrorNote error={t("remove.failed", { why: says(step.failed) })} />
            )}
            <div className="row end">
              <button type="button" className="secondary" disabled={step === "removing"} onClick={() => { setOpen(false); setStep("ask"); }}>
                {t("confirm.no")}
              </button>
              <button type="button" disabled={step === "removing"} onClick={() => void remove()} data-testid="remove-confirm">
                {step === "removing" ? t("remove.removing") : t("remove.confirm")}
              </button>
            </div>
          </>
        )}
      </Dialog>
    </section>
  );
}
