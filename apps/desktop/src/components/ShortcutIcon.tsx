/** Glyphs for the shortcuts in the sidebar, drawn like `Icon`'s: 16-unit strokes in the text colour. */
const PATHS = {
  pin: "M9.5 2.5 13.5 6.5M10.5 3.5 7 7 4.5 7.5 8.5 11.5 9 9 12.5 5.5M6.5 9.5 3 13",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  file: "M4 2.5h5l3 3v8H4ZM9 2.5v3h3",
  missing: "M8 3 14 13H2ZM8 7v2.5M8 11.3h.01",
  tag: "M2.5 2.5h5.3l5.7 5.7-5.3 5.3-5.7-5.7ZM5.5 5.5h.01",
  search: "M7 12A5 5 0 1 0 7 2a5 5 0 0 0 0 10ZM10.7 10.7 14 14",
  workspace: "M2.5 5.5h11v7h-11ZM4.5 3.5h7",
  recent: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM8 5v3.2l2 1.3",
  plus: "M8 3.5v9M3.5 8h9",
  chevron: "M6 3.5 10.5 8 6 12.5",
  save: "M4 2.5h8v11L8 10.5 4 13.5Z",
} as const;

export type ShortcutGlyph = keyof typeof PATHS;

export function ShortcutIcon(props: { name: ShortcutGlyph; size?: number; class?: string }) {
  return (
    <svg
      class={`icon ${props.class ?? ""}`}
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[props.name]} />
    </svg>
  );
}
