/**
 * The Gallery's layout: items grouped by month, each month's items in justified rows (each row
 * as tall as it must be for its items, at their own shapes, to fill the width exactly; the last
 * row of a month left at the usual height), under a header per month.
 *
 * Laid out in typed arrays and found by binary search, so tens of thousands of items lay out in
 * a few milliseconds and the page draws only the rows in view.
 */

export interface MonthGroup {
  /** `year * 12 + month - 1`, in local time. */
  key: number;
  year: number;
  /** 1 to 12. */
  month: number;
  start: number;
  count: number;
  /** Where its header starts, from the top of the layout. */
  top: number;
}

export interface GalleryLayout {
  width: number;
  height: number;
  groups: MonthGroup[];
  /** Each row's first item, top and height; one more entry at the end closes the last row. */
  rowStart: Int32Array;
  rowTop: Float64Array;
  rowHeight: Float64Array;
  /** Each item's left edge and width, and its row. */
  left: Float64Array;
  itemWidth: Float64Array;
  rowOf: Int32Array;
}

export interface LayoutOptions {
  /** The height rows have before they are stretched to fill the width. */
  rowHeight: number;
  gap: number;
  header: number;
  /** The space after each month's last row. */
  groupGap: number;
}

/** Runs of consecutive items in the same local month, as the dates come (newest first). */
export function monthGroups(dates: number[]): MonthGroup[] {
  const groups: MonthGroup[] = [];
  for (let i = 0; i < dates.length; i++) {
    const at = new Date(dates[i]);
    const key = at.getFullYear() * 12 + at.getMonth();
    const last = groups[groups.length - 1];
    if (last && last.key === key) last.count += 1;
    else groups.push({ key, year: at.getFullYear(), month: at.getMonth() + 1, start: i, count: 1, top: 0 });
  }
  return groups;
}

/** A shape fit for laying out: unknown (0) is square, and extremes are kept in bounds. */
export const layoutRatio = (ratio: number) => (ratio > 0 ? Math.min(3, Math.max(0.4, ratio)) : 1);

export function justify(ratios: number[], groups: MonthGroup[], width: number, options: LayoutOptions): GalleryLayout {
  const total = ratios.length;
  const left = new Float64Array(total);
  const itemWidth = new Float64Array(total);
  const rowOf = new Int32Array(total);
  const rowStart: number[] = [];
  const rowTop: number[] = [];
  const rowHeight: number[] = [];
  const inner = Math.max(1, width);
  let top = 0;
  const placeRow = (from: number, to: number, height: number, fill: boolean) => {
    const row = rowStart.length;
    rowStart.push(from);
    rowTop.push(top);
    rowHeight.push(height);
    let x = 0;
    for (let i = from; i < to; i++) {
      const last = i === to - 1;
      const w = fill && last ? inner - x : Math.round(layoutRatio(ratios[i]) * height);
      left[i] = x;
      itemWidth[i] = Math.max(1, w);
      rowOf[i] = row;
      x += w + options.gap;
    }
    top += height + options.gap;
  };
  const placed = groups.map((group) => ({ ...group }));
  for (const group of placed) {
    group.top = top;
    top += options.header;
    let from = group.start;
    let sum = 0;
    const end = group.start + group.count;
    for (let i = group.start; i < end; i++) {
      sum += layoutRatio(ratios[i]);
      const gaps = options.gap * (i - from);
      if (sum * options.rowHeight + gaps >= inner) {
        // Stretched (or shrunk) to fill the width exactly.
        placeRow(from, i + 1, Math.max(1, (inner - gaps) / sum), true);
        from = i + 1;
        sum = 0;
      }
    }
    if (from < end) placeRow(from, end, options.rowHeight, false);
    top += options.groupGap - options.gap;
  }
  rowStart.push(total);
  rowTop.push(top);
  rowHeight.push(0);
  return {
    width: inner,
    height: Math.max(0, top),
    groups: placed,
    rowStart: Int32Array.from(rowStart),
    rowTop: Float64Array.from(rowTop),
    rowHeight: Float64Array.from(rowHeight),
    left,
    itemWidth,
    rowOf,
  };
}

/** The number of rows (not counting the closing entry). */
export const rowCount = (layout: GalleryLayout) => layout.rowStart.length - 1;

/** The first row whose bottom is below `y`. */
export function rowAt(layout: GalleryLayout, y: number): number {
  let low = 0;
  let high = rowCount(layout);
  while (low < high) {
    const mid = (low + high) >> 1;
    if (layout.rowTop[mid] + layout.rowHeight[mid] < y) low = mid + 1;
    else high = mid;
  }
  return low;
}

/** The rows that meet the band from `top` to `bottom`, as `[first, end)`. */
export function rowsIn(layout: GalleryLayout, top: number, bottom: number): [number, number] {
  const first = rowAt(layout, top);
  let end = first;
  while (end < rowCount(layout) && layout.rowTop[end] <= bottom) end += 1;
  return [first, end];
}

/** The month group an item is in. */
export function groupOf(layout: GalleryLayout, index: number): number {
  let low = 0;
  let high = layout.groups.length - 1;
  while (low < high) {
    const mid = (low + high + 1) >> 1;
    if (layout.groups[mid].start <= index) low = mid;
    else high = mid - 1;
  }
  return low;
}

/** The item in `row` whose middle is nearest `x`, for moving up and down. */
export function nearestInRow(layout: GalleryLayout, row: number, x: number): number {
  const from = layout.rowStart[row];
  const to = layout.rowStart[row + 1];
  let best = from;
  let distance = Infinity;
  for (let i = from; i < to; i++) {
    const middle = layout.left[i] + layout.itemWidth[i] / 2;
    if (Math.abs(middle - x) < distance) {
      distance = Math.abs(middle - x);
      best = i;
    }
  }
  return best;
}
