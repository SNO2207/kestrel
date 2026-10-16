const PATHS = {
  back: "M7 3.5 2.5 8 7 12.5M2.5 8h11",
  forward: "M9 3.5 13.5 8 9 12.5M13.5 8h-11",
  up: "M3.5 7 8 2.5 12.5 7M8 2.5v11",
  refresh: "M13 8a5 5 0 1 1-1.5-3.6M13 2.5v2.5h-2.5",
  search: "M7 12A5 5 0 1 0 7 2a5 5 0 0 0 0 10ZM10.7 10.7 14 14",
  close: "M4 4l8 8M12 4l-8 8",
  plus: "M8 3v10M3 8h10",
  chevron: "M6 3.5 10.5 8 6 12.5",
  home: "M2.5 7.5 8 3l5.5 4.5V13h-3.5V9.5h-4V13H2.5Z",
  desktop: "M2.5 3.5h11v7h-11ZM6 13h4M8 10.5V13",
  documents: "M4 2.5h5l3 3v8H4ZM9 2.5v3h3",
  downloads: "M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10",
  pictures: "M2.5 3.5h11v9h-11ZM2.5 10.5l3.5-3 3 2.5 2-1.5 2.5 2M10.5 6.5h.01",
  music: "M6 12V4l6.5-1.5v8M6 12a1.75 1.75 0 1 1-3.5 0 1.75 1.75 0 0 1 3.5 0ZM12.5 10.5a1.75 1.75 0 1 1-3.5 0 1.75 1.75 0 0 1 3.5 0Z",
  videos: "M2.5 4h8v8h-8ZM10.5 7l3-2v6l-3-2",
  drive: "M2.5 9.5h11V13h-11ZM2.5 9.5l2-6.5h7l2 6.5M11 11.3h.01",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  globe: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM2.5 8h11M8 2.5c-2 2-2 9 0 11M8 2.5c2 2 2 9 0 11",
  minimize: "M3.5 8h9",
  maximize: "M3.5 3.5h9v9h-9Z",
  restore: "M5.5 5.5v-2h7v7h-2M3.5 5.5h7v7h-7Z",
  check: "M3.5 8.5 6.5 11.5 12.5 4.5",
  "view-details": "M2.5 4h11M2.5 8h11M2.5 12h11",
  "view-icons": "M2.5 2.5h4v4h-4ZM9.5 2.5h4v4h-4ZM2.5 9.5h4v4h-4ZM9.5 9.5h4v4h-4Z",
  "view-tiles": "M2.5 3h4v4h-4ZM8.5 4h5M8.5 6h3M2.5 9h4v4h-4ZM8.5 10h5M8.5 12h3",
  // Places: This PC, the Recycle Bin and the kinds of drive.
  pc: "M3.5 3.5h9v6.5h-9ZM1.5 12.5h13",
  bin: "M2.5 4.5h11M6.25 4.5V2.75h3.5V4.5M4 4.5l.75 9h6.5l.75-9M6.75 7v4M9.25 7v4",
  usb: "M5.5 6.5h5v7h-5ZM6.5 2.5h3v4h-3Z",
  disc: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM8 6.5a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3Z",
  "network-drive": "M2.5 2.5h11v5h-11ZM8 7.5v4M3 13.5h10M11 5h.01",
  "restore-item": "M5.5 3.5 2.5 6.5l3 3M2.5 6.5h7a3.5 3.5 0 0 1 0 7H7",
  eject: "M3 9.5 8 4l5 5.5ZM3 12.5h10",
  // A penguin, for WSL's Linux distributions (wsl.ts), on This PC.
  linux: "M8 2.5c-1.6 0-2.4 1.3-2.4 2.9v1.4L3.8 10c-.5 1 .1 2.5 1.4 2.5h5.6c1.3 0 1.9-1.5 1.4-2.5l-1.8-3.2V5.4c0-1.6-.8-2.9-2.4-2.9ZM6.6 8.5c.4.9.8 1.3 1.4 1.3s1-.4 1.4-1.3M7 5.2h.01M9 5.2h.01M5 12.5l-.7 1M11 12.5l.7 1",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon(props: { name: IconName; size?: number }) {
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

export const hasIcon = (name: string): name is IconName => name in PATHS;
