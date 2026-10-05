/** A few line icons (24×24, stroke = currentColor). */
const paths = {
  plus: "M12 5v14M5 12h14",
  edit: "M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16v4z",
  home: "M4 11l8-7 8 7v9h-5v-6H9v6H4z",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  chat: "M5 5h14v10H9l-4 4z",
  message: "M3 20l1.3-3.9C2 12.7 2.9 8.2 6.4 5.7c3.5-2.5 8.6-2.3 11.8.5 3.3 2.8 3.7 7.3 1 10.5C16.6 19.9 11.7 20.9 7.7 19z",
  computer: "M3 5a1 1 0 0 1 1-1h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1zM7 20h10M9 16v4M15 16v4",
  code: "M7 8l-4 4 4 4M17 8l4 4-4 4M14 4l-4 16",
  pulse: "M3 12h4l3 8 4-16 3 8h4",
  paperclip: "M15 7l-6.5 6.5a1.5 1.5 0 0 0 3 3L18 10a3 3 0 0 0-6-6l-6.5 6.5a4.5 4.5 0 0 0 9 9L21 13",
  lock: "M5 13a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2zM8 11V7a4 4 0 0 1 8 0v4",
  trash: "M5 7h14M10 7V5h4v2M7 7l1 12h8l1-12",
  sidebar: "M4 5h16v14H4zM9 5v14",
  chevron: "M9 6l6 6-6 6",
  close: "M6 6l12 12M18 6L6 18",
  bulb: "M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9c.6.5 1 1.2 1 2.1h5c0-.9.4-1.6 1-2.1A6 6 0 0 0 12 3z",
  doc: "M7 3h7l4 4v14H7zM14 3v4h4M10 12h5M10 16h5",
  tool: "M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.6 2.6-2.4-.6-.6-2.4z",
  gear: "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1M15 4v4M9 10v4M17 16v4",
  globe: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18zM3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9s1.3-6.4 3.8-9z",
  help: "M12 3a9 9 0 1 0 0 18a9 9 0 1 0 0-18zM9.6 9.4a2.5 2.5 0 1 1 3.4 2.4c-.6.3-1 .8-1 1.5v.7M12 16.8v.2",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg className="ic" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      <path d={paths[name]} />
    </svg>
  );
}
