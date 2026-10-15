/**
 * A set of selected row positions stored as sorted, non-overlapping inclusive ranges, so that
 * selecting all of a million-row list costs the same as selecting one row. Immutable: every
 * operation returns a new selection.
 */
export class Selection {
  static readonly empty = new Selection([]);

  private constructor(readonly ranges: readonly (readonly [number, number])[]) {}

  /** Builds a selection from sorted, non-overlapping inclusive ranges. */
  static of(ranges: readonly (readonly [number, number])[]): Selection {
    return ranges.length ? new Selection(ranges) : Selection.empty;
  }

  static range(a: number, b: number): Selection {
    return new Selection([[Math.min(a, b), Math.max(a, b)]]);
  }

  has(row: number): boolean {
    return this.ranges.some(([start, end]) => row >= start && row <= end);
  }

  get count(): number {
    return this.ranges.reduce((sum, [start, end]) => sum + end - start + 1, 0);
  }

  /** The lowest selected row, or -1 when nothing is selected. */
  get first(): number {
    return this.ranges.length ? this.ranges[0][0] : -1;
  }

  add(a: number, b: number): Selection {
    const merged: [number, number][] = [];
    let [start, end] = [Math.min(a, b), Math.max(a, b)];
    for (const [s, e] of this.ranges) {
      if (e < start - 1) merged.push([s, e]);
      else if (s > end + 1) {
        merged.push([start, end]);
        [start, end] = [s, e];
      } else {
        start = Math.min(start, s);
        end = Math.max(end, e);
      }
    }
    merged.push([start, end]);
    return new Selection(merged);
  }

  toggle(row: number): Selection {
    if (!this.has(row)) return this.add(row, row);
    const split: [number, number][] = [];
    for (const [s, e] of this.ranges) {
      if (row < s || row > e) split.push([s, e]);
      else {
        if (s < row) split.push([s, row - 1]);
        if (row < e) split.push([row + 1, e]);
      }
    }
    return new Selection(split);
  }

  /** Up to `limit` selected rows, lowest first. */
  rows(limit: number): number[] {
    const out: number[] = [];
    for (const [start, end] of this.ranges) {
      for (let row = start; row <= end && out.length < limit; row++) out.push(row);
    }
    return out;
  }
}
