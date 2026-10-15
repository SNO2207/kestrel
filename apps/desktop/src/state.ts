import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import { getCurrentWindow } from "@tauri-apps/api/window";

import {
  api,
  type IndexStatus,
  type Listing,
  type Marks,
  type Places,
  type Row,
  type RowRange,
  type SortKey,
  type SortSpec,
} from "./lib/api";
import { parentPath } from "./lib/format";
import { Selection } from "./lib/selection";
import { SEARCH_LOCATION, folderKey, locationKey, recall, remember } from "./lib/views";

export interface Tab {
  id: number;
  /** The folder this tab is in. A search runs relative to it and leaves it unchanged. */
  path: string;
  history: string[];
  cursor: number;
  /** Search text; empty while browsing. */
  query: string;
  listing: Listing | null;
  /** Bumped whenever the rows behind `listing` change, including a re-sort of the same set. */
  revision: number;
  sort: SortSpec;
  error: string | null;
  selected: number;
  /** Selection to show when the current rows first appear, after a refresh or re-sort. */
  restore: Marks | null;
  /** Names to select once they appear, because an operation just created them. */
  pending: { names: string[]; rename: boolean } | null;
  /** A name is being edited in place, so the list must not be refreshed underneath it. */
  renaming: boolean;
}

const DEFAULT_SORT: SortSpec = { key: "name", desc: false };
const NOTICE_MS = 6000;
/** Paths are copied as text; beyond this many the clipboard text would be unwieldy. */
const MAX_COPIED_PATHS = 5000;

const [tabs, setTabs] = createStore<Tab[]>([]);
const [activeId, setActiveId] = createSignal(0);
const [status, setStatus] = createSignal<IndexStatus>({
  state: "loading",
  entries: 0,
  refreshing: false,
  generation: 0,
});
const [places, setPlaces] = createSignal<Places>({ home: "", folders: [], drives: [] });
const [everywhere, setEverywhereSignal] = createSignal(localStorage.getItem("search-scope") !== "folder");
const [notice, setNotice] = createSignal<string | null>(null);
/** Whether the notice reports a failure or simply says what happened. */
const [noticeTone, setNoticeTone] = createSignal<"error" | "info">("error");

export { tabs, activeId, status, places, everywhere, notice, noticeTone };

export const activeTab = () => tabs.find((tab) => tab.id === activeId());
export const isSearch = (tab: Tab) => tab.listing?.path === "";

let nextTabId = 1;
let noticeTimer: number | undefined;

/** Shows a short message, such as why an operation failed, that clears itself. */
export function notify(message: unknown, tone: "error" | "info" = "error") {
  setNoticeTone(tone);
  setNotice(String(message));
  clearTimeout(noticeTimer);
  noticeTimer = window.setTimeout(() => setNotice(null), NOTICE_MS);
}

export const dismissNotice = () => setNotice(null);

/**
 * Each tab's selection, kept outside the store because only its size is ever rendered.
 * `version` changes with every change, which is how an async step notices that the user
 * selected something else while it was waiting.
 */
const selections = new Map<number, { selection: Selection; focus: number; version: number }>();
const NO_MARKS: Marks = { ranges: [], focus: null };

/**
 * Loads for one tab run one at a time, and only the most recent request waiting behind the
 * running one is kept. Typing quickly therefore never shows results out of order.
 */
const queues = new Map<number, { running: boolean; next: (() => Promise<void>) | null }>();

function enqueue(tabId: number, job: () => Promise<void>) {
  const queue = queues.get(tabId) ?? { running: false, next: null };
  queues.set(tabId, queue);
  if (queue.running) {
    queue.next = job;
    return;
  }
  queue.running = true;
  void (async () => {
    let current: (() => Promise<void>) | null = job;
    while (current) {
      await current().catch((error) => console.error(error));
      current = queue.next;
      queue.next = null;
    }
    queue.running = false;
  })();
}

const find = (tabId: number) => tabs.find((tab) => tab.id === tabId);
const patch = (tabId: number, change: (tab: Tab) => void) =>
  setTabs((tab) => tab.id === tabId, produce(change));
const samePath = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/** The tab's selected and focused rows, or `null` when there are none. */
function marksOf(tabId: number): Marks | null {
  const held = selections.get(tabId);
  if (!held || (held.selection.count === 0 && held.focus < 0)) return null;
  return { ranges: held.selection.ranges as RowRange[], focus: held.focus >= 0 ? held.focus : null };
}

/**
 * Works out where the tab's selection sits in `listing`, which shows the same files as the
 * listing on screen. The selection is read when the new listing has arrived, not when it was
 * requested, and re-read if the user changes it meanwhile, so a click made during a
 * background refresh is never lost.
 */
async function carryMarks(tabId: number, listing: Listing): Promise<Marks> {
  const previous = find(tabId)?.listing?.set;
  for (;;) {
    const marks = marksOf(tabId);
    if (previous === undefined || !marks) return NO_MARKS;
    const version = selections.get(tabId)?.version;
    const carried = await api.carryMarks(previous, listing.set, marks);
    if (selections.get(tabId)?.version === version) return carried;
  }
}

/** Shows a new listing in the tab and frees the one it replaces. */
function show(tabId: number, listing: Listing, restore: Marks, change: (tab: Tab) => void = () => {}) {
  const previous = find(tabId)?.listing?.set;
  patch(tabId, (tab) => {
    change(tab);
    tab.listing = listing;
    tab.restore = restore;
    tab.revision += 1;
    tab.error = null;
  });
  // Released only now that the tab shows the new listing, so a late row request for the old
  // one is simply ignored rather than read as a stale index.
  if (previous !== undefined) void api.releaseSet(previous);
}

type NavigateMode = "push" | "keep";

/** Shows `path` in the tab. `push` records it in the tab's history; `keep` leaves history alone. */
export function navigate(tabId: number, path: string, mode: NavigateMode = "push") {
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab) return;
    try {
      // Re-listing the folder already on screen is a refresh, which keeps the selection.
      const refreshing = !isSearch(tab) && tab.listing !== null && samePath(path, tab.path);
      // Each folder keeps the sort it was last given.
      const sort = { ...(recall(folderKey(path)).sort ?? DEFAULT_SORT) };
      // Only the tab on screen is watched for changes; others catch up when activated.
      const listing = await api.listDir(path, sort, tabId === activeId());
      const restore = refreshing ? await carryMarks(tabId, listing) : NO_MARKS;
      show(tabId, listing, restore, (tab) => {
        tab.sort = sort;
        if (mode === "push" && tab.history[tab.cursor] !== listing.path) {
          tab.history.splice(tab.cursor + 1);
          tab.history.push(listing.path);
          tab.cursor = tab.history.length - 1;
        }
        if (!samePath(tab.path, listing.path)) tab.pending = null;
        tab.path = listing.path;
        tab.query = "";
      });
    } catch (error) {
      patch(tabId, (tab) => void (tab.error = String(error)));
    }
  });
}

function runSearch(tabId: number, keepSelection = false) {
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab || tab.query.trim() === "") return;
    try {
      const scope = everywhere() ? null : tab.path;
      // Search results keep a sort of their own, separate from any folder's.
      const sort = { ...(recall(SEARCH_LOCATION).sort ?? DEFAULT_SORT) };
      const listing = await api.search(tab.query, scope, sort);
      show(tabId, listing, keepSelection ? await carryMarks(tabId, listing) : NO_MARKS, (tab) => void (tab.sort = sort));
    } catch (error) {
      patch(tabId, (tab) => void (tab.error = String(error)));
    }
  });
}

/** Sets the tab's search text. Clearing it returns to the folder. */
export function setQuery(tabId: number, text: string) {
  const tab = find(tabId);
  if (!tab) return;
  if (text.trim() === "") {
    const wasSearching = isSearch(tab) || tab.query !== "";
    patch(tabId, (tab) => void (tab.query = ""));
    if (wasSearching) navigate(tabId, tab.path, "keep");
    return;
  }
  patch(tabId, (tab) => void (tab.query = text));
  runSearch(tabId);
}

export function refresh(tabId: number) {
  const tab = find(tabId);
  if (!tab) return;
  if (tab.query.trim() !== "") runSearch(tabId, true);
  else navigate(tabId, tab.path, "keep");
}

export function setEverywhere(value: boolean) {
  setEverywhereSignal(value);
  localStorage.setItem("search-scope", value ? "everywhere" : "folder");
  const tab = activeTab();
  if (tab && tab.query.trim() !== "") runSearch(tab.id);
}

export function goBack(tabId: number) {
  const tab = find(tabId);
  if (!tab) return;
  // Leaving a search counts as going back to the folder it was started from.
  if (isSearch(tab)) return navigate(tabId, tab.path, "keep");
  if (tab.cursor > 0) stepHistory(tab, -1);
}

export function goForward(tabId: number) {
  const tab = find(tabId);
  if (tab && tab.cursor < tab.history.length - 1) stepHistory(tab, 1);
}

function stepHistory(tab: Tab, step: 1 | -1) {
  const cursor = tab.cursor + step;
  const path = tab.history[cursor];
  patch(tab.id, (tab) => void (tab.cursor = cursor));
  navigate(tab.id, path, "keep");
}

export function goUp(tabId: number) {
  const tab = find(tabId);
  const parent = tab && parentPath(tab.path);
  if (parent) navigate(tabId, parent);
}

export const canGoBack = (tab: Tab) => isSearch(tab) || tab.cursor > 0;
export const canGoForward = (tab: Tab) => tab.cursor < tab.history.length - 1;
export const canGoUp = (tab: Tab) => parentPath(tab.path) !== null;

/** Sizes and dates start with the largest and newest, as Explorer does. */
export const startsDescending = (key: SortKey) => key === "size" || key === "modified";

/** Sorts by `key`, flipping direction if it is already the sort column. */
export function setSort(tabId: number, key: SortKey) {
  const tab = find(tabId);
  if (!tab) return;
  setSortSpec(tabId, { key, desc: tab.sort.key === key ? !tab.sort.desc : startsDescending(key) });
}

/** Sorts the tab's rows, remembering the order for its folder (or for search results). */
export function setSortSpec(tabId: number, sort: SortSpec) {
  const tab = find(tabId);
  if (!tab) return;
  remember(locationKey(tab), { sort });
  patch(tabId, (tab) => void (tab.sort = sort));
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab?.listing) return;
    try {
      const moved = await api.sortSet(tab.listing.set, sort, marksOf(tabId) ?? NO_MARKS);
      patch(tabId, (tab) => {
        // A listing that arrived while this waited set the sort it was made with; the rows
        // are now in this order instead.
        tab.sort = sort;
        tab.restore = moved;
        tab.revision += 1;
      });
    } catch {
      refresh(tabId);
    }
  });
}

/** Called by a tab's list whenever its selection or focused row changes. */
export function reportSelection(tabId: number, selection: Selection, focus: number) {
  selections.set(tabId, { selection, focus, version: (selections.get(tabId)?.version ?? 0) + 1 });
  if (find(tabId)?.selected !== selection.count) patch(tabId, (tab) => void (tab.selected = selection.count));
}

export const setRenaming = (tabId: number, active: boolean) =>
  patch(tabId, (tab) => void (tab.renaming = active));

export const clearPending = (tabId: number) => patch(tabId, (tab) => void (tab.pending = null));

/** Selects the items with these names once the tab's listing shows them. */
export const selectWhenListed = (tabId: number, names: string[]) =>
  patch(tabId, (tab) => void (tab.pending = names.length > 0 ? { names, rename: false } : null));

// ---------- file operations ----------

/** The selection of a tab that is showing rows, or `null` if nothing is selected. */
function selected(tabId: number) {
  const tab = find(tabId);
  const held = selections.get(tabId);
  if (!tab?.listing || !held || held.selection.count === 0) return null;
  return { tab, set: tab.listing.set, selection: held.selection, ranges: held.selection.ranges as RowRange[] };
}

/** Fetches the selected rows, lowest first, up to `limit`. */
export async function selectedRows(tabId: number, limit: number): Promise<Row[]> {
  const current = selected(tabId);
  if (!current) return [];
  const rows: Row[] = [];
  for (const [start, end] of current.ranges) {
    if (rows.length >= limit) break;
    rows.push(...(await api.rows(current.set, start, Math.min(end - start + 1, limit - rows.length))));
  }
  return rows;
}

/** Runs an operation, reports any failure, and refreshes the tab either way. */
async function operate(tabId: number, run: () => Promise<void>) {
  try {
    await run();
  } catch (error) {
    notify(error);
  }
  refresh(tabId);
}

/** Sends the selection to the Recycle Bin, or erases it for good when `permanent`. */
export function deleteSelection(tabId: number, permanent: boolean) {
  const current = selected(tabId);
  if (!current) return;
  void operate(tabId, async () => void (await api.deleteItems(current.set, current.ranges, permanent)));
}

export async function copySelection(tabId: number, cut: boolean) {
  const current = selected(tabId);
  if (!current) return;
  await api.copyItems(current.set, current.ranges, cut).catch(notify);
}

export function paste(tabId: number) {
  const tab = find(tabId);
  if (!tab || isSearch(tab)) return;
  void operate(tabId, async () => {
    const names = await api.pasteItems(tab.path);
    if (names.length > 0) patch(tabId, (tab) => void (tab.pending = { names, rename: false }));
  });
}

/**
 * Refreshes every tab showing `folder` and selects `names` there once they appear, as paste
 * does. Used after files are dropped into a folder, which may be shown in any tab or none.
 */
export function selectInFolder(folder: string, names: string[]) {
  for (const tab of tabs) {
    if (isSearch(tab) || tab.renaming || !samePath(tab.path, folder)) continue;
    if (names.length > 0) patch(tab.id, (tab) => void (tab.pending = { names, rename: false }));
    refresh(tab.id);
  }
}

/** Creates a folder here and starts renaming it, as Explorer does. */
export function createFolder(tabId: number) {
  const tab = find(tabId);
  if (!tab || isSearch(tab)) return;
  void operate(tabId, async () => {
    const name = await api.newFolder(tab.path);
    patch(tabId, (tab) => void (tab.pending = { names: [name], rename: true }));
  });
}

export function rename(tabId: number, row: Row, name: string) {
  void operate(tabId, async () => {
    // The shell may settle on another name, such as "name (2)" after a conflict.
    const renamed = await api.renameItem(row.path, name);
    if (renamed) patch(tabId, (tab) => void (tab.pending = { names: [renamed], rename: false }));
  });
}

/** Copies the selected items' full paths as text, quoted the way Explorer's "Copy as path" does. */
export async function copyPaths(tabId: number) {
  const rows = await selectedRows(tabId, MAX_COPIED_PATHS);
  if (rows.length === 0) return;
  await navigator.clipboard.writeText(rows.map((row) => `"${row.path}"`).join("\r\n")).catch(notify);
}

/** Opens the Windows Properties dialog for the selected item, or for the folder itself. */
export async function showProperties(tabId: number, ofFolder: boolean) {
  const tab = find(tabId);
  if (!tab) return;
  const path = ofFolder ? (isSearch(tab) ? null : tab.path) : (await selectedRows(tabId, 1))[0]?.path;
  if (path) await api.showProperties(path).catch(notify);
}

// ---------- tabs ----------

export function openTab(path?: string) {
  const id = nextTabId++;
  const start = path ?? activeTab()?.path ?? places().home;
  setTabs(tabs.length, {
    id,
    path: start,
    history: [],
    cursor: -1,
    query: "",
    listing: null,
    revision: 0,
    sort: { ...DEFAULT_SORT },
    error: null,
    selected: 0,
    restore: null,
    pending: null,
    renaming: false,
  });
  setActiveId(id);
  navigate(id, start);
}

export function closeTab(tabId: number) {
  if (tabs.length === 1) {
    void getCurrentWindow().close();
    return;
  }
  const position = tabs.findIndex((tab) => tab.id === tabId);
  if (activeId() === tabId) activateTab(tabs[position === 0 ? 1 : position - 1].id);
  const set = find(tabId)?.listing?.set;
  setTabs((all) => all.filter((tab) => tab.id !== tabId));
  queues.delete(tabId);
  selections.delete(tabId);
  if (set !== undefined) void api.releaseSet(set);
}

/** Switches to a tab, bringing it up to date: only the tab on screen is watched for changes. */
export function activateTab(tabId: number) {
  if (activeId() === tabId) return;
  setActiveId(tabId);
  const tab = find(tabId);
  if (tab && !isSearch(tab) && !tab.renaming) refresh(tabId);
}

/** Moves to the next or previous tab, wrapping around. */
export function cycleTab(step: 1 | -1) {
  const position = tabs.findIndex((tab) => tab.id === activeId());
  activateTab(tabs[(position + step + tabs.length) % tabs.length].id);
}

export async function start() {
  const [initialStatus, initialPlaces] = await Promise.all([api.indexStatus(), api.places()]);
  setStatus(initialStatus);
  setPlaces(initialPlaces);
  await api.onIndexStatus((next) => {
    const replaced = next.generation !== status().generation;
    setStatus(next);
    // A new index invalidates every result set, so each tab reloads what it was showing.
    if (replaced) tabs.forEach((tab) => refresh(tab.id));
  });
  await api.onDirChanged((path) => {
    for (const tab of tabs) {
      if (!isSearch(tab) && !tab.renaming && samePath(tab.path, path)) refresh(tab.id);
    }
  });
  openTab(initialPlaces.home);
}
