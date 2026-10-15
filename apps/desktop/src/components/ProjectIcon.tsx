const PATHS = {
  projects: "M2.5 4.5h4L8 6h5.5v6.5h-11ZM6.2 8.4 4.9 9.6l1.3 1.2M9.8 8.4l1.3 1.2-1.3 1.2",
  branch: "M5 3.5v9M5 3.5a1.3 1.3 0 1 0 0 .01M5 12.5a1.3 1.3 0 1 0 0 .01M11 4.5a1.3 1.3 0 1 0 0 .01M11 5.8c0 3.2-6 2.2-6 5.4",
  pin: "M9.5 2.5 13.5 6.5 11 7.5 8.5 10l.5 3-6.5-6.5 3 .5L8 4.5Z",
  close: "M4 4l8 8M12 4l-8 8",
  cards: "M2.5 2.5h4.5v4.5h-4.5ZM9 2.5h4.5v4.5H9ZM2.5 9h4.5v4.5h-4.5ZM9 9h4.5v4.5H9Z",
  table: "M2.5 3.5h11v9h-11ZM2.5 6.5h11M2.5 9.5h11M6 3.5v9",
  external: "M9 2.5h4.5V7M13.5 2.5 7.5 8.5M11.5 9.5v4h-9v-9h4",
  terminal: "M2.5 3.5h11v9h-11ZM5 6.5l2 1.5-2 1.5M8.5 10h3",
  code: "M5.5 4.5 2 8l3.5 3.5M10.5 4.5 14 8l-3.5 3.5",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  play: "M5 3.5v9l7-4.5Z",
  spark: "M8 2v3M8 11v3M2 8h3M11 8h3M4 4l2 2M10 10l2 2M12 4l-2 2M6 10l-2 2",
  window: "M2.5 3.5h11v9h-11ZM2.5 6h11",
} as const;

export type ProjectIconName = keyof typeof PATHS;

export const hasProjectIcon = (name: string): name is ProjectIconName => Object.hasOwn(PATHS, name);

/** Line icons for the projects hub, drawn like the app's own `Icon`. */
export function ProjectIcon(props: { name: ProjectIconName; size?: number }) {
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
