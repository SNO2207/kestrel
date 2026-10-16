const PATHS = {
  /** A page with lines of text: search inside files. */
  inside: "M4 2.5h5l3 3v8H4ZM9 2.5v3h3M6 8.5h4M6 11h3",
  /** Options: three lines of slider. */
  options: "M2.5 4.5h11M2.5 8h11M2.5 11.5h11M5.5 3v3M10.5 6.5v3M7 10v3",
  close: "M4.5 4.5l7 7M11.5 4.5l-7 7",
  stop: "M4.5 4.5h7v7h-7Z",
  /** A penguin, for WSL's Linux distributions. */
  linux: "M8 2.5c-1.6 0-2.4 1.3-2.4 2.9v1.4L3.8 10c-.5 1 .1 2.5 1.4 2.5h5.6c1.3 0 1.9-1.5 1.4-2.5l-1.8-3.2V5.4c0-1.6-.8-2.9-2.4-2.9ZM6.6 8.5c.4.9.8 1.3 1.4 1.3s1-.4 1.4-1.3M7 5.2h.01M9 5.2h.01M5 12.5l-.7 1M11 12.5l.7 1",
  /** A tray with an arrow into it: new downloads. */
  arrived: "M8 2.5v6.5M5.5 6.5 8 9l2.5-2.5M2.5 9.5v4h11v-4h-3l-1 1.5h-3l-1-1.5Z",
  install: "M3.5 3.5h9v9h-9ZM3.5 6h9M8 7.5v3.5M6.5 9.5 8 11l1.5-1.5",
  archive: "M3 4.5h10v2H3ZM4 6.5v7h8v-7M6.5 8.5h3",
  document: "M4 2.5h5l3 3v8H4ZM9 2.5v3h3",
  image: "M2.5 3.5h11v9h-11ZM2.5 10.5l3.5-3 3 2.5 2-1.5 2.5 2M10.5 6.5h.01",
  video: "M2.5 4h8v8h-8ZM10.5 7l3-2v6l-3-2",
  other: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM5.5 8h.01M8 8h.01M10.5 8h.01",
  open: "M9 3.5h3.5V7M12.5 3.5 7.5 8.5M11 9.5v3H3.5V5H6.5",
  move: "M2.5 4.5h4L8 6h5.5v6.5h-11ZM7 9.5h4M9.5 8l1.5 1.5L9.5 11",
  recycle: "M2.5 4.5h11M6.25 4.5V2.75h3.5V4.5M4 4.5l.75 9h6.5l.75-9",
  extract: "M3 4.5h10v2H3ZM4 6.5v7h8v-7M8 8v4M6.5 10.5 8 12l1.5-1.5",
} as const;

export type SearchIconName = keyof typeof PATHS;

/** Line icons for searching, the Downloads triage page and WSL, drawn like the app's own `Icon`. */
export function SearchIcon(props: { name: SearchIconName; size?: number }) {
  return (
    <svg
      class="icon"
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
