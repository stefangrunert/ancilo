import { useState } from "react";
import type { OpOutput } from "../api/client";
import { useI18n, type Key } from "../i18n";
import { usePro } from "../state/prefs";
import { navigate, type ChatKind } from "../state/route";
import { type Live, useClient, useLive, useOp, useRefresh } from "../state/store";
import { ChatLayout, Composer, Thinking, UserBubble } from "./Chat";
import { Markdown } from "./Markdown";
import { BackToChat, greetingOf, QuickActions } from "./QuickActions";
import { nameOf } from "./ChatProjects";
import { DocChip, LocalNote, PendingDocs, usePendingDocs } from "./Documents";
import { Icon } from "./Icon";
import { ErrorNote } from "./ui";
import { WebSwitch } from "./WebSearch";
import { DocSources, linkEvidence, SourceDialog, useSource } from "./Sources";

type Conversation = OpOutput<"get_conversation">;
type Message = Conversation["messages"][number];
type Pending = NonNullable<Message["pending"]>[number];
type WebNote = NonNullable<Message["web"]>;
type T = (k: Key, v?: Record<string, string | number>) => string;

/** The technical form (for experts, folded away). */
export function describeAction(p: { operation: string; input?: unknown }): string {
  const input = p.input as Record<string, unknown> | null | undefined;
  const args = input
    ? Object.entries(input)
        .map(([k, v]) => `${k}=${typeof v === "string" ? v : JSON.stringify(v)}`)
        .join(" ")
    : "";
  return `${p.operation} ${args}`.trim();
}

const KNOWN_ACTIONS = [
  "assign_role",
  "set_route",
  "start_model",
  "stop_model",
  "add_model",
  "remove_model",
  "set_resources",
  "connect_claude_code",
  "connect_codex",
  "disconnect_claude_code",
  "disconnect_codex",
  "setup",
  "set_permissions",
  "set_pinned",
  "unload_models",
  "create_project",
  "compare_models",
  "ab_start",
  "apply_recommendation",
];

/** What a proposed action will do – one plain sentence. */
export function actionSentence(p: { operation: string; summary?: string; input?: unknown }, t: T): string {
  const i = (p.input ?? {}) as Record<string, unknown>;
  const s = (v: unknown) => (typeof v === "string" ? v : v === undefined || v === null ? "" : JSON.stringify(v));
  if (!KNOWN_ACTIONS.includes(p.operation)) return p.summary || p.operation;
  const role = s(i.role);
  const roleText = ["default", "delegation", "coding", "assistant", "embed"].includes(role) ? t(`role.${role}` as Key) : role;
  return t(`action.${p.operation}` as Key, {
    model: s(i.model ?? i.b),
    role: roleText,
    kind: s(i.kind),
    level: i.level ? t(`cockpit.level.${s(i.level)}` as Key) : "",
    address: s(i.address).replace(/^hf\.co\//, "").split(":")[0] ?? "",
    access: i.max_access ? t(`allow.${s(i.max_access)}` as Key) : "",
    name: s(i.name),
  });
}

/** What Ancilo looked at, in words ("your models, your computer"). */
function lookedAt(ops: string[], t: T): string {
  const known = ["list_models", "hardware_info", "diagnose", "search", "resource_status", "recommend_models", "gateway_stats", "leaderboard", "connections", "get_permissions"];
  const words = [...new Set(ops.map((o) => (known.includes(o) ? t(`looked.${o}` as Key) : t("looked.other"))))];
  return words.join(", ");
}

function Proposals({ pending, live, onAsk }: { pending: Pending[]; live: Set<string>; onAsk: (text: string) => void }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const pro = usePro();
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const decide = async (p: Pending, run: boolean) => {
    setBusy(p.id);
    try {
      if (run) await client.op("confirm_action", { id: p.id }, true);
      else await client.op("reject_action", { id: p.id });
      await refresh("get_conversation", "pending_actions", "list_models", "list_routes", "ab_status", "list_comparisons", "connections", "get_permissions", "resource_status", "list_projects");
    } catch (e) {
      setErrors((x) => ({ ...x, [p.id]: String((e as Error).message ?? e) }));
    } finally {
      setBusy(null);
    }
  };
  const outcome = (p: Pending) => {
    const o = p.outcome;
    if (errors[p.id]) return <span className="outcome bad">{errors[p.id]}</span>;
    if (o === "executed") return <span className="outcome ok" role="status">{t("assistant.doneFriendly")}</span>;
    if (o === "rejected") return <span className="outcome" role="status">{t("assistant.skipped")}</span>;
    if (o) return <span className="outcome bad" role="status">{o}</span>;
    if (!live.has(p.id)) return <span className="outcome">{t("assistant.expired")}</span>;
    return null;
  };
  return (
    <div className="card pending" role="group" aria-label={t("assistant.proposed")}>
      <h3>{t("assistant.proposedFriendly")}</h3>
      <ul>
        {pending.map((p) => (
          <li key={p.id} className="proposal">
            <span className="proposal-text">{actionSentence(p, t)}</span>
            {outcome(p) ?? (
              <span className="actions">
                <button type="button" disabled={busy !== null} onClick={() => void decide(p, true)}>
                  {t("assistant.yes")}
                </button>
                <button type="button" className="secondary" disabled={busy !== null} onClick={() => void decide(p, false)}>
                  {t("assistant.no")}
                </button>
                <button type="button" className="ghost small" onClick={() => onAsk(t("assistant.whatMeans"))}>
                  {t("assistant.whatMeans")}
                </button>
              </span>
            )}
            {pro && (
              <details className="tech">
                <summary className="muted small">{t("assistant.technical")}</summary>
                <code className="small">{describeAction(p)}</code>
              </details>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function providerName(p: WebNote["provider"], t: T): string {
  return p ? t(`web.provider.${p}` as Key) : "";
}

/** What the web search behind an answer was, and its sources. */
function WebSources({ web }: { web: WebNote }) {
  const { t, says } = useI18n();
  if (web.state === "failed") {
    return <p className="reply-meta web-failed">{t("web.chat.failed", { why: says(web.error ?? "") })}</p>;
  }
  if (web.state === "offer") {
    return (
      <div className="web-offer" data-testid="web-offer">
        <span>{t("web.chat.offer")}</span>
        <button type="button" className="secondary" onClick={() => navigate({ view: "web" })}>
          {t("web.setUp")}
        </button>
      </div>
    );
  }
  if (web.state !== "searched") return null;
  const sources = web.sources ?? [];
  return (
    <div className="web-sources" data-testid="web-sources">
      <p className="reply-meta">{t("web.chat.searched", { query: web.query ?? "", provider: providerName(web.provider, t) })}</p>
      {sources.length > 0 && (
        <ol aria-label={t("web.chat.sources")}>
          {sources.map((s) => (
            <li key={s.n} value={s.n}>
              <a href={s.url} target="_blank" rel="noopener noreferrer" title={s.url}>
                {s.title || s.url}
              </a>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/** "[1]" in an answer becomes a link to source 1. */
export function linkCitations(text: string, sources: { n: number; url: string }[]): string {
  return text.replace(/\[(\d+)\](?!\()/g, (all, n: string) => {
    const s = sources.find((x) => x.n === Number(n));
    return s?.url ? `[[${n}]](${s.url})` : all;
  });
}

/** Ancilo asks before anything goes out: the query, changeable, and yes or no. */
function WebProposal({ web, onDecide, busy }: { web: WebNote; onDecide: (search: boolean, query: string) => void; busy: boolean }) {
  const { t } = useI18n();
  const [query, setQuery] = useState(web.query ?? "");
  return (
    <form
      className="web-proposal"
      data-testid="web-proposal"
      onSubmit={(e) => {
        e.preventDefault();
        if (query.trim()) onDecide(true, query);
      }}
    >
      <p>
        <strong>{t("web.chat.ask")}</strong>
      </p>
      <label className="stack-tight">
        <span className="muted small">{t("web.chat.only", { provider: providerName(web.provider, t) })}</span>
        <input type="text" aria-label={t("web.chat.query")} value={query} onChange={(e) => setQuery(e.target.value)} disabled={busy} />
      </label>
      <div className="row">
        <button type="submit" disabled={busy || !query.trim()}>
          {t("web.chat.search")}
        </button>
        <button type="button" className="secondary" disabled={busy} onClick={() => onDecide(false, query)}>
          {t("web.chat.noSearch")}
        </button>
      </div>
    </form>
  );
}

function Reply({ m, live, onAsk, conversation }: { m: Message; live: Set<string>; onAsk: (text: string) => void; conversation: string | null }) {
  const { t, says } = useI18n();
  const pro = usePro();
  const source = useSource();
  const ran = (m.operations ?? []).filter((o) => o.outcome !== "proposed");
  const failed = m.text.startsWith("(failed:");
  const evidence = m.evidence ?? [];
  // A decided proposal leaves no trace of its own: the answer below says what happened.
  if (m.web && ["proposed", "accepted", "declined"].includes(m.web.state) && !m.text) return null;
  let text = m.web?.state === "searched" ? linkCitations(m.text, m.web.sources ?? []) : says(m.text);
  if (evidence.length > 0) text = linkEvidence(text, evidence);
  return (
    <div className={failed ? "reply failed" : "reply"}>
      <div data-testid="assistant-answer">
        <Markdown text={text} onSource={evidence.length > 0 ? source.open : undefined} sources={evidence.map((e) => e.id)} />
      </div>
      {evidence.length > 0 && conversation && (
        <>
          <DocSources evidence={evidence} dropped={m.dropped_marks ?? []} onOpen={source.open} />
          <SourceDialog conversation={conversation} mark={source.mark} onClose={source.close} />
        </>
      )}
      {m.web && <WebSources web={m.web} />}
      {(m.pending ?? []).length > 0 && <Proposals pending={m.pending ?? []} live={live} onAsk={onAsk} />}
      {ran.length > 0 && !pro && <p className="reply-meta">{t("assistant.lookedAt", { what: lookedAt(ran.map((o) => o.operation), t) })}</p>}
      {ran.length > 0 && pro && (
        <div className="reply-meta">
          <details className="steps">
            <summary>{t("assistant.lookedAt", { what: lookedAt(ran.map((o) => o.operation), t) })}</summary>
            <div className="steps-body">
              {ran.map((o, i) => (
                <div key={i} className="step">
                  <code className={o.outcome === "failed" ? "call op-chip failed" : "call op-chip"}>{describeAction(o)}</code>
                  {o.result && <pre>{o.result}</pre>}
                </div>
              ))}
            </div>
          </details>
        </div>
      )}
    </div>
  );
}

/** Answers Ancilo offers to click. */
function Replies({ replies, onPick }: { replies: string[]; onPick: (text: string) => void }) {
  const { t } = useI18n();
  if (replies.length === 0) return null;
  return (
    <div className="replies" role="group" aria-label={t("quick.replies")}>
      {replies.map((r) => (
        <button key={r} type="button" className="reply-chip" onClick={() => onPick(r)}>
          {r}
        </button>
      ))}
    </div>
  );
}

/**
 * A chat – with the local model, about anything. Chats started from a quick
 * action begin with Ancilo's greeting and answers to click; Ancilo's own
 * ("set up Ancilo") can change Ancilo, after asking.
 */
/** What a chat does right now, in words: loading the model, searching the
 * web, writing the answer – or thinking. */
function chatProgress(t: ReturnType<typeof useI18n>["t"], live: Live, id: string | null | undefined): string {
  if (Object.keys(live.loading).length > 0) return t("assistant.loadingModel");
  const phase = id ? live.phase[id] : undefined;
  if (phase?.step === "web") return t("assistant.searchingWeb", { query: phase.query });
  if (phase?.step === "answer") return t("assistant.answering");
  return t("assistant.thinking");
}

export function ConversationView({ id, kind = "chat" }: { id: string | null; kind?: ChatKind }) {
  const { t } = useI18n();
  const client = useClient();
  const refresh = useRefresh();
  const live = useLive();
  const pro = usePro();
  const conversation = useOp("get_conversation", id ? { id } : undefined, Boolean(id));
  const pendingActions = useOp("pending_actions");
  const [inflight, setInflight] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [searching, setSearching] = useState(false);
  const pending = usePendingDocs();
  const messages = id ? (conversation.data?.messages ?? []) : [];
  const local = Boolean(conversation.data?.folder) || messages.some((m) => m.documents || (m.attachments ?? []).length > 0) || pending.ids.length > 0;
  const liveIds = new Set((pendingActions.data ?? []).map((p) => p.id));
  const greeting = id ? null : greetingOf(kind, t);

  const ask = async (prompt: string) => {
    setError(null);
    setInflight(prompt);
    const sent = pending.ids.length > 0 ? { attachments: pending.ids } : {};
    try {
      const r = await client.op("ask", id ? { prompt, conversation: id, ...sent } : { prompt, remember: true, kind, greeting: greeting?.text ?? null, ...sent });
      pending.clear();
      await refresh("get_conversation", "list_conversations", "pending_actions");
      if (!id && r.conversation) navigate({ view: "chat", id: r.conversation }, true);
    } catch (e) {
      setError(e);
      throw e;
    } finally {
      setInflight(null);
    }
  };
  const pick = (text: string) => void ask(text).catch(() => {});
  const decide = async (search: boolean, query: string) => {
    if (!id) return;
    setError(null);
    setSearching(true);
    try {
      await client.op("answer_web_proposal", { conversation: id, search, query: search ? query : null });
      await refresh("get_conversation", "list_conversations");
    } catch (e) {
      setError(e);
    } finally {
      setSearching(false);
    }
  };
  // Setup chats are about Ancilo itself: no web there.
  const webSwitch = kind !== "setup" ? <WebSwitch onError={setError} /> : undefined;

  // Setup chats are about Ancilo itself: no documents there either.
  const docs = kind !== "setup" && (!id || conversation.data?.kind !== "setup");
  const composer = (
    <Composer
      label={t("assistant.placeholder")}
      placeholder={t(greeting ? "assistant.placeholderReply" : "assistant.placeholderLong")}
      onSend={ask}
      busy={inflight !== null || searching || pending.reading}
      autoFocus
      extra={webSwitch}
      onFiles={docs ? pending.add : undefined}
      above={
        <>
          {local && <LocalNote />}
          <PendingDocs docs={pending.docs} onRemove={pending.remove} />
        </>
      }
    />
  );
  // A new plain chat: what one can do here.
  if (!id && !greeting && !inflight && messages.length === 0) {
    return (
      <div className="chat-empty">
        <h1>{t("assistant.welcome")}</h1>
        <div className="composer-wrap">{composer}</div>
        <QuickActions />
        <ErrorNote error={error} onDismiss={() => setError(null)} />
      </div>
    );
  }
  const activity = id ? (live.activity[id] ?? []).slice(-3) : [];
  const last = messages[messages.length - 1];
  return (
    <ChatLayout composer={composer} follow={`${messages.length}:${inflight ?? ""}:${activity.length}`}>
      {conversation.error && <ErrorNote error={conversation.error} />}
      {conversation.data?.folder && (
        <div>
          <button type="button" className="ghost folder-chip" onClick={() => navigate({ view: "folder", path: conversation.data!.folder! })}>
            <Icon name="folder" size={14} /> {nameOf(conversation.data.folder)}
          </button>
        </div>
      )}
      {greeting && (
        <div>
          <BackToChat />
        </div>
      )}
      {greeting && (
        <div className="reply greeting" data-testid="greeting">
          <Markdown text={greeting.text} />
          {!inflight && <Replies replies={greeting.replies} onPick={pick} />}
        </div>
      )}
      {messages.map((m, i) =>
        m.role === "user" ? (
          <UserBubble
            key={i}
            text={m.text}
            files={
              (m.attachments ?? []).length > 0 && (
                <span className="doc-row">
                  {(m.attachments ?? []).map((a) => (
                    <DocChip key={a.id} name={a.name} view={a} />
                  ))}
                </span>
              )
            }
          />
        ) : (
          <Reply key={i} m={m} live={liveIds} onAsk={pick} conversation={id} />
        ),
      )}
      {/* A greeting kept in the conversation offers its answers until the first reply. */}
      {id && messages.length === 1 && last?.role === "assistant" && !inflight && conversation.data && (
        <Replies replies={greetingOf(conversation.data.kind as ChatKind, t)?.replies ?? []} onPick={pick} />
      )}
      {inflight && !messages.some((m, i) => i === messages.length - 1 && m.role === "user" && m.text === inflight) && <UserBubble text={inflight} />}
      {last?.web?.state === "proposed" && !inflight && <WebProposal key={messages.length} web={last.web} busy={searching} onDecide={(s, q) => void decide(s, q)} />}
      {searching && <Thinking label={t("web.chat.searching")} lines={pro ? activity : []} />}
      {inflight && <Thinking label={chatProgress(t, live, id)} lines={pro ? activity : []} />}
      <ErrorNote error={error} onDismiss={() => setError(null)} />
    </ChatLayout>
  );
}
