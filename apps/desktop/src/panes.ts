/**
 * The second pane: showing and hiding it, the split between the panes, moving the keyboard
 * between them, copying and moving to the other pane, and the layout kept between runs.
 *
 * Tabs belong to a pane (`Tab.pane` in state.ts), and each pane shows one of its tabs; the
 * active tab is the one the pane with the keyboard shows. The layout (one or two panes, the
 * split, each pane's tabs) is kept in the app's database as it changes and opened again when
 * the app starts. A reload of the page, which only tests and development do, opens one tab as
 * it always has.
 */
import { createEffect, createSignal, on, onCleanup } from "solid-js";

import { loadColumns } from "./lib/columns";
import { layoutApi } from "./lib/layoutApi";
import { locationOf } from "./lib/locations";
import {
  activateTab,
  addTab,
  activeId,
  activePane,
  activeTab,
  dual,
  isSearch,
  notify,
  openTab,
  refresh,
  revealTab,
  selectInFolder,
  selectionOf,
  setDualPane,
  setFilter,
  setQuery,
  shownTab,
  tabs,
  tabsIn,
} from "./state";

const PANES_DOC = "panes";
/** Set for the life of the page's session, so a reload can be told from a start. */
const SESSION_KEY = "kestrel.started";
const DEFAULT_SPLIT = 0.5;
export const MIN_SPLIT = 0.2;
export const MAX_SPLIT = 0.8;

interface SavedTab {
  path: string;
  /** The search the tab showed, if it showed one. */
  query?: string;
}

interface SavedPane {
  tabs: SavedTab[];
  /** Which of them the pane shows. */
  shown: number;
}

interface SavedLayout {
  dual: boolean;
  split: number;
  /** The pane with the keyboard. */
  active: number;
  /** The first pane, and the second, kept while hidden so it opens on the same tabs again. */
  panes: SavedPane[];
}

const [split, setSplitSignal] = createSignal(DEFAULT_SPLIT);
/** The second pane's tabs while it is hidden. */
let hiddenSecond: SavedTab[] = [];
let lastSaved = "";

export { split };

const clampSplit = (value: number) => Math.min(MAX_SPLIT, Math.max(MIN_SPLIT, value));

function parseLayout(raw: string | undefined): SavedLayout | null {
  try {
    const value = JSON.parse(raw ?? "null") as Partial<SavedLayout> | null;
    if (!value || !Array.isArray(value.panes)) return null;
    const panes = value.panes.slice(0, 2).map((pane) => ({
      tabs: (Array.isArray(pane?.tabs) ? pane.tabs : [])
        .filter((tab): tab is SavedTab => typeof tab?.path === "string" && tab.path !== "")
        .map((tab) => ({ path: tab.path, query: typeof tab.query === "string" && tab.query !== "" ? tab.query : undefined })),
      shown: typeof pane?.shown === "number" ? pane.shown : 0,
    }));
    return {
      dual: value.dual === true,
      split: typeof value.split === "number" && Number.isFinite(value.split) ? clampSplit(value.split) : DEFAULT_SPLIT,
      active: value.active === 1 ? 1 : 0,
      panes,
    };
  } catch {
    return null;
  }
}

/** Whether this page load is the app starting, rather than a reload of its page. */
function starting(): boolean {
  try {
    const started = sessionStorage.getItem(SESSION_KEY) !== null;
    sessionStorage.setItem(SESSION_KEY, "1");
    return !started;
  } catch {
    return true;
  }
}

/** Opens a saved tab in a pane, behind the others; resolves to its id. */
function reopen(saved: SavedTab, pane: number): number {
  const id = addTab(saved.path, pane, false);
  if (saved.query) setQuery(id, saved.query);
  return id;
}

/**
 * Opens the window's tabs as they were when the app last ran: called by `start` (state.ts)
 * once it knows the home folder. Loads the details view's columns too.
 */
export async function openLayout(home: string) {
  const docs = new Map(await layoutApi.load().catch(() => [] as [string, string][]));
  loadColumns(docs);
  const saved = parseLayout(docs.get(PANES_DOC));
  setSplitSignal(saved?.split ?? DEFAULT_SPLIT);
  hiddenSecond = saved && !saved.dual ? (saved.panes[1]?.tabs ?? []) : [];
  if (!starting() || !saved) {
    openTab(home, 0);
    return;
  }
  // Tabs on folders deleted since the last run are left out; a first pane left with none opens
  // at home.
  // Special locations (This PC, the Recycle Bin) are always there.
  const paths = saved.panes.flatMap((pane) => pane.tabs.map((tab) => tab.path)).filter((path) => !locationOf(path));
  const exists = await layoutApi.foldersExist(paths).catch(() => paths.map(() => true));
  const found = new Set(paths.filter((_, i) => exists[i]).map((path) => path.toLowerCase()));
  const kept = (path: string) => locationOf(path) !== null || found.has(path.toLowerCase());
  const existing = (pane: SavedPane | undefined): SavedPane => {
    const tabs = (pane?.tabs ?? []).filter((tab) => kept(tab.path));
    const shownPath = pane?.tabs[pane.shown]?.path;
    return { tabs, shown: Math.max(0, tabs.findIndex((tab) => tab.path === shownPath)) };
  };
  const first = existing(saved.panes[0]);
  const second = existing(saved.panes[1]);
  if (!saved.dual) hiddenSecond = second.tabs;
  if (first.tabs.length === 0) first.tabs.push({ path: home });
  const opened = first.tabs.map((tab) => reopen(tab, 0));
  activateTab(opened[Math.min(first.shown, opened.length - 1)]);
  if (saved.dual && second.tabs.length > 0) {
    setDualPane(true, second.tabs.map((tab) => tab.path));
    const ids = tabsIn(1).map((tab) => tab.id);
    second.tabs.forEach((tab, i) => {
      if (tab.query && ids[i] !== undefined) setQuery(ids[i], tab.query);
    });
    revealTab(ids[Math.min(second.shown, ids.length - 1)]);
    if (saved.active === 1) activateTab(shownTab(1));
  }
}

function snapshot(): SavedLayout {
  const pane = (index: number): SavedPane => {
    const own = tabsIn(index);
    return {
      tabs: own.map((tab) => ({ path: tab.path, query: tab.query.trim() !== "" ? tab.query : undefined })),
      shown: Math.max(0, own.findIndex((tab) => tab.id === shownTab(index))),
    };
  };
  const panes = [pane(0), dual() ? pane(1) : { tabs: hiddenSecond, shown: 0 }];
  return { dual: dual(), split: split(), active: activePane(), panes };
}

/**
 * Keeps the layout in the database as it changes. Call once from a component, which owns the
 * effect.
 */
export function startPanes() {
  let timer: number | undefined;
  createEffect(() => {
    // Read here so the effect follows every part of the layout; saved as it is a moment later.
    snapshot();
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      const text = JSON.stringify(snapshot());
      if (text === lastSaved) return;
      lastSaved = text;
      void layoutApi.save(PANES_DOC, text).catch(() => {});
    }, 200);
  });
  // The second pane's folder is watched while it shows one; nothing is watched without it.
  createEffect(
    on([dual, () => shownTab(1), () => shownTab(0), activeId], () => {
      if (!dual()) return;
      const other = shownTab(activePane() === 0 ? 1 : 0);
      const tab = tabs.find((each) => each.id === other);
      void layoutApi.watchOtherPane(tab && !isSearch(tab) && !locationOf(tab.path) ? tab.path : null).catch(() => {});
    }),
  );
  onCleanup(() => clearTimeout(timer));
}

// ---------- showing, splitting and moving between panes ----------

/** Shows the second pane, on the tabs it had when last hidden, or hides it. */
export function toggleSecondPane() {
  if (dual()) {
    hiddenSecond = tabsIn(1).map((tab) => ({ path: tab.path, query: tab.query.trim() !== "" ? tab.query : undefined }));
    setDualPane(false);
    return;
  }
  const saved = hiddenSecond.filter((tab) => !tab.query);
  setDualPane(true, saved.map((tab) => tab.path));
}

/** Sets where the panes split, as a share of the room for both; `persist` is done by the layout effect. */
export function setSplit(value: number) {
  setSplitSignal(clampSplit(value));
}

export const resetSplit = () => setSplitSignal(DEFAULT_SPLIT);

/** Focus functions for each tab's list, which App.tsx hands over. */
let focusListOf: (tabId: number) => void = () => {};
export const bindListFocus = (focus: (tabId: number) => void) => void (focusListOf = focus);

/** Moves the keyboard to a pane: the tab it shows becomes the active tab, and its list takes focus. */
export function focusPane(pane: number) {
  const target = shownTab(pane);
  if (!tabs.some((tab) => tab.id === target)) return;
  activateTab(target);
  focusListOf(target);
}

/** The folder the pane beside a tab's shows, if it shows a folder rather than search results. */
export function otherPaneFolder(tabId: number): string | null {
  const tab = tabs.find((each) => each.id === tabId);
  if (!dual() || !tab) return null;
  const other = tabs.find((each) => each.id === shownTab(tab.pane === 0 ? 1 : 0));
  return other && !isSearch(other) && other.listing ? other.listing.path : null;
}

/**
 * Copies or moves a tab's selection into the folder the other pane shows, as one undoable
 * operation, and selects the items there.
 */
export async function sendToOtherPane(tabId: number, mode: "copy" | "move") {
  const destination = otherPaneFolder(tabId);
  const selection = selectionOf(tabId);
  if (!destination) return notify("The other pane shows no folder to put them in.", "info");
  if (!selection) return;
  try {
    const names = await layoutApi.transferItems(selection.set, selection.ranges, destination, mode);
    selectInFolder(destination, names);
  } catch (error) {
    notify(error);
  }
  refresh(tabId);
}

/** Opens the filter box of a tab, or puts the keyboard in it if it is open. */
const [filterFocus, setFilterFocus] = createSignal(0);
export { filterFocus };

export function openFilter(tabId: number) {
  const tab = tabs.find((each) => each.id === tabId);
  if (!tab) return;
  if (tab.filter === null) setFilter(tabId, "");
  setFilterFocus((n) => n + 1);
}

/** The keyboard shortcuts of the panes and the filter box. Returns whether `event` was one. */
export function layoutKey(event: KeyboardEvent): boolean {
  const key = event.key.toLowerCase();
  const typing = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement;
  const tab = activeTab();
  if (!tab) return false;
  if (event.ctrlKey && event.shiftKey && !event.altKey && key === "d") {
    toggleSecondPane();
    return true;
  }
  if (event.ctrlKey && event.shiftKey && !event.altKey && key === "l") {
    openFilter(tab.id);
    return true;
  }
  if (!dual()) return false;
  // Tab moves between the panes' lists, as in a two-pane file manager.
  if (key === "tab" && !event.ctrlKey && !event.altKey && (event.target as HTMLElement | null)?.classList?.contains("list-viewport")) {
    focusPane(tab.pane === 0 ? 1 : 0);
    return true;
  }
  if (event.altKey && !event.ctrlKey && !event.shiftKey && !typing && (key === "c" || key === "x")) {
    if ((tab.selected ?? 0) > 0) void sendToOtherPane(tab.id, key === "c" ? "copy" : "move");
    return true;
  }
  return false;
}

// ---------- test hook ----------

// Development builds let the end-to-end tests read the layout as it would be saved, and the
// app start over as if it had just started rather than reloaded. Production builds compile
// this out.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelLayout: {
      snapshot: () => snapshot(),
      /** Forgets that the page has loaded before, so its next load opens the saved layout. */
      forgetSession: () => sessionStorage.removeItem(SESSION_KEY),
      /** Saves the layout now rather than after the usual pause. */
      flush: async () => {
        const text = JSON.stringify(snapshot());
        lastSaved = text;
        await layoutApi.save(PANES_DOC, text);
      },
    },
  });
}
