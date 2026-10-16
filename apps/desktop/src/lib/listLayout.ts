/**
 * Where everything sits along a file list's scroll axis once its items are grouped.
 *
 * The list is a run of groups, each a header followed by its items in lines of equal size (see
 * viewLayout.ts). A collapsed group shows its header only. In the views that scroll down, a
 * header takes a line of its own; in the list view, whose columns scroll sideways, a header sits
 * in a band above the group's columns. Without groups the list is one group with no header,
 * and every position below is the plain arithmetic the list always used.
 *
 * Group boundaries come from the engine, which grouped the rows (`layout.rs`); only the starts
 * are held here, so even thousands of groups cost a few kilobytes. The keyboard and the marquee
 * work from the same positions, so items outside the DOM are reached by geometry.
 */
import type { Geometry } from "./viewLayout";

export interface GroupSlot {
  index: number;
  /** First row of the group, and how many rows it has. */
  first: number;
  count: number;
  collapsed: boolean;
  /** Where the group begins along the scroll axis, header included. */
  start: number;
  /** Where its first line of items begins. */
  items: number;
  /** Lines of items shown: none while collapsed. */
  lines: number;
  end: number;
}

export interface LayoutOptions {
  /** A header's size along the scroll axis; 0 where headers sit beside the items (list view). */
  header: number;
  /** Space between one group and the next. */
  gap: number;
}

/** Inclusive row ranges. */
export type Ranges = [number, number][];

export class ListLayout {
  readonly slots: GroupSlot[] = [];
  readonly extent: number;
  readonly grouped: boolean;

  constructor(
    readonly g: Geometry,
    readonly total: number,
    starts: readonly number[] | null,
    collapsed: (index: number) => boolean,
    options: LayoutOptions,
  ) {
    this.grouped = starts !== null && starts.length > 0;
    const firsts = this.grouped ? starts! : [0];
    const across = g.axis === "x";
    let cursor = 0;
    for (let index = 0; index < firsts.length; index++) {
      const first = firsts[index];
      const count = (index + 1 < firsts.length ? firsts[index + 1] : total) - first;
      const shut = this.grouped && collapsed(index);
      const start = cursor + (index > 0 ? options.gap : 0);
      const items = start + (this.grouped ? options.header : 0);
      const lines = shut ? 0 : Math.ceil(count / g.perLine);
      // Sideways, a collapsed group keeps one column's width for its header.
      const size = across && this.grouped ? Math.max(1, lines) * g.lineSize : lines * g.lineSize;
      const end = items + size;
      this.slots.push({ index, first, count, collapsed: shut, start, items, lines, end });
      cursor = end;
    }
    this.extent = cursor;
  }

  /** The group holding `row`. */
  slotOfRow(row: number): GroupSlot {
    let low = 0;
    let high = this.slots.length - 1;
    while (low < high) {
      const mid = (low + high + 1) >> 1;
      if (this.slots[mid].first <= row) low = mid;
      else high = mid - 1;
    }
    return this.slots[low];
  }

  /** The group at a position along the scroll axis. */
  slotAt(position: number): GroupSlot {
    let low = 0;
    let high = this.slots.length - 1;
    while (low < high) {
      const mid = (low + high + 1) >> 1;
      if (this.slots[mid].start <= position) low = mid;
      else high = mid - 1;
    }
    return this.slots[low];
  }

  /** Where the line holding `row` begins; a row in a collapsed group is at its header. */
  lineStart(row: number): number {
    const slot = this.slotOfRow(row);
    if (slot.collapsed) return slot.start;
    return slot.items + Math.floor((row - slot.first) / this.g.perLine) * this.g.lineSize;
  }

  /** Whether `row` can be seen: its group is expanded. */
  shows(row: number): boolean {
    return row >= 0 && row < this.total && !this.slotOfRow(row).collapsed;
  }

  /** The groups that reach into `[from, to)`, with the lines of each that do. */
  blocks(from: number, to: number): { slot: GroupSlot; firstLine: number; lastLine: number }[] {
    const found = [];
    for (let i = this.slotAt(from).index; i < this.slots.length && this.slots[i].start < to; i++) {
      const slot = this.slots[i];
      if (slot.end <= from) continue;
      const firstLine = Math.max(0, Math.floor((from - slot.items) / this.g.lineSize));
      const lastLine = Math.min(slot.lines - 1, Math.floor((to - slot.items) / this.g.lineSize));
      found.push({ slot, firstLine, lastLine });
    }
    return found;
  }

  /** The rows of lines `firstLine` to `lastLine` of a group. */
  rowsOf(slot: GroupSlot, firstLine: number, lastLine: number): number[] {
    const rows: number[] = [];
    const end = Math.min(slot.first + slot.count, slot.first + (lastLine + 1) * this.g.perLine);
    for (let row = slot.first + firstLine * this.g.perLine; row < end; row++) rows.push(row);
    return rows;
  }

  /** The first and last rows that can be seen, or -1 when every group is collapsed. */
  firstShown(): number {
    return this.slots.find((slot) => !slot.collapsed && slot.count > 0)?.first ?? -1;
  }

  lastShown(): number {
    const slot = [...this.slots].reverse().find((each) => !each.collapsed && each.count > 0);
    return slot ? slot.first + slot.count - 1 : -1;
  }

  /** The next expanded group after (or before) `slot`, if any. */
  private neighbour(slot: GroupSlot, step: 1 | -1): GroupSlot | null {
    for (let i = slot.index + step; i >= 0 && i < this.slots.length; i += step) {
      if (!this.slots[i].collapsed && this.slots[i].count > 0) return this.slots[i];
    }
    return null;
  }

  /** The row one line on (or back) from `row`, in the same column where the line is long enough. */
  stepLine(row: number, step: 1 | -1): number {
    const slot = this.slotOfRow(row);
    const { perLine } = this.g;
    const line = Math.floor((row - slot.first) / perLine);
    const column = (row - slot.first) % perLine;
    const last = slot.first + slot.count - 1;
    if (step > 0 && line + 1 < slot.lines) return Math.min(last, row + perLine);
    if (step < 0 && line > 0) return row - perLine;
    const next = this.neighbour(slot, step);
    if (!next) return row;
    if (step > 0) return Math.min(next.first + column, next.first + next.count - 1);
    const lastLine = next.lines - 1;
    return Math.min(next.first + lastLine * perLine + column, next.first + next.count - 1);
  }

  /** The row before or after `row` in reading order, skipping collapsed groups. */
  stepItem(row: number, step: 1 | -1): number {
    const slot = this.slotOfRow(row);
    const target = row + step;
    if (target >= slot.first && target < slot.first + slot.count) return target;
    const next = this.neighbour(slot, step);
    if (!next) return row;
    return step > 0 ? next.first : next.first + next.count - 1;
  }

  /**
   * The rows whose cells meet a rectangle: `along` and `across` are its extent along and across
   * the scroll axis, in list pixels. `cross` says where cells sit across: the first one's offset
   * and each one's size; lines of one item span the whole width.
   */
  rowsInRect(along: [number, number], across: [number, number], cross: { start: number; size: number }): Ranges {
    const ranges: Ranges = [];
    const { perLine, lineSize } = this.g;
    const [a0, a1] = along;
    let columnFrom = 0;
    let columnTo = perLine - 1;
    if (perLine > 1 || this.g.axis === "x") {
      columnFrom = Math.max(0, Math.floor((across[0] - cross.start) / cross.size));
      columnTo = Math.min(perLine - 1, Math.floor((across[1] - cross.start) / cross.size));
      if (columnFrom > columnTo || across[1] < cross.start) return ranges;
    }
    const push = (from: number, to: number) => {
      const previous = ranges[ranges.length - 1];
      if (previous && previous[1] + 1 >= from) previous[1] = Math.max(previous[1], to);
      else ranges.push([from, to]);
    };
    for (const slot of this.slots) {
      if (slot.end < a0) continue;
      if (slot.start > a1) break;
      if (slot.lines === 0) continue;
      const lineFrom = Math.max(0, Math.floor((a0 - slot.items) / lineSize));
      const lineTo = Math.min(slot.lines - 1, Math.floor((a1 - slot.items) / lineSize));
      if (a1 < slot.items || lineFrom > lineTo) continue;
      const last = slot.first + slot.count - 1;
      if (columnFrom === 0 && columnTo === perLine - 1) {
        push(slot.first + lineFrom * perLine, Math.min(last, slot.first + (lineTo + 1) * perLine - 1));
        continue;
      }
      for (let line = lineFrom; line <= lineTo; line++) {
        const from = slot.first + line * perLine + columnFrom;
        if (from > last) break;
        push(from, Math.min(last, slot.first + line * perLine + columnTo));
      }
    }
    return ranges;
  }
}

/**
 * Where a navigation key moves the focus in a grouped list, or `null` if the key does not
 * navigate. Up and Down (Left and Right in the list view) cross into the next group at the same
 * column, as Explorer does; collapsed groups are passed over; a page is `pageLines` lines.
 */
export function groupedKeyTarget(key: string, current: number, layout: ListLayout, pageLines: number): number | null {
  if (layout.total === 0) return null;
  const vertical = layout.g.axis === "y";
  const one = layout.g.perLine === 1 && vertical;
  const along = vertical ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
  const across = vertical ? ["ArrowUp", "ArrowDown"] : ["ArrowLeft", "ArrowRight"];
  const start = current >= 0 && layout.shows(current) ? current : -1;
  switch (key) {
    case "Home":
      return layout.firstShown();
    case "End":
      return layout.lastShown();
    case "PageDown":
    case "PageUp": {
      if (start < 0) return layout.firstShown();
      let row = start;
      for (let i = 0; i < Math.max(1, pageLines); i++) row = layout.stepLine(row, key === "PageDown" ? 1 : -1);
      return row;
    }
  }
  const isAlong = !one && along.includes(key);
  if (!isAlong && !across.includes(key)) return null;
  if (start < 0) return layout.firstShown();
  const forward = key === (isAlong ? along[1] : across[1]);
  return isAlong ? layout.stepItem(start, forward ? 1 : -1) : layout.stepLine(start, forward ? 1 : -1);
}
