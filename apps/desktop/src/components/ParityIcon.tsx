/** Glyphs for the command bar, the details pane and Home, drawn like `Icon`'s: 16 px strokes. */
const PATHS = {
  new: "M8 3v10M3 8h10",
  cut: "M5 13a1.8 1.8 0 1 0 0-3.6A1.8 1.8 0 0 0 5 13ZM11 13a1.8 1.8 0 1 0 0-3.6A1.8 1.8 0 0 0 11 13ZM6.3 10 11.5 2.5M9.7 10 4.5 2.5",
  copy: "M5.5 5.5h7.5v8H5.5ZM3 10.5V2.5h7.5",
  paste: "M5.5 3H3.5v10.5h9V3h-2M5.5 2h5v2.5h-5Z",
  rename: "M2.5 5.5h11v5h-11ZM9.5 3.5v9M8.3 3.5h2.4M8.3 12.5h2.4",
  delete: "M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.7 9h5.6l.7-9",
  sort: "M5 2.5v11M2.5 11 5 13.5 7.5 11M11 13.5v-11M8.5 5 11 2.5 13.5 5",
  more: "M3.5 8h.01M8 8h.01M12.5 8h.01",
  details: "M2.5 3.5h11v9h-11ZM9.5 3.5v9M11 6h.01M11 8v2.5",
  terminal: "M2.5 3h11v10h-11ZM5 6.5 7 8.3 5 10M8.5 10.5h3",
  window: "M2.5 4.5h9v8h-9ZM5 4.5V2.5h8.5v8h-2M2.5 6.5h9",
  star: "M8 2.5l1.7 3.5 3.8.5-2.8 2.6.7 3.8L8 11.1l-3.4 1.8.7-3.8L2.5 6.5l3.8-.5Z",
  clock: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM8 5v3.2l2.2 1.4",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  options: "M8 5.6a2.4 2.4 0 1 0 0 4.8 2.4 2.4 0 0 0 0-4.8ZM8 1.8v1.7M8 12.5v1.7M1.8 8h1.7M12.5 8h1.7M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M3.6 12.4l1.2-1.2M11.2 4.8l1.2-1.2",
  fullscreen: "M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10",
  chevron: "M4.5 6 8 9.5 11.5 6",
  select: "M2.5 2.5h4v4h-4ZM9.5 2.5h4v4h-4ZM2.5 9.5h4v4h-4ZM9.5 9.5h4v4h-4Z",
  undo: "M5.5 3.5 2.5 6.5l3 3M2.5 6.5h7a3.5 3.5 0 0 1 0 7H7",
  location: "M2.5 4.5h4L8 6h5.5v6.5h-11ZM8 8.5v3M6.5 10 8 8.5 9.5 10",
  person: "M8 8a2.75 2.75 0 1 0 0-5.5A2.75 2.75 0 0 0 8 8ZM3 13.5c.6-2.4 2.6-3.8 5-3.8s4.4 1.4 5 3.8",
  home: "M2.5 7.5 8 3l5.5 4.5V13h-3.5V9.5h-4V13H2.5Z",
} as const;

export type ParityIconName = keyof typeof PATHS;

export function ParityIcon(props: { name: ParityIconName; size?: number }) {
  return (
    <svg
      class="icon"
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width={props.name === "more" ? 2.4 : 1.4}
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[props.name]} />
    </svg>
  );
}
