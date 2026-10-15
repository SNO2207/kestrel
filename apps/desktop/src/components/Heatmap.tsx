import { For, createMemo } from "solid-js";

import { formatCount } from "../lib/format";

const WEEKS = 53;
const CELL = 10;
const GAP = 2;
const STEP = CELL + GAP;
/** Room for the weekday labels; the drawing is scaled to the panel's width. */
const LEFT = 34;
const TOP = 19;
const DAY_MS = 86_400_000;
const monthName = new Intl.DateTimeFormat(undefined, { month: "short", timeZone: "UTC" });
const dayName = new Intl.DateTimeFormat(undefined, { weekday: "short", timeZone: "UTC" });
const fullDate = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" });

/** How strongly a day is coloured, from 1 to 4, relative to the busiest day. */
function level(value: number, max: number) {
  if (value <= 0 || max <= 0) return 0;
  const share = value / max;
  return share > 0.75 ? 4 : share > 0.5 ? 3 : share > 0.25 ? 2 : 1;
}

const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`;

/**
 * A year of activity, a column per week and a row per weekday: ochre for days with commits,
 * blue for days when files changed without a commit. Days are counted from the Unix epoch in
 * the viewer's time zone; `start` is the Sunday of the first week.
 */
export function Heatmap(props: { start: number; days: [number, number, number][] }) {
  const byDay = createMemo(() => new Map(props.days.map(([day, commits, files]) => [day, { commits, files }])));
  const today = () => Math.floor((Date.now() - new Date().getTimezoneOffset() * 60_000) / DAY_MS);
  const maxCommits = createMemo(() => Math.max(0, ...props.days.map(([, commits]) => commits)));
  const maxFiles = createMemo(() => Math.max(0, ...props.days.map(([, , files]) => files)));
  const totals = createMemo(() =>
    props.days.reduce((sum, [, commits, files]) => ({ commits: sum.commits + commits, files: sum.files + files }), {
      commits: 0,
      files: 0,
    }),
  );
  const cells = createMemo(() => {
    const out: { day: number; x: number; y: number; commits: number; files: number }[] = [];
    for (let week = 0; week < WEEKS; week++) {
      for (let weekday = 0; weekday < 7; weekday++) {
        const day = props.start + week * 7 + weekday;
        if (day > today()) break;
        const found = byDay().get(day);
        out.push({ day, x: LEFT + week * STEP, y: TOP + weekday * STEP, commits: found?.commits ?? 0, files: found?.files ?? 0 });
      }
    }
    return out;
  });
  /** A label over the first week of each month. */
  const months = createMemo(() => {
    const out: { x: number; label: string }[] = [];
    let last = -1;
    for (let week = 0; week < WEEKS; week++) {
      const date = new Date((props.start + week * 7) * DAY_MS);
      if (date.getUTCMonth() !== last) {
        last = date.getUTCMonth();
        // The first column is often the tail of a month; label it only if it has room.
        if (week < WEEKS - 2 && !(week === 0 && date.getUTCDate() > 24)) out.push({ x: LEFT + week * STEP, label: monthName.format(date) });
      }
    }
    return out;
  });
  const describe = (cell: { day: number; commits: number; files: number }) => {
    const parts = [];
    if (cell.commits > 0) parts.push(plural(cell.commits, "commit", "commits"));
    if (cell.files > 0) parts.push(plural(cell.files, "file changed", "files changed"));
    return `${fullDate.format(new Date(cell.day * DAY_MS))}: ${parts.length ? parts.join(", ") : "no activity"}`;
  };

  return (
    <figure class="heatmap">
      <svg viewBox={`0 0 ${LEFT + WEEKS * STEP} ${TOP + 7 * STEP}`} role="img" aria-label="Activity over the last year">
        <For each={months()}>
          {(month) => (
            <text class="heatmap-label" x={month.x} y={TOP - 6}>
              {month.label}
            </text>
          )}
        </For>
        <For each={[1, 3, 5]}>
          {(weekday) => (
            <text class="heatmap-label" x={0} y={TOP + weekday * STEP + CELL - 1}>
              {/* 1970-01-04, day 3, was a Sunday. */}
              {dayName.format(new Date((3 + weekday) * DAY_MS))}
            </text>
          )}
        </For>
        <For each={cells()}>
          {(cell) => (
            <rect
              class="heatmap-cell"
              classList={{
                [`commits-${level(cell.commits, maxCommits())}`]: cell.commits > 0,
                [`files-${level(cell.files, maxFiles())}`]: cell.commits === 0 && cell.files > 0,
              }}
              x={cell.x}
              y={cell.y}
              width={CELL}
              height={CELL}
              rx="2"
              data-day={cell.day}
              data-commits={cell.commits}
              data-files={cell.files}
            >
              <title>{describe(cell)}</title>
            </rect>
          )}
        </For>
      </svg>
      <figcaption class="heatmap-caption">
        <span>
          {plural(totals().commits, "commit", "commits")} and {plural(totals().files, "file change", "file changes")} in the
          last year
        </span>
        <span class="heatmap-key">
          <i class="heatmap-swatch commits-3" /> Commits
          <i class="heatmap-swatch files-3" /> Files changed
        </span>
      </figcaption>
    </figure>
  );
}
