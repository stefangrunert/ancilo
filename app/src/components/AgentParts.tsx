import type { OpOutput } from "../api/client";
import { useI18n } from "../i18n";
import { usePro } from "../state/prefs";
import { useClient, useLive, useRefresh } from "../state/store";
import { Thinking } from "./Chat";
import { Markdown } from "./Markdown";

/** What coding sessions and tasks share: the access mode, the agent's
 * questions, its steps. */

type Session = OpOutput<"get_session">;
type Approval = Session["approvals"][number];
export type Access = Session["permission"];
type SessionMessage = NonNullable<Session["messages"]>[number];

/** The access modes of coding sessions (decision
 * `2026-10-03-coding-zugriff-websuche`): "ask" = read freely, ask before
 * changes, commands and searches; "auto" = changes in the copy and sandboxed
 * commands without asking. Searches always ask. (Tasks have no choice: they
 * work on their own – nothing reaches the folder before it is kept.) */
export function AccessChoice({ s, disabled, onChange }: { s: Session; disabled: boolean; onChange: (p: Access) => void }) {
  const { t } = useI18n();
  const hint = s.permission === "shell" ? "code.mode.autoHint" : "code.mode.askHint";
  return (
    <label className="row access-choice" title={t(hint)}>
      <span className="sr-only">{t("code.mode")}</span>
      <select value={s.permission} onChange={(e) => onChange(e.target.value as Access)} disabled={disabled} aria-label={t("code.mode")} data-testid="access-mode">
        <option value="read">{t("code.mode.ask")}</option>
        {/* Sessions from before the two modes may still allow only edits. */}
        {s.permission === "edit" && <option value="edit">{t("code.mode.edit")}</option>}
        <option value="shell">{t("code.mode.auto")}</option>
      </select>
    </label>
  );
}

/** A few words for what the agent wants to do. */
function describe(a: Approval): string {
  const args = (a.arguments ?? {}) as Record<string, unknown>;
  if (a.tool === "bash") return `$ ${String(args.command ?? "")}`;
  if (typeof args.path === "string") return `${a.tool} ${args.path}`;
  return `${a.tool} ${JSON.stringify(args)}`.slice(0, 200);
}

export function Approvals({ s, onError }: { s: Session; onError: (e: unknown) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  if (s.approvals.length === 0) return null;
  const provider = (a: Approval) => (a.sends_to === "serper" ? "Google (Serper)" : "Wikipedia");
  const decide = async (a: Approval, how: "once" | "session" | "no") => {
    try {
      if (how === "no") await client.op("reject", { approval: a.id });
      else await client.op("approve", { approval: a.id, remember: how === "session" });
      await refresh("get_session");
    } catch (e) {
      onError(e);
    }
  };
  return (
    <div className="approvals stack" role="group" aria-label={t("code.approvals")}>
      {s.approvals.map((a) => (
        <div key={a.id} className="card approval" data-testid="approval">
          {a.sends_to ? (
            <>
              <p>
                {t("code.wantsSearch", { provider: provider(a) })} <code>{String((a.arguments as { query?: string })?.query ?? "")}</code>
              </p>
              <p className="muted small">{t("code.searchSends", { provider: provider(a) })}</p>
              <div className="row">
                <button type="button" onClick={() => void decide(a, "once")}>
                  {t("code.search")}
                </button>
                <button type="button" className="secondary" onClick={() => void decide(a, "no")}>
                  {t("code.dontSearch")}
                </button>
              </div>
            </>
          ) : (
            <>
              <p>
                {t("code.wants")} <code>{describe(a)}</code>
              </p>
              <div className="row">
                <button type="button" onClick={() => void decide(a, "once")}>
                  {t("code.allow")}
                </button>
                <button type="button" className="secondary" onClick={() => void decide(a, "session")}>
                  {t("code.allowSession")}
                </button>
                <button type="button" className="secondary" onClick={() => void decide(a, "no")}>
                  {t("code.reject")}
                </button>
              </div>
            </>
          )}
        </div>
      ))}
    </div>
  );
}

/** One piece of a turn: what the agent said, or the steps it took in between. */
type Block = { kind: "user"; text: string } | { kind: "say"; text: string } | { kind: "steps"; steps: { call: string; result?: string }[] };

/** The session's messages as a conversation: tool calls and their results fold into steps. */
export function blocks(messages: SessionMessage[]): Block[] {
  const out: Block[] = [];
  const steps = () => {
    const last = out[out.length - 1];
    if (last?.kind === "steps") return last.steps;
    const b: Block = { kind: "steps", steps: [] };
    out.push(b);
    return b.steps;
  };
  for (const m of messages) {
    if (m.role === "user") out.push({ kind: "user", text: m.text });
    else if (m.role === "tool") {
      const list = steps();
      const open = list.find((x) => x.result === undefined);
      if (open) open.result = m.text;
      else list.push({ call: "", result: m.text });
    } else {
      if (m.text?.trim()) out.push({ kind: "say", text: m.text });
      for (const c of m.tool_calls ?? []) steps().push({ call: c });
    }
  }
  return out;
}

export function Steps({ steps, open }: { steps: { call: string; result?: string }[]; open: boolean }) {
  const { t } = useI18n();
  return (
    <details className="steps" open={open}>
      <summary>{t("code.steps", { n: steps.length })}</summary>
      <div className="steps-body">
        {steps.map((s, i) => (
          <div key={i} className="step">
            {s.call && <code className="call">{s.call}</code>}
            {s.result !== undefined && (
              <details>
                <summary className="muted small">{t("code.toolResult")}</summary>
                <pre>{s.result}</pre>
              </details>
            )}
          </div>
        ))}
      </div>
    </details>
  );
}


/** While the agent works: what it says on the way – at once, for everyone –
 * and how many steps it took so far (in the expert view also its last
 * actions). */
export function LiveWork({ id, label }: { id: string; label: string }) {
  const { t } = useI18n();
  const live = useLive();
  const pro = usePro();
  const turn = live.turns[id];
  const activity = (live.activity[id] ?? []).slice(-3);
  const steps = turn?.steps ?? 0;
  return (
    <>
      {(turn?.notes ?? []).map((n, i) => (
        <div key={i} className="live-note" data-testid="live-note">
          <Markdown text={n} />
        </div>
      ))}
      <Thinking label={steps > 0 ? `${label} · ${t("code.steps", { n: steps })}` : label} lines={pro ? activity : []} />
    </>
  );
}
