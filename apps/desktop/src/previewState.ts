import { createSignal } from "solid-js";

import type { Row } from "./lib/api";
import { baseName } from "./lib/format";
import type { PreviewItem } from "./lib/preview";
import { activeId, activeTab, isSearch } from "./state";

const OPEN_KEY = "preview-open";
const WIDTH_KEY = "preview-width";
export const MIN_PANE_WIDTH = 240;
export const DEFAULT_PANE_WIDTH = 380;

/** Reads a remembered setting; storage can be unavailable, which only loses the memory. */
function remembered(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function remember(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not remembered, which is harmless.
  }
}

const storedWidth = Number(remembered(WIDTH_KEY));
const [paneOpen, setPaneOpenSignal] = createSignal(remembered(OPEN_KEY) === "1");
const [paneWidth, setPaneWidthSignal] = createSignal(
  Number.isFinite(storedWidth) && storedWidth >= MIN_PANE_WIDTH ? storedWidth : DEFAULT_PANE_WIDTH,
);
const [quickLookOpen, setQuickLookOpen] = createSignal(false);

export { paneOpen, paneWidth, quickLookOpen, setQuickLookOpen };

export function setPaneOpen(open: boolean) {
  setPaneOpenSignal(open);
  remember(OPEN_KEY, open ? "1" : "0");
}

export const togglePane = () => setPaneOpen(!paneOpen());

/** Sets the pane's width; `persist` remembers it, which happens once a drag ends. */
export function setPaneWidth(width: number, persist: boolean) {
  const rounded = Math.round(Math.max(MIN_PANE_WIDTH, width));
  setPaneWidthSignal(rounded);
  if (persist) remember(WIDTH_KEY, String(rounded));
}

/** The focused row of each tab's list, as reported by the list once the row has loaded. */
const [focusedRows, setFocusedRows] = createSignal(new Map<number, Row | null>(), { equals: false });

export function setFocusedRow(tabId: number, row: Row | null) {
  const rows = focusedRows();
  if (rows.get(tabId) === row) return;
  rows.set(tabId, row);
  setFocusedRows(rows);
}

/** The focused item of the tab on screen, if any. */
export const focusedItem = (): PreviewItem | null => focusedRows().get(activeId()) ?? null;

/** What the pane shows: the focused item, or else the folder the tab is in. */
export function paneItem(): PreviewItem | null {
  const focused = focusedItem();
  if (focused) return focused;
  const tab = activeTab();
  if (!tab?.listing || isSearch(tab)) return null;
  // The revision stands in for a modification time, so the folder's details are read again
  // whenever its listing changes.
  return { name: baseName(tab.path) || tab.path, path: tab.path, dir: true, size: 0, mtime: tab.revision, ext: "" };
}

/** Each tab list's way of moving its focus, for Quick Look's previous and next. */
const steppers = new Map<number, (delta: number) => void>();

/** Registers a tab's list; the function returned forgets the tab once its list is gone. */
export function registerStepper(tabId: number, step: (delta: number) => void) {
  steppers.set(tabId, step);
  return () => {
    steppers.delete(tabId);
    const rows = focusedRows();
    if (rows.delete(tabId)) setFocusedRows(rows);
  };
}

export const stepFocus = (delta: number) => steppers.get(activeId())?.(delta);
