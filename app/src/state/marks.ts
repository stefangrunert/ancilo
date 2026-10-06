/**
 * Links of Ancilo's own source marks. Their address carries a key made new
 * each time the app starts: a model writes Markdown, but cannot know the
 * key – a link it writes to a source (however spelled) never becomes one.
 */
const KEY = (() => {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
})();

/** The address of the mark of source `id` ("D3"). */
export const markHref = (id: string): string => `#source-${id}-${KEY}`;

/** The source a link opens – only for a mark Ancilo made. */
export function markOf(href: string | undefined): string | null {
  const m = /^#source-(D\d+)-([0-9a-f]+)$/i.exec(href ?? "");
  return m?.[1] && m[2] === KEY ? m[1].toUpperCase() : null;
}

/** Whether a link points at a source at all (Ancilo's or made up). */
export const toSource = (href: string | undefined): boolean => /^#source-/i.test(href ?? "");
