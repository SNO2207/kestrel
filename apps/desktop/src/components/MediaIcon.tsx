/** The media library's icons, drawn like the app's others: 16 by 16, stroked, current colour. */
const PATHS = {
  gallery: "M2.5 4.5h9v8h-9ZM4.5 2.5h9v8M2.5 10.5l2.5-2.2 2 1.7 1.5-1.2 3 2.2M8.8 6.6h.01",
  photo: "M2.5 3.5h11v9h-11ZM2.5 10.5l3.5-3 3 2.5 2-1.5 2.5 2M10.5 6.5h.01",
  video: "M2.5 4h8v8h-8ZM10.5 7l3-2v6l-3-2",
  play: "M5 3.5v9l7-4.5Z",
  pause: "M5.5 3.5v9M10.5 3.5v9",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  settings:
    "M8 5.6a2.4 2.4 0 1 0 0 4.8 2.4 2.4 0 0 0 0-4.8ZM8 1.8v1.7M8 12.5v1.7M1.8 8h1.7M12.5 8h1.7M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M3.6 12.4l1.2-1.2M11.2 4.8l1.2-1.2",
  text: "M3 4h10M3 7h10M3 10h6M3 13h8",
  copy: "M5.5 5.5h7.5v8H5.5ZM3 10.5V2.5h7.5",
  check: "M3.5 8.5 6.5 11.5 12.5 4.5",
  wait: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM8 5v3.2l2.2 1.4",
  chevron: "M4.5 6 8 9.5 11.5 6",
  plus: "M8 3v10M3 8h10",
  close: "M4 4l8 8M12 4l-8 8",
  cloud: "M4.5 12.5h7a2.5 2.5 0 0 0 .3-5A3.5 3.5 0 0 0 5 6.6a3 3 0 0 0-.5 5.9Z",
} as const;

export type MediaIconName = keyof typeof PATHS;

export function MediaIcon(props: { name: MediaIconName; size?: number; filled?: boolean }) {
  return (
    <svg
      class="icon media-icon"
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 16 16"
      fill={props.filled ? "currentColor" : "none"}
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
