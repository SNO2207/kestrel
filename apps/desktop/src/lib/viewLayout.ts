/**
 * The geometry of each view mode, which the file list virtualises over.
 *
 * Every view is a sequence of equal lines of equal items: details has one row per line, the
 * icon and tile views have rows of cells, and the list view has columns that flow left to
 * right, as in Explorer. Only the lines in view are rendered, whatever the item count.
 */
import type { ViewMode } from "./views";

export interface Geometry {
  mode: ViewMode;
  /** Lines run down the view (rows), or across it (the list view's columns). */
  axis: "y" | "x";
  /** Items in each line. */
  perLine: number;
  /** A line's extent along the scroll axis: a row's height, or a column's width. */
  lineSize: number;
  /** An item's extent along its line: a cell's least width, or a list entry's height. */
  itemSize: number;
  /** CSS pixels of each item's icon or thumbnail. */
  visual: number;
}

interface Spec {
  axis: "y" | "x";
  /** Least cell width (rows of cells), or column width (the list view). */
  width: number;
  height: number;
  visual: number;
}

const SPECS: Record<ViewMode, Spec> = {
  details: { axis: "y", width: 0, height: 28, visual: 16 },
  list: { axis: "x", width: 264, height: 24, visual: 16 },
  tiles: { axis: "y", width: 280, height: 72, visual: 48 },
  small: { axis: "y", width: 92, height: 88, visual: 32 },
  medium: { axis: "y", width: 112, height: 124, visual: 64 },
  large: { axis: "y", width: 168, height: 192, visual: 128 },
  xl: { axis: "y", width: 296, height: 324, visual: 256 },
  // One wide row per item, with a thumbnail and two lines of details (ViewItem.tsx).
  content: { axis: "y", width: 0, height: 66, visual: 48 },
};

/**
 * Explorer's "Compact view" (parity2.ts): the same views with less space around each item,
 * matched by `.filelist.compact` in parity2.css.
 */
const COMPACT: Record<ViewMode, Spec> = {
  details: { axis: "y", width: 0, height: 22, visual: 16 },
  list: { axis: "x", width: 264, height: 20, visual: 16 },
  tiles: { axis: "y", width: 264, height: 58, visual: 48 },
  small: { axis: "y", width: 84, height: 76, visual: 32 },
  medium: { axis: "y", width: 104, height: 106, visual: 64 },
  large: { axis: "y", width: 160, height: 172, visual: 128 },
  xl: { axis: "y", width: 288, height: 300, visual: 256 },
  content: { axis: "y", width: 0, height: 56, visual: 48 },
};

/** Views with one item per line, spanning the view: details and content. */
export const oneItemPerLine = (mode: ViewMode) => mode === "details" || mode === "content";

/** Space between the edges of the view and its items. */
export const GRID_PADDING = 8;

export function geometry(mode: ViewMode, width: number, height: number, compact = false): Geometry {
  const spec = (compact ? COMPACT : SPECS)[mode];
  // Details and content rows span the view; their columns are laid out by CSS.
  if (oneItemPerLine(mode)) return { mode, axis: "y", perLine: 1, lineSize: spec.height, itemSize: 0, visual: spec.visual };
  if (spec.axis === "x") {
    const perLine = Math.max(1, Math.floor((height - GRID_PADDING) / spec.height));
    return { mode, axis: "x", perLine, lineSize: spec.width, itemSize: spec.height, visual: spec.visual };
  }
  const perLine = Math.max(1, Math.floor((width - 2 * GRID_PADDING) / spec.width));
  return { mode, axis: "y", perLine, lineSize: spec.height, itemSize: spec.width, visual: spec.visual };
}

export const sameGeometry = (a: Geometry, b: Geometry) =>
  a.mode === b.mode && a.perLine === b.perLine && a.lineSize === b.lineSize && a.itemSize === b.itemSize;

/** Views that show thumbnails rather than just icons. */
export const showsThumbnails = (mode: ViewMode) => mode !== "details" && mode !== "list";

/**
 * Where a navigation key moves the focus from `current`, or `null` if the key does not
 * navigate. Along a line the arrows step one item; across lines they step a whole line,
 * landing on the last item when the next line is shorter, as Explorer does. `page` is the
 * number of items a Page Up or Page Down moves.
 */
export function keyTarget(key: string, current: number, total: number, g: Geometry, page: number): number | null {
  if (total === 0) return null;
  const last = total - 1;
  const clamp = (item: number) => Math.min(Math.max(0, item), last);
  switch (key) {
    case "Home":
      return 0;
    case "End":
      return last;
    case "PageDown":
      return clamp(Math.max(0, current) + page);
    case "PageUp":
      return clamp(current - page);
  }
  const vertical = g.axis === "y";
  // Keys that step to the neighbouring item in a line, and to the next or previous line.
  const along = vertical ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
  const across = vertical ? ["ArrowUp", "ArrowDown"] : ["ArrowLeft", "ArrowRight"];
  // Details and content have one item per line, so only Up and Down move in them.
  const alongMoves = !oneItemPerLine(g.mode) && along.includes(key);
  if (!alongMoves && !across.includes(key)) return null;
  if (current < 0) return 0;
  const forward = key === (alongMoves ? along[1] : across[1]);
  const target = current + (forward ? 1 : -1) * (alongMoves ? 1 : g.perLine);
  if (target >= 0 && target <= last) return target;
  const lineOf = (item: number) => Math.floor(item / g.perLine);
  // Stepping into a last line too short to have an item there lands on its last item.
  if (!alongMoves && forward && lineOf(current) < lineOf(last)) return last;
  return current;
}

/** The icon size to ask the system for: one of the sizes its image lists hold. */
export function iconPixels(cssPixels: number): number {
  const wanted = Math.ceil(cssPixels * (window.devicePixelRatio || 1));
  return [16, 32, 48].find((size) => size >= wanted) ?? 256;
}

const THUMB_SIZES = [32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024];

/** The thumbnail size to ask for: enough for every device pixel, from a few fixed sizes. */
export function thumbPixels(cssPixels: number): number {
  const wanted = Math.ceil(cssPixels * (window.devicePixelRatio || 1));
  return THUMB_SIZES.find((size) => size >= wanted) ?? THUMB_SIZES[THUMB_SIZES.length - 1];
}
