import { useI18n, type Key } from "../i18n";
import { navigate, type ChatKind } from "../state/route";
import { Icon, type IconName } from "./Icon";

type Action = "chat" | "build" | "setup";

/** The ways into a chat: just chatting (the default), building something, setting Ancilo up. */
const ACTIONS: { kind: Action; icon: IconName }[] = [
  { kind: "chat", icon: "chat" },
  { kind: "build", icon: "tool" },
  { kind: "setup", icon: "gear" },
];

export function QuickActions({ current = "chat" }: { current?: Action }) {
  const { t } = useI18n();
  return (
    <nav className="quick-actions" aria-label={t("quick.label")}>
      {ACTIONS.map((a) => (
        <button
          key={a.kind}
          type="button"
          className={current === a.kind ? "quick on" : "quick"}
          aria-pressed={current === a.kind}
          onClick={() =>
            a.kind === "build"
              ? navigate({ view: "build" })
              : navigate({ view: "chat", id: null, kind: a.kind === "setup" ? "setup" : undefined })
          }
        >
          <Icon name={a.icon} />
          <span>{t(`quick.${a.kind}` as Key)}</span>
        </button>
      ))}
    </nav>
  );
}

/** Back to a new, plain chat (from "build something" and "set up Ancilo"). */
export function BackToChat() {
  const { t } = useI18n();
  return (
    <button type="button" className="ghost back" aria-label={t("quick.backLabel")} onClick={() => navigate({ view: "chat", id: null })}>
      <span aria-hidden="true">←</span> {t("quick.back")}
    </button>
  );
}

/** What Ancilo says first in a chat of this kind, and the answers it offers. */
export function greetingOf(kind: ChatKind, t: (k: Key) => string): { text: string; replies: string[] } | null {
  if (kind !== "setup") return null;
  const replies = t("greet.setup.replies")
    .split("|")
    .map((s) => s.trim())
    .filter(Boolean);
  return { text: t("greet.setup"), replies };
}
