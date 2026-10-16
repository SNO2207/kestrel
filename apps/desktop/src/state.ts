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
import { CANCELLED, archiveApi, nextJobId } from "./lib/archivesApi";
import { layoutApi, type GroupSpec } from "./lib/layoutApi";
import { locationOf, parentOf, resolveLocation } from "./lib/locations";
import { Selection } from "./lib/selection";
import { SEARCH_LOCATION, folderKey, groupAt, listingLocation, locationKey, recall, remember } from "./lib/views";

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
  /**
   * The folder a navigation failed to reach, which `error` is about; the tab is still in
   * `path`, and going back shows it again.
   */
  failed: string | null;
  selected: number;
  /** Selection to show when the current rows first appear, after a refresh or re-sort. */
  restore: Marks | null;
  /** Names to select once they appear, because an operation just created them. */
  pending: { names: string[]; rename: boolean } | null;
  /** A name is being edited in place, so the list must not be refreshed underneath it. */
  renaming: boolean;
  /** The pane the tab belongs to: 0, or 1 for the second pane (panes.ts). */
  pane: number;
  /** The filter box's text, or null while the box is closed (FilterBar.tsx). */
  filter: string | null;
  /** The listing as it was before the filter narrowed it, while one does. */
  unfiltered: Listing | null;
  /** Where each group of the listing starts, while it is grouped. */
  groups: number[] | null;
}

const DEFAULT_SORT: SortSpec = { key: "name", desc: false };
const NOTICE_MS = 6000;
/** Paths are copied as text; beyond this many the clipboard text would be unwieldy. */
const MAX_COPIED_PATHS = 5000;

const [tabs, setTabs] = createStore<Tab[]>([]);
const [activeId, setActiveId] = createSignal(0);
/**
 * Whether the second pane is shown, and the tab each pane shows. The active tab is always the
 * one its pane shows; the other pane's tab is on screen too, and watched (panes.ts).
 */
const [dual, setDual] = createSignal(false);
const [paneTabs, setPaneTabs] = createSignal<readonly [number, number]>([0, 0]);
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

export { tabs, activeId, status, places, everywhere, notice, noticeTone, dual };

export const activeTab = () => tabs.find((tab) => tab.id === activeId());
export const isSearch = (tab: Tab) => tab.listing?.path === "";
/** The pane the active tab is in. */
export const activePane = () => activeTab()?.pane ?? 0;
/** The tab a pane shows. */
export const shownTab = (pane: number) => paneTabs()[pane === 1 ? 1 : 0];
/** Whether a tab is on screen: the active one, or the one the other pane shows. */
export const onScreen = (tabId: number) => tabId === activeId() || (dual() && paneTabs().includes(tabId));

function showInPane(pane: number, tabId: number) {
  setPaneTabs((current) => (pane === 1 ? [current[0], tabId] : [tabId, current[1]]));
}

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
const selections = new Map<number, { selection: Selection; focus: number; version: number; set?: number }>();
const NO_MARKS: Marks = { ranges: [], focus: null };

/**
 * Sets a tab has stopped showing but its list may still draw: the list keeps the rows it has
 * until the replacement's rows for the view have arrived (FileList.tsx), and its selection is
 * of the set it draws, so commands meanwhile still act on that set. Released once the list
 * reports the new set on screen (`listShows`).
 */
const retiring = new Map<number, Set<number>>();

/** The set a tab's selection refers to: the one its list draws. */
const selectionSet = (tabId: number) => selections.get(tabId)?.set ?? find(tabId)?.listing?.set;

/** A tab's list now draws `set`: the sets it drew before can go. */
export function listShows(tabId: number, set: number | undefined) {
  const old = retiring.get(tabId);
  if (!old) return;
  const keep = new Set([set, find(tabId)?.listing?.set, find(tabId)?.unfiltered?.set]);
  for (const each of old) {
    if (keep.has(each)) continue;
    old.delete(each);
    void api.releaseSet(each);
  }
  if (old.size === 0) retiring.delete(tabId);
}

function retire(tabId: number, set: number) {
  const old = retiring.get(tabId) ?? new Set<number>();
  old.add(set);
  retiring.set(tabId, old);
}

/**
 * Loads for one tab run one at a time, and only the most recent request waiting behind the
 * running one is kept. Typing quickly therefore never shows results out of order.
 */
const queues = new Map<number, { running: boolean; next: (() => Promise<void>) | null }>();

/**
 * Tabs whose refresh came back while a name was being edited in them. Showing it would end
 * the edit (the list redraws its rows), so it is set aside and the tab is listed again once the
 * edit is over. No refresh starts during an edit, but one already on its way still arrives: F2
 * just after a file operation, or the rename that follows "New folder", met one regularly.
 */
const refreshAfterRename = new Set<number>();

/** Whether a refresh that just came back must wait for an edit in the tab to end. */
function heldForRename(tabId: number, arranged: Arranged): boolean {
  if (!find(tabId)?.renaming) return false;
  void api.releaseSet(arranged.listing.set);
  if (arranged.unfiltered) void api.releaseSet(arranged.unfiltered.set);
  refreshAfterRename.add(tabId);
  return true;
}

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

/**
 * The tab's selected and focused rows, or `null` when there are none, or when they are rows of
 * another set than `set` (the list still draws the one before it).
 */
function marksOf(tabId: number, set?: number): Marks | null {
  const held = selections.get(tabId);
  if (!held || (held.selection.count === 0 && held.focus < 0)) return null;
  if (set !== undefined && held.set !== undefined && held.set !== set) return null;
  return { ranges: held.selection.ranges as RowRange[], focus: held.focus >= 0 ? held.focus : null };
}

/**
 * Works out where the tab's selection sits in `listing`, which shows the same files as the
 * listing on screen. The selection is read when the new listing has arrived, not when it was
 * requested, and re-read if the user changes it meanwhile, so a click made during a
 * background refresh is never lost.
 */
async function carryMarks(tabId: number, listing: Listing): Promise<Marks> {
  // The set the selection is of: the one the list draws, which may be older than the tab's.
  const previous = selectionSet(tabId);
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
  const previousBase = find(tabId)?.unfiltered?.set;
  patch(tabId, (tab) => {
    change(tab);
    tab.listing = listing;
    tab.restore = restore;
    tab.revision += 1;
    tab.error = null;
    tab.failed = null;
  });
  // Released only once the tab's list draws the new listing (`listShows`), so a late row
  // request for the old one is simply ignored rather than read as a stale index, and a command
  // on the selection the list still shows acts on the rows it shows.
  if (previous !== undefined && previous !== listing.set) retire(tabId, previous);
  // The listing a filter narrowed goes too, once nothing is narrowed from it any more.
  const base = find(tabId)?.unfiltered?.set;
  if (previousBase !== undefined && previousBase !== base && previousBase !== listing.set) retire(tabId, previousBase);
}

/** A listing as the tab shows it: grouped as its location is, and narrowed by the tab's filter. */
interface Arranged {
  listing: Listing;
  unfiltered: Listing | null;
  groups: number[] | null;
}

/**
 * Groups a new listing as its folder (or search results) remembers, and narrows it by the
 * tab's filter, before it is shown; see `layout.rs`.
 */
async function arrange(tabId: number, listing: Listing, sort: SortSpec): Promise<Arranged> {
  const group = groupAt(listingLocation(listing));
  const filter = find(tabId)?.filter?.trim() ?? "";
  try {
    if (filter !== "") {
      const narrowed = await layoutApi.filterSet(listing.set, filter, sort, group);
      // A narrowed archive folder is still read-only (`Listing.archive`).
      const shown = { ...narrowed.listing, path: listing.path, archive: listing.archive, took_us: listing.took_us + narrowed.listing.took_us };
      return { listing: shown, unfiltered: listing, groups: group ? narrowed.starts : null };
    }
    if (!group) return { listing, unfiltered: null, groups: null };
    const grouped = await layoutApi.groupSet(listing.set, sort, group, NO_MARKS);
    return { listing, unfiltered: null, groups: grouped.starts };
  } catch (error) {
    // The listing will never be shown.
    void api.releaseSet(listing.set);
    throw error;
  }
}

/**
 * Lists a folder for a tab, watching it when the tab is on screen: the active tab's folder by
 * `list_dir` itself, the other pane's by a watch of its own, armed first.
 */
async function listFolder(tabId: number, path: string, sort: SortSpec): Promise<Listing> {
  if (tabId !== activeId() && onScreen(tabId)) await layoutApi.watchOtherPane(path).catch(() => {});
  const ticket = nextJobId();
  listings.set(tabId, { ticket, path });
  try {
    return await api.listDir(path, sort, tabId === activeId(), undefined, ticket);
  } finally {
    if (listings.get(tabId)?.ticket === ticket) listings.delete(tabId);
  }
}

/**
 * Each tab's listing in progress. Listing a large compressed archive means reading it through,
 * which takes minutes, and the tab's other loads wait behind it: going elsewhere cancels it.
 */
const listings = new Map<number, { ticket: number; path: string }>();

/** Cancels the tab's listing in progress, unless it is of `unless`, which is wanted again. */
function cancelListing(tabId: number, unless?: string) {
  const listing = listings.get(tabId);
  if (!listing || (unless !== undefined && samePath(listing.path, unless))) return;
  listings.delete(tabId);
  void archiveApi.cancel(listing.ticket).catch(() => {});
}

type NavigateMode = "push" | "keep";

/**
 * Shows a special location (lib/locations.ts) in the tab: its page takes the list's place, so
 * the tab keeps no listing. Showing the one already there is a refresh, which the page sees
 * as a new revision.
 */
function showLocation(tabId: number, address: string, mode: NavigateMode) {
  const previous = find(tabId)?.listing?.set;
  const previousBase = find(tabId)?.unfiltered?.set;
  patch(tabId, (tab) => {
    if (mode === "push" && tab.history[tab.cursor] !== address) {
      tab.history.splice(tab.cursor + 1);
      tab.history.push(address);
      tab.cursor = tab.history.length - 1;
    }
    tab.path = address;
    tab.query = "";
    tab.listing = null;
    tab.restore = null;
    tab.pending = null;
    tab.selected = 0;
    tab.revision += 1;
    tab.error = null;
    tab.failed = null;
    // A page has no rows to group or narrow.
    tab.filter = null;
    tab.unfiltered = null;
    tab.groups = null;
  });
  if (previous !== undefined) void api.releaseSet(previous);
  if (previousBase !== undefined && previousBase !== previous) void api.releaseSet(previousBase);
}

/** Shows `path` in the tab. `push` records it in the tab's history; `keep` leaves history alone. */
export function navigate(tabId: number, path: string, mode: NavigateMode = "push") {
  // A listing still running for somewhere else is no longer wanted.
  cancelListing(tabId, path);
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab) return;
    const location = resolveLocation(path);
    if (location !== null) return showLocation(tabId, location, mode);
    try {
      // Re-listing the folder already on screen is a refresh, which keeps the selection.
      const refreshing = !isSearch(tab) && tab.listing !== null && samePath(path, tab.path);
      // The filter narrows the folder it was typed in; another folder opens unfiltered.
      if (!samePath(path, tab.path) && tab.filter !== null) patch(tabId, (tab) => void (tab.filter = null));
      // Each folder keeps the sort it was last given.
      const sort = { ...(recall(folderKey(path)).sort ?? DEFAULT_SORT) };
      // Only the tabs on screen are watched for changes; others catch up when activated.
      const shown = await arrange(tabId, await listFolder(tabId, path, sort), sort);
      if (refreshing && heldForRename(tabId, shown)) return;
      const listing = shown.listing;
      const restore = refreshing ? await carryMarks(tabId, listing) : NO_MARKS;
      if (refreshing && heldForRename(tabId, shown)) return;
      show(tabId, listing, restore, (tab) => {
        tab.unfiltered = shown.unfiltered;
        tab.groups = shown.groups;
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
      // Cancelled because the tab went elsewhere, which is loading next.
      if (String(error) === CANCELLED) return;
      patch(tabId, (tab) => {
        tab.error = String(error);
        tab.failed = samePath(path, tab.path) ? null : path;
      });
    }
  });
}

function runSearch(tabId: number, keepSelection = false) {
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab || tab.query.trim() === "") return;
    try {
      // A special location has no folder to search beneath, so its searches go everywhere.
      const scope = everywhere() || locationOf(tab.path) ? null : tab.path;
      // Search results keep a sort of their own, separate from any folder's.
      const sort = { ...(recall(SEARCH_LOCATION).sort ?? DEFAULT_SORT) };
      const shown = await arrange(tabId, await api.search(tab.query, scope, sort), sort);
      if (keepSelection && heldForRename(tabId, shown)) return;
      const listing = shown.listing;
      const restore = keepSelection ? await carryMarks(tabId, listing) : NO_MARKS;
      if (keepSelection && heldForRename(tabId, shown)) return;
      show(tabId, listing, restore, (tab) => {
        tab.unfiltered = shown.unfiltered;
        tab.groups = shown.groups;
        tab.sort = sort;
      });
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

/**
 * Searches for `query` beneath the folder `scope`, or everywhere when it is null, as a saved
 * search does. The tab moves to that folder, so leaving the search lands there.
 */
export function openSearch(tabId: number, query: string, scope: string | null) {
  if (!find(tabId) || query.trim() === "") return;
  setEverywhereSignal(scope === null);
  localStorage.setItem("search-scope", scope === null ? "everywhere" : "folder");
  patch(tabId, (tab) => {
    tab.query = query;
    if (scope !== null && !samePath(tab.path, scope)) {
      tab.history.splice(tab.cursor + 1);
      tab.history.push(scope);
      tab.cursor = tab.history.length - 1;
      tab.path = scope;
      tab.pending = null;
    }
  });
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
  // Leaving a search counts as going back to the folder it was started from, and leaving the
  // message that a folder can't be opened, as going back to the folder the tab is still in.
  if (isSearch(tab) || tab.failed !== null) return navigate(tabId, tab.path, "keep");
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
  // A drive's parent is This PC, as in Explorer (lib/locations.ts).
  const parent = tab && parentOf(tab.path);
  if (parent) navigate(tabId, parent);
}

export const canGoBack = (tab: Tab) => isSearch(tab) || tab.failed !== null || tab.cursor > 0;
export const canGoForward = (tab: Tab) => tab.cursor < tab.history.length - 1;
export const canGoUp = (tab: Tab) => parentOf(tab.path) !== null;

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
      const moved = await api.sortSet(tab.listing.set, sort, marksOf(tabId, tab.listing.set) ?? NO_MARKS);
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

/**
 * Groups the tab's rows (Explorer's "Group by"), or ungroups them with `null`, remembering it
 * for the folder, or for search results, as the view and sort are.
 */
export function setGrouping(tabId: number, group: GroupSpec | null) {
  const tab = find(tabId);
  if (!tab) return;
  remember(locationKey(tab), { group });
  enqueue(tabId, async () => {
    const tab = find(tabId);
    if (!tab?.listing) return;
    try {
      const grouped = await layoutApi.groupSet(tab.listing.set, tab.sort, group, marksOf(tabId, tab.listing.set) ?? NO_MARKS);
      patch(tabId, (tab) => {
        tab.groups = group ? grouped.starts : null;
        tab.restore = grouped.marks;
        tab.revision += 1;
      });
    } catch {
      refresh(tabId);
    }
  });
}

/**
 * Opens the filter box with `text`, narrowing the tab's listing to the names that contain it
 * (or match it, with `*` and `?`), or closes it with `null`. The selection follows its items.
 */
export function setFilter(tabId: number, text: string | null) {
  const tab = find(tabId);
  if (!tab || tab.filter === text) return;
  const wasNarrowed = tab.unfiltered !== null;
  patch(tabId, (tab) => void (tab.filter = text));
  if (text === null || text.trim() === "") {
    // Back to everything: listed again, so the order is the one the tab sorts by now.
    if (wasNarrowed) refresh(tabId);
    return;
  }
  enqueue(tabId, async () => {
    const tab = find(tabId);
    const base = tab?.unfiltered ?? tab?.listing;
    if (!tab || !base || tab.filter === null) return;
    try {
      const group = groupAt(listingLocation(base));
      const narrowed = await layoutApi.filterSet(base.set, tab.filter, tab.sort, group);
      const listing = { ...narrowed.listing, path: base.path, archive: base.archive, took_us: narrowed.listing.took_us };
      show(tabId, listing, await carryMarks(tabId, listing), (tab) => {
        tab.unfiltered = base;
        tab.groups = group ? narrowed.starts : null;
      });
    } catch {
      refresh(tabId);
    }
  });
}

/**
 * Called by a tab's list whenever its selection or focused row changes, or it draws another set:
 * `set` is the set the rows it draws (and so the selection) belong to.
 */
export function reportSelection(tabId: number, selection: Selection, focus: number, set?: number) {
  selections.set(tabId, { selection, focus, set, version: (selections.get(tabId)?.version ?? 0) + 1 });
  if (find(tabId)?.selected !== selection.count) patch(tabId, (tab) => void (tab.selected = selection.count));
}

export function setRenaming(tabId: number, active: boolean) {
  patch(tabId, (tab) => void (tab.renaming = active));
  // A refresh set aside during the edit (see `heldForRename`) is due now.
  if (!active && refreshAfterRename.delete(tabId)) refresh(tabId);
}

export const clearPending = (tabId: number) => patch(tabId, (tab) => void (tab.pending = null));

/** Selects the items with these names once the tab's listing shows them. */
export const selectWhenListed = (tabId: number, names: string[]) =>
  patch(tabId, (tab) => void (tab.pending = names.length > 0 ? { names, rename: false } : null));

/** Selects the item called `name` once the tab's listing shows it, and starts renaming it. */
export const renameWhenListed = (tabId: number, name: string) =>
  patch(tabId, (tab) => void (tab.pending = { names: [name], rename: true }));

// ---------- file operations ----------

/** The selection of a tab that is showing rows, or `null` if nothing is selected. */
function selected(tabId: number) {
  const tab = find(tabId);
  const held = selections.get(tabId);
  if (!tab?.listing || !held || held.selection.count === 0) return null;
  // The rows the list draws, which for a moment after a refresh may be the previous set's.
  return { tab, set: held.set ?? tab.listing.set, selection: held.selection, ranges: held.selection.ranges as RowRange[] };
}

/** The set and ranges of a tab's selection, for commands that act on it, or `null` if none. */
export function selectionOf(tabId: number): { set: number; ranges: RowRange[] } | null {
  const current = selected(tabId);
  return current && { set: current.set, ranges: current.ranges };
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
  // A special location (Home, This PC, the Recycle Bin) is no folder to make things in.
  if (!tab || isSearch(tab) || locationOf(tab.path)) return;
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
    if (isSearch(tab) || !samePath(tab.path, folder)) continue;
    if (tab.renaming) {
      refreshAfterRename.add(tab.id);
      continue;
    }
    if (names.length > 0) patch(tab.id, (tab) => void (tab.pending = { names, rename: false }));
    refresh(tab.id);
  }
}

/** Creates a folder here and starts renaming it, as Explorer does. */
export function createFolder(tabId: number) {
  const tab = find(tabId);
  // A special location (Home, This PC, the Recycle Bin) is no folder to make things in.
  if (!tab || isSearch(tab) || locationOf(tab.path)) return;
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

/**
 * Shows the Windows context menu, with every shell extension's commands, for the selection
 * or, when `onItems` is false, for the folder itself. `x` and `y` are where in the window.
 * The tab refreshes once the chosen command is done.
 */
export function showShellMenu(tabId: number, x: number, y: number, onItems: boolean, extended: boolean) {
  const tab = find(tabId);
  const current = onItems ? selected(tabId) : null;
  if (!tab || (onItems ? !current : isSearch(tab))) return;
  // The menu matches the window, which is dark unless the system asks for light (styles.css).
  const dark = !window.matchMedia("(prefers-color-scheme: light)").matches;
  void operate(tabId, async () => {
    const choice = current
      ? await api.shellMenu(current.set, current.ranges, x, y, dark, extended)
      : await api.shellFolderMenu(tab.path, x, y, dark, extended);
    // Rename edits the name in place, which only Kestrel can do.
    if (choice.verb === "rename" && !choice.invoked && current?.selection.count === 1) {
      const [row] = await selectedRows(tabId, 1);
      if (row) patch(tabId, (tab) => void (tab.pending = { names: [row.name], rename: true }));
    }
  });
}

/** Shows Windows' Open With dialog for the selected file. */
export async function openWith(tabId: number) {
  const [row] = await selectedRows(tabId, 1);
  if (row && !row.dir) void operate(tabId, async () => api.openWith(row.path));
}

// ---------- tabs ----------

/**
 * Opens a tab on `path` (the active tab's folder by default) in a pane, the active one unless
 * given, and makes it the active tab unless `activate` is false; then it is only the tab its
 * pane shows.
 */
export function openTab(path?: string, pane: number = activePane(), activate = true) {
  addTab(path, pane, activate);
}

/** `openTab`, resolving to the new tab's id. */
export function addTab(path?: string, pane: number = activePane(), activate = true): number {
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
    failed: null,
    selected: 0,
    restore: null,
    pending: null,
    renaming: false,
    pane,
    filter: null,
    unfiltered: null,
    groups: null,
  });
  // A tab opened behind the others shows only in a pane that had none to show.
  const shown = shownTab(pane);
  if (activate || !tabs.some((tab) => tab.id === shown && tab.pane === pane)) showInPane(pane, id);
  if (activate) setActiveId(id);
  navigate(id, start);
  return id;
}

/**
 * Opens a tab that brings its own history: a duplicate (beside the tab `after`) or a tab moved
 * here from another window (parity2.ts). It becomes the active tab.
 */
export function addTabWith(source: { path: string; history: string[]; cursor: number }, pane: number = activePane(), after?: number): number {
  const id = addTab(source.path, pane);
  const history = source.history.length > 0 ? [...source.history] : [];
  const cursor = Math.min(Math.max(source.cursor, -1), history.length - 1);
  setTabs(
    produce((all) => {
      const at = all.findIndex((tab) => tab.id === id);
      if (at < 0) return;
      const [tab] = all.splice(at, 1);
      // Its own history, which navigating to where it already is leaves as it is.
      if (history.length > 0 && history[cursor]?.toLowerCase() === source.path.toLowerCase()) {
        tab.history = history;
        tab.cursor = cursor;
      }
      const beside = after === undefined ? -1 : all.findIndex((other) => other.id === after);
      all.splice(beside < 0 ? all.length : beside + 1, 0, tab);
    }),
  );
  return id;
}

/** Makes a tab the one its pane shows, without moving the keyboard to that pane. */
export function revealTab(tabId: number) {
  const tab = find(tabId);
  if (!tab || shownTab(tab.pane) === tabId) return;
  if (tab.pane === activePane()) return activateTab(tabId);
  showInPane(tab.pane, tabId);
  if (!isSearch(tab) && !tab.renaming) refresh(tabId);
}

/** The tabs of a pane, in order. */
export const tabsIn = (pane: number) => tabs.filter((tab) => tab.pane === pane);

export function closeTab(tabId: number) {
  const tab = find(tabId);
  if (!tab) return;
  const siblings = tabsIn(tab.pane);
  if (siblings.length === 1) {
    // The last tab of the second pane closes the pane; the last tab of all, the window.
    if (dual()) closePane(tab.pane);
    else void getCurrentWindow().close();
    return;
  }
  const position = siblings.findIndex((each) => each.id === tabId);
  const next = siblings[position === 0 ? 1 : position - 1].id;
  if (activeId() === tabId) activateTab(next);
  else if (shownTab(tab.pane) === tabId) showInPane(tab.pane, next);
  forget(tabId);
}

/** Removes a tab and frees what it held. */
function forget(tabId: number) {
  const tab = find(tabId);
  const sets = [tab?.listing?.set, tab?.unfiltered?.set, ...(retiring.get(tabId) ?? [])];
  setTabs((all) => all.filter((each) => each.id !== tabId));
  cancelListing(tabId);
  queues.delete(tabId);
  selections.delete(tabId);
  retiring.delete(tabId);
  for (const set of new Set(sets)) if (set !== undefined) void api.releaseSet(set);
}

/**
 * Shows or hides the second pane. Shown, it opens on `paths` (one tab each), or on the active
 * tab's folder; hidden, its tabs close, and their folders are returned so it can open on them
 * again. Either way the first pane keeps its tabs.
 */
export function setDualPane(on: boolean, paths: string[] = []): string[] {
  if (on === dual()) return [];
  if (on) {
    setDual(true);
    // The first tab opened is the one the pane shows.
    for (const path of paths.length > 0 ? paths : [activeTab()?.path ?? places().home]) openTab(path, 1, false);
    return [];
  }
  return closePane(1);
}

/**
 * Closes a pane with all its tabs, leaving one pane; the second pane's tabs move to the first
 * when the first is closed. Returns the folders of the tabs that closed.
 */
export function closePane(pane: number): string[] {
  if (!dual()) return [];
  const closing = tabsIn(pane);
  const kept = pane === 0 ? 1 : 0;
  if (activePane() === pane) activateTab(shownTab(kept));
  if (pane === 0) {
    setTabs((tab) => tab.pane === 1, "pane", 0);
    showInPane(0, shownTab(1));
  }
  setDual(false);
  void layoutApi.watchOtherPane(null).catch(() => {});
  const paths = closing.map((tab) => tab.path);
  closing.forEach((tab) => forget(tab.id));
  return paths;
}

/** Switches to a tab, bringing it up to date: only the tabs on screen are watched for changes. */
export function activateTab(tabId: number) {
  if (activeId() === tabId) return;
  const previous = activeTab();
  const tab = find(tabId);
  // A tab the other pane already shows is watched and current; only the watches change places.
  const watched = tab !== undefined && onScreen(tabId);
  setActiveId(tabId);
  if (tab) showInPane(tab.pane, tabId);
  // The tab the other pane shows now is watched by the other pane's watch.
  if (dual() && previous && tab && previous.pane !== tab.pane && !isSearch(previous) && !locationOf(previous.path)) {
    void layoutApi.watchOtherPane(previous.path).catch(() => {});
  }
  if (!tab || isSearch(tab) || tab.renaming) return;
  if (watched && tab.listing) void layoutApi.watchActiveFolder(tab.path).catch(() => {});
  else refresh(tabId);
}

/** Moves to the next or previous tab of the active pane, wrapping around. */
export function cycleTab(step: 1 | -1) {
  const siblings = tabsIn(activePane());
  const position = siblings.findIndex((tab) => tab.id === activeId());
  activateTab(siblings[(position + step + siblings.length) % siblings.length].id);
}

/**
 * Loads what the window needs and opens its tabs: with `open`, as it says (the layout kept from
 * the last run, panes.ts), or one tab in the home folder.
 */
export async function start(open: (home: string) => Promise<void> = async (home) => void openTab(home)) {
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
      if (isSearch(tab) || !samePath(tab.path, path)) continue;
      // A change during an edit is shown once the edit is over.
      if (tab.renaming) refreshAfterRename.add(tab.id);
      else refresh(tab.id);
    }
  });
  await open(initialPlaces.home);
}
