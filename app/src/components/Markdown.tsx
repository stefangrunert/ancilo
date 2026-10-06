import { memo, useMemo, useState, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { useI18n } from "../i18n";

function textOf(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (node && typeof node === "object" && "props" in node) return textOf((node as { props: { children?: ReactNode } }).props.children);
  return "";
}

function CodeBlock({ language, children }: { language: string; children: ReactNode }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const text = textOf(children).replace(/\n$/, "");
  return (
    <div className="code-block">
      <div className="code-head">
        <span>{language || t("markdown.code")}</span>
        <button
          type="button"
          className="icon"
          onClick={() => {
            void navigator.clipboard?.writeText(text).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          {copied ? t("markdown.copied") : t("markdown.copy")}
        </button>
      </div>
      <pre>
        <code>{text}</code>
      </pre>
    </div>
  );
}

const components: Components = {
  // Fenced code: a block with its language and a copy button.
  pre: ({ children }) => {
    const child = Array.isArray(children) ? children[0] : children;
    const className = (child as { props?: { className?: string } })?.props?.className ?? "";
    const language = /language-([\w+-]+)/.exec(className)?.[1] ?? "";
    return <CodeBlock language={language}>{(child as { props?: { children?: ReactNode } })?.props?.children ?? children}</CodeBlock>;
  },
  // Links leave the app (the system browser in the desktop app).
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
  // Model output never loads images from elsewhere.
  img: ({ alt }) => <span>{alt ? `[${alt}]` : ""}</span>,
};

/**
 * A model's answer as Markdown (GitHub flavour). Raw HTML in the text is not
 * rendered – the page can operate Ancilo, so model output never becomes markup.
 * With `onSource`, a link to `#source-D3` (a checked source of an answer
 * from documents, see Sources) opens that source instead of leaving the app.
 */
export const Markdown = memo(function Markdown({ text, onSource, sources }: { text: string; onSource?: (id: string) => void; sources?: string[] }) {
  const { t } = useI18n();
  const withSources = useMemo<Components>(
    () =>
      onSource
        ? {
            ...components,
            a: ({ href, children }) => {
              const id = href?.startsWith("#source-") ? href.slice("#source-".length) : null;
              // Only a source of this very answer becomes a button; any other
              // link to a source is its plain text.
              if (id && !(sources ?? []).includes(id)) return <>{children}</>;
              if (!id) {
                return (
                  <a href={href} target="_blank" rel="noreferrer noopener">
                    {children}
                  </a>
                );
              }
              return (
                <button type="button" className="source-mark" data-testid={`mark-${id}`} aria-label={t("sources.open", { n: textOf(children) })} onClick={() => onSource(id)}>
                  {children}
                </button>
              );
            },
          }
        : components,
    [onSource, sources, t],
  );
  return (
    <div className="markdown">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={withSources} skipHtml>
        {text}
      </ReactMarkdown>
    </div>
  );
});
