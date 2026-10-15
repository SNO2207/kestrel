const PATHS = {
  /** A square divided as a treemap divides it. */
  usage: "M2.5 2.5h11v11h-11ZM8.5 2.5v11M2.5 8.5h6M8.5 6h5M11 6v7.5",
  overview: "M3 13.5V8.5M6.5 13.5v-10M10 13.5V9.5M13.5 13.5V6",
  back: "M7 3.5 2.5 8 7 12.5M2.5 8h11",
  close: "M4 4l8 8M12 4l-8 8",
  up: "M8 13V3.5M4 7l4-4 4 4",
  cloud: "M4.5 12.5h7a2.5 2.5 0 0 0 .3-5A3.5 3.5 0 0 0 5 6.8a2.9 2.9 0 0 0-.5 5.7Z",
} as const;

export type DiskIconName = keyof typeof PATHS;

/** Line icons for the disk usage hub, drawn like the app's own `Icon`. */
export function DiskIcon(props: { name: DiskIconName; size?: number }) {
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
