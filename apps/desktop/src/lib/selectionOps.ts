/**
 * Selection commands Explorer's "..." menu has besides Select all: Select none and Invert
 * selection. Each tab's list hands over its own (`FileList`'s `selectRef`); the command bar,
 * the palette and the keyboard act on the tab on screen.
 */
import { Selection } from "./selection";

/** What a tab's list lets others do to its selection. */
export interface SelectControl {
  all: () => void;
  none: () => void;
  invert: () => void;
  /** Starts renaming the selected item, or a batch rename of several, as F2 does. */
  rename: () => void;
}

/** Every row of `total` that `selection` leaves out. */
export function inverted(selection: Selection, total: number): Selection {
  const ranges: [number, number][] = [];
  let next = 0;
  for (const [start, end] of selection.ranges) {
    if (start > next) ranges.push([next, Math.min(start, total) - 1]);
    next = Math.max(next, end + 1);
    if (next >= total) break;
  }
  if (next < total) ranges.push([next, total - 1]);
  return Selection.of(ranges.filter(([start, end]) => start <= end));
}

const controls = new Map<number, SelectControl>();

/** Registers a tab's list; the function returned forgets it once the list is gone. */
export function registerSelector(tabId: number, control: SelectControl) {
  controls.set(tabId, control);
  return () => {
    if (controls.get(tabId) === control) controls.delete(tabId);
  };
}

/** The selection commands of a tab's list, if it shows one. */
export const selector = (tabId: number): SelectControl | undefined => controls.get(tabId);
