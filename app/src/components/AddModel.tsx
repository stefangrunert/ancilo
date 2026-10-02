import { useEffect, useState, type FormEvent } from "react";
import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { useClient, useRefresh } from "../state/store";
import { ErrorNote, formatBytes } from "./ui";

type Plan = OpOutput<"plan_model">;
type Ctx = "small" | "medium" | "large";

const isServer = (a: string) => /^https?:\/\//i.test(a.trim());

export function AddModel({ onAdded }: { onAdded?: () => void } = {}) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const [address, setAddress] = useState("");
  const [context, setContext] = useState<Ctx>("medium");
  const [plan, setPlan] = useState<Plan | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [adding, setAdding] = useState(false);

  // Live detection: plan while typing (debounced).
  useEffect(() => {
    setPlan(null);
    setError(null);
    const a = address.trim();
    if (a.length < 4 || isServer(a)) return;
    setChecking(true);
    const id = window.setTimeout(() => {
      client
        .op("plan_model", { address: a, context })
        .then((p) => setPlan(p))
        .catch((e) => setError(e))
        .finally(() => setChecking(false));
    }, 400);
    return () => {
      window.clearTimeout(id);
      setChecking(false);
    };
  }, [address, context, client]);

  async function add(e: FormEvent) {
    e.preventDefault();
    setAdding(true);
    setError(null);
    try {
      // The click is the confirmation: size and fit are shown above.
      await client.op("add_model", { address: address.trim(), context, start: true }, true);
      setAddress("");
      setPlan(null);
      await refresh("list_models", "hardware_info");
      onAdded?.();
    } catch (err) {
      setError(err);
    } finally {
      setAdding(false);
    }
  }

  const fit = plan?.plan.fit;
  const source = plan?.repo ? `Hugging Face · ${plan.repo.id}` : (plan?.plan.files[0]?.path ?? "");
  const blocked = fit === "does_not_fit";
  return (
    <form onSubmit={add} className="add-model">
      <div className="row">
        <label htmlFor="model-address" className="label">
          {t("model.label")}
        </label>
        <input
          id="model-address"
          type="text"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          placeholder={t("model.placeholder")}
          aria-describedby="model-detected"
          spellCheck={false}
          autoCapitalize="off"
        />
        <button type="submit" disabled={adding || !address.trim() || blocked || (!plan && !isServer(address))}>
          {t("model.add")}
        </button>
      </div>
      <p id="model-detected" className={`detected ${blocked ? "bad" : ""}`} aria-live="polite">
        {checking && t("model.detecting")}
        {plan && (
          <>
            ↳{" "}
            {t("model.detected", {
              source: source || plan.plan.quant || "",
              size: formatBytes(plan.plan.size_bytes),
              fit: t(`fit.${plan.plan.fit}` as "fit.fits"),
            })}
            {plan.existing && <> · {t("model.existing")}</>}
            {blocked && <span className="reason"> – {plan.plan.reason}</span>}
          </>
        )}
      </p>
      <fieldset className="row radios">
        <legend className="label">{t("context.label")}</legend>
        {(["small", "medium", "large"] as const).map((c) => (
          <label key={c}>
            <input type="radio" name="context" value={c} checked={context === c} onChange={() => setContext(c)} />
            {t(`context.${c}`)}
          </label>
        ))}
      </fieldset>
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </form>
  );
}
