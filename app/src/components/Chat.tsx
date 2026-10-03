import { useEffect, useLayoutEffect, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { useI18n } from "../i18n";
import { Icon } from "./Icon";

/**
 * The input of every chat: grows with its text, Enter sends, Shift+Enter
 * starts a new line (and Enter while composing an IME character does not send).
 */
export function Composer({
  label,
  placeholder,
  onSend,
  busy = false,
  onStop,
  disabled = false,
  autoFocus = false,
  extra,
  initial = "",
  above,
  onFiles,
}: {
  label: string;
  placeholder?: string;
  onSend: (text: string) => Promise<unknown> | void;
  busy?: boolean;
  onStop?: () => void;
  disabled?: boolean;
  autoFocus?: boolean;
  /** Controls next to the send button (e.g. a model choice). */
  extra?: ReactNode;
  initial?: string;
  /** Shown above the input (attached documents). */
  above?: ReactNode;
  /** Documents chosen, dropped or pasted – offers the paperclip when set. */
  onFiles?: (files: File[]) => void;
}) {
  const { t } = useI18n();
  const [text, setText] = useState(initial);
  const [dropping, setDropping] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const take = (list: FileList | null | undefined) => {
    const files = Array.from(list ?? []);
    if (files.length > 0 && onFiles) onFiles(files);
  };
  const [sending, setSending] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, window.innerHeight * 0.4)}px`;
  }, [text]);
  useEffect(() => {
    if (autoFocus) area.current?.focus();
  }, [autoFocus]);
  const canSend = text.trim().length > 0 && !busy && !disabled && !sending;
  const send = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!canSend) return;
    const value = text;
    setSending(true);
    try {
      // Cleared at once; given back if sending fails.
      setText("");
      await onSend(value);
    } catch {
      setText(value);
    } finally {
      setSending(false);
      area.current?.focus();
    }
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };
  return (
    <form
      className={dropping ? "composer dropping" : "composer"}
      onSubmit={send}
      onDragOver={(e) => {
        if (!onFiles || !e.dataTransfer.types.includes("Files")) return;
        e.preventDefault();
        setDropping(true);
      }}
      onDragLeave={() => setDropping(false)}
      onDrop={(e) => {
        if (!onFiles) return;
        e.preventDefault();
        setDropping(false);
        take(e.dataTransfer.files);
      }}
    >
      {above}
      <textarea
        ref={area}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKey}
        onPaste={(e) => {
          if (onFiles && e.clipboardData.files.length > 0) {
            e.preventDefault();
            take(e.clipboardData.files);
          }
        }}
        placeholder={placeholder ?? label}
        aria-label={label}
        rows={1}
        disabled={disabled}
      />
      <div className="composer-bar">
        {onFiles && (
          <>
            <button type="button" className="icon attach" aria-label={t("chat.attach")} title={t("chat.attachHint")} onClick={() => picker.current?.click()}>
              <Icon name="paperclip" />
            </button>
            <input
              ref={picker}
              type="file"
              multiple
              hidden
              accept=".pdf,.docx,.xlsx,.xlsm,.xls,.ods,.csv,.tsv,.txt,.md,.markdown,.json,.xml,.yaml,.yml,.log,.html,.htm,.eml"
              onChange={(e) => {
                take(e.target.files);
                e.target.value = "";
              }}
            />
          </>
        )}
        {extra}
        <span className="hint">{t("chat.hint")}</span>
        <span className="spacer" />
        {busy && onStop ? (
          <button type="button" className="send stop" onClick={onStop} aria-label={t("chat.stop")} title={t("chat.stop")}>
            ■
          </button>
        ) : (
          <button type="submit" className="send" disabled={!canSend} aria-label={t("chat.send")} title={t("chat.send")}>
            ↑
          </button>
        )}
      </div>
    </form>
  );
}

/**
 * A scrolling conversation with the composer below it. Follows new content
 * while the reader is at the bottom, and stays put when they scrolled up.
 */
export function ChatLayout({ children, composer, follow }: { children: ReactNode; composer: ReactNode; follow: unknown }) {
  const scroller = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  useEffect(() => {
    const el = scroller.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  }, [follow]);
  return (
    <div className="chat-layout">
      <div
        className="chat-scroll"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        <div className="chat-column">{children}</div>
      </div>
      <div className="composer-dock">{composer}</div>
    </div>
  );
}

export function UserBubble({ text, files }: { text: string; files?: ReactNode }) {
  return (
    <div className="bubble-row user">
      <div className="bubble" data-testid="user-message">
        {files}
        {text}
      </div>
    </div>
  );
}

export function Thinking({ label, lines = [] }: { label: string; lines?: string[] }) {
  return (
    <div className="thinking" role="status">
      <span className="dots">{label}</span>
      {lines.map((l, i) => (
        <code key={i} className="activity">
          {l}
        </code>
      ))}
    </div>
  );
}
