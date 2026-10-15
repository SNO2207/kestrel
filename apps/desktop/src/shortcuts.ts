/**
 * Shortcuts back to places: pinned places in groups, frequent folders, saved searches, tags
 * and workspaces. This module holds what the sidebar and the dialogs show, kept in step with
 * the backend, and the actions on it; the components draw it.
 *
 * Every folder a tab shows counts as a visit, which the backend ranks by frecency (it ignores
 * scratch folders such as %TEMP%). Tags are shown by path, so where every tagged item is now is
 * asked again whenever a listing changes: an item renamed or moved keeps its tags because the
 * backend follows it by file id.
 */
import { batch, createEffect, createSignal, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";

import { api } from "./lib/api";
import { baseName, parentPath } from "./lib/format";
import {
  shortcutsApi as backend,
  type Board,
  type Color,
  type Frequent,
  type Pin,
  type SavedSearch,
  type Tag,
  type TagsState,
  type Workspace,
  type WorkspaceTab,
} from "./lib/shortcutsApi";
import { VIEW_MODES, folderKey, recall, remember, viewModeAt, type ViewMemory } from "./lib/views";
import {
  activateTab,
  activeId,
  activeTab,
  closeTab,
  everywhere,
  isSearch,
  navigate,
  notify,
  openSearch,
  openTab,
  places,
  selectWhenListed,
  selectedRows,
  status,
  tabs,
  type Tab,
} from "./state";

/** Folders the Frequent list shows. */
const FREQUENT_SHOWN = 6;
/** Most items one pinning takes. */
const MAX_PINNED_AT_ONCE = 50;
/** Most items one tagging takes. */
export const MAX_TAGGED_AT_ONCE = 10_000;
/** Listings settle for this long before tags and pins are checked again. */
const SETTLE_MS = 250;

/**
 * What the sidebar shows. Each refresh is reconciled into the store by id (or path), so an
 * unchanged pin keeps its object, and with it its row and any focus on it: refreshes follow
 * every listing change.
 */
const [data, setData] = createStore<{
  board: Board;
  frequent: Frequent[];
  searches: SavedSearch[];
  tags: Tag[];
  workspaces: Workspace[];
}>({ board: { groups: [], pins: [] }, frequent: [], searches: [], tags: [], workspaces: [] });
/** Each tagged item's tags, by its path in lower case. */
const [tagMap, setTagMap] = createSignal(new Map<string, number[]>());

export const board = () => data.board;
export const frequent = () => data.frequent;
export const searches = () => data.searches;
export const tags = () => data.tags;
export const workspaces = () => data.workspaces;

/**
 * Makes a refresher that fetches and applies. Fetches can overlap and finish in any order (the
 * backend runs each on a thread of its own), so only the answer to the latest one is applied:
 * an older one may describe files from before they moved.
 */
function loader<T>(fetch: () => Promise<T>, apply: (value: T) => void) {
  let latest = 0;
  return async () => {
    const request = ++latest;
    try {
      const value = await fetch();
      if (request === latest) apply(value);
    } catch (error) {
      console.error(error);
    }
  };
}

export const refreshBoard = loader(backend.board, (value) => setData("board", reconcile(value, { key: "id" })));
export const refreshFrequent = loader(
  () => backend.frequent(FREQUENT_SHOWN),
  (value) => setData("frequent", reconcile(value, { key: "path" })),
);
export const refreshSearches = loader(backend.searches, (value) => setData("searches", reconcile(value, { key: "id" })));
export const refreshWorkspaces = loader(backend.workspaces, (value) => setData("workspaces", reconcile(value, { key: "id" })));
export const refreshTags = loader(backend.tags, (state: TagsState) => {
  const map = new Map<string, number[]>();
  for (const item of state.items) {
    const key = item.path.toLowerCase();
    // Hard links can make two items one path; their tags are shown together.
    map.set(key, [...new Set([...(map.get(key) ?? []), ...item.tags])]);
  }
  batch(() => {
    setData("tags", reconcile(state.tags, { key: "id" }));
    setTagMap(map);
  });
});

export const refreshAll = () =>
  Promise.all([refreshBoard(), refreshFrequent(), refreshSearches(), refreshTags(), refreshWorkspaces()]);

// ---------- names ----------

/** A folder's name for a list: its own name, or for a drive root, the drive as Places names it. */
export function folderName(path: string): string {
  const name = baseName(path);
  if (!name.endsWith(":")) return name || path;
  const drive = places().drives.find((candidate) => candidate.path.toLowerCase().startsWith(name.toLowerCase()));
  return `${drive?.label || "Local Disk"} (${name.toUpperCase()})`;
}

/** What a pin is called in the sidebar. */
export const pinName = (pin: Pick<Pin, "alias" | "path">) => pin.alias ?? folderName(pin.path);

const items = (count: number) => (count === 1 ? "1 item" : `${count} items`);

// ---------- pins ----------

/** Pins paths at the end of `group` (`null`: above every group), and says what happened. */
export async function pinPaths(paths: string[], group: number | null = null) {
  if (paths.length === 0) return;
  if (paths.length > MAX_PINNED_AT_ONCE) return notify(`Pin at most ${MAX_PINNED_AT_ONCE} items at a time.`);
  try {
    const { added, existing } = await backend.pin(paths, group);
    await refreshBoard();
    if (added === 0) notify("Already pinned to the sidebar", "info");
    else notify(`Pinned ${items(added)} to the sidebar${existing > 0 ? `; ${existing} already were` : ""}`, "info");
  } catch (error) {
    notify(error);
  }
}

/** Pins the tab's selection, or the folder it shows when nothing is selected (Ctrl+D). */
export async function pinSelection(tabId: number) {
  const rows = await selectedRows(tabId, MAX_PINNED_AT_ONCE + 1);
  if (rows.length > 0) return pinPaths(rows.map((row) => row.path));
  return pinFolder(tabId);
}

/** Pins the folder the tab shows. */
export async function pinFolder(tabId: number) {
  const tab = tabs.find((candidate) => candidate.id === tabId);
  if (tab && !isSearch(tab) && tab.path) await pinPaths([tab.path]);
}

/** Opens a folder in the active tab or a new one, or a file in its default app. */
export function openPlace(path: string, dir: boolean, inNewTab = false) {
  if (!dir) return void api.openPath(path).catch(notify);
  const tab = activeTab();
  if (inNewTab || !tab) openTab(path);
  else navigate(tab.id, path);
}

/** Opens what a pin points at. A missing pin is looked for first, by its file id. */
export async function openPin(pin: Pin, inNewTab = false) {
  if (!pin.missing) return openPlace(pin.path, pin.dir, inNewTab);
  const found = await findPin(pin);
  if (found) openPlace(found, pin.dir, inNewTab);
}

/** Looks for a missing pin's target through the index, by its file id. Resolves to its new path. */
export async function findPin(pin: Pin): Promise<string | null> {
  // Named now: the pin is live, and once found it takes the name of where it is.
  const name = pinName(pin);
  try {
    const path = await backend.locatePin(pin.id);
    await refreshBoard();
    notify(`Found ${name} at ${path}`, "info");
    return path;
  } catch (error) {
    notify(error);
    return null;
  }
}

/** Opens the pin with number key `key` (Ctrl+1 to Ctrl+9). */
export function openPinKey(key: number) {
  const pin = board().pins.find((candidate) => candidate.key === key);
  if (pin) void openPin(pin);
}

/** Runs a change to pins or groups, then shows the result, or why it failed. */
export async function changePins(change: () => Promise<unknown>) {
  try {
    await change();
  } catch (error) {
    notify(error);
  }
  await refreshBoard();
}

/** Waits, up to a few seconds, until the tab is `ready`. Resolves to whether it got there. */
async function until(tabId: number, ready: (tab: Tab) => boolean): Promise<boolean> {
  for (let waited = 0; waited < 5000; waited += 50) {
    const tab = tabs.find((candidate) => candidate.id === tabId);
    if (!tab) return false;
    if (ready(tab)) return true;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  return false;
}

/** Shows the folder holding `path`, with the item selected. */
export async function showInFolder(path: string) {
  const parent = parentPath(path);
  const tab = activeTab();
  if (!parent || !tab) return;
  navigate(tab.id, parent);
  // Items to select are set once the tab has arrived; arriving clears any set before.
  const arrived = await until(tab.id, (current) => !isSearch(current) && current.listing?.path.toLowerCase() === parent.toLowerCase());
  if (arrived) selectWhenListed(tab.id, [baseName(path)]);
}

// ---------- frequent folders ----------

export async function forgetFolder(path: string) {
  await backend.forgetFolder(path).catch(notify);
  await refreshFrequent();
}

// ---------- saved searches ----------

/** Saves the tab's search, with where it searches, under `name`. */
export async function saveSearch(tabId: number, name: string) {
  const tab = tabs.find((candidate) => candidate.id === tabId);
  if (!tab || tab.query.trim() === "") return;
  try {
    await backend.saveSearch(name, tab.query, everywhere() ? null : tab.path);
    await refreshSearches();
    notify(`Saved the search as "${name.trim()}"`, "info");
  } catch (error) {
    notify(error);
  }
}

/** Runs a saved search in the active tab, or a new one. */
export async function runSavedSearch(saved: SavedSearch, inNewTab = false) {
  if (inNewTab || !activeTab()) {
    openTab(saved.scope ?? undefined);
    // A new tab lists its folder first, which would clear a search started before it arrives.
    const tab = activeTab();
    if (!tab || !(await until(tab.id, (current) => current.listing !== null))) return;
  }
  const tab = activeTab();
  if (tab) openSearch(tab.id, saved.query, saved.scope);
}

export async function changeSearches(change: () => Promise<unknown>) {
  try {
    await change();
  } catch (error) {
    notify(error);
  }
  await refreshSearches();
}

// ---------- tags ----------

/** The tags on the item at `path`, in sidebar order. */
export function tagsFor(path: string | undefined): Tag[] {
  if (!path) return [];
  const ids = tagMap().get(path.toLowerCase());
  if (!ids) return [];
  // Reading each tag through the store keeps a name or colour change live on every dot.
  return ids.map((id) => data.tags.find((tag) => tag.id === id)).filter((tag): tag is Tag => tag !== undefined);
}

/** Puts a tag on the paths, or takes it off with `on` false. */
export async function setTag(paths: string[], tag: Tag, on: boolean) {
  try {
    await backend.assignTag(paths, tag.id, on);
  } catch (error) {
    notify(error);
  }
  await refreshTags();
}

/** Makes a tag and, if any paths are given, puts it on them. */
export async function createTag(name: string, color: Color, paths: string[] = []): Promise<Tag | null> {
  try {
    const tag = await backend.createTag(name, color);
    if (paths.length > 0) await backend.assignTag(paths, tag.id, true);
    await refreshTags();
    return tag;
  } catch (error) {
    notify(error);
    return null;
  }
}

export async function changeTags(change: () => Promise<unknown>) {
  try {
    await change();
  } catch (error) {
    notify(error);
  }
  await refreshTags();
}

/** The search text that finds a tag's items. */
export const tagQuery = (tag: Pick<Tag, "name">) => `tag:${/\s/.test(tag.name) ? `"${tag.name}"` : tag.name}`;

/** Shows everything carrying `tag`, searching every indexed drive. */
export function searchTag(tag: Tag, inNewTab = false) {
  void runSavedSearch({ id: 0, name: tag.name, query: tagQuery(tag), scope: null }, inNewTab);
}

// ---------- workspaces ----------

const isViewMode = (value: string | null): value is NonNullable<ViewMemory["mode"]> =>
  VIEW_MODES.some((info) => info.mode === value);

/** The open tabs as a workspace keeps them: folder, view and sort. */
function currentTabs(): { tabs: WorkspaceTab[]; active: number } {
  const saved = tabs.map((tab) => {
    const memory = recall(folderKey(tab.path));
    return { path: tab.path, view: viewModeAt(folderKey(tab.path)), sort: memory.sort ?? (isSearch(tab) ? null : { ...tab.sort }) };
  });
  return { tabs: saved, active: Math.max(0, tabs.findIndex((tab) => tab.id === activeId())) };
}

/** Saves the open tabs under `name`, replacing the workspace of that name if there is one. */
export async function saveWorkspace(name: string) {
  const { tabs: saved, active } = currentTabs();
  try {
    const workspace = await backend.saveWorkspace(name, saved, active);
    await refreshWorkspaces();
    notify(`Saved ${saved.length === 1 ? "1 tab" : `${saved.length} tabs`} as "${workspace.name}"`, "info");
  } catch (error) {
    notify(error);
  }
}

/** Replaces the open tabs with the workspace's, each with the view and sort it was saved with. */
export function restoreWorkspace(workspace: Workspace) {
  if (workspace.tabs.length === 0) return;
  const previous = tabs.map((tab) => tab.id);
  for (const saved of workspace.tabs) {
    const memory: ViewMemory = {};
    if (isViewMode(saved.view)) memory.mode = saved.view;
    if (saved.sort) memory.sort = { ...saved.sort };
    if (memory.mode || memory.sort) remember(folderKey(saved.path), memory);
    openTab(saved.path);
  }
  const opened = tabs.map((tab) => tab.id).filter((id) => !previous.includes(id));
  activateTab(opened[Math.min(workspace.active, opened.length - 1)]);
  // The new tabs are open, so closing every old one never closes the window.
  for (const id of previous) closeTab(id);
}

export async function changeWorkspaces(change: () => Promise<unknown>) {
  try {
    await change();
  } catch (error) {
    notify(error);
  }
  await refreshWorkspaces();
}

// ---------- keeping up ----------

/**
 * Loads everything, records folder visits, and keeps tags and pins current as listings change.
 * Call once, from a component, which owns what this sets up.
 */
export function startShortcuts() {
  void refreshAll();

  // A visit is a tab arriving in a folder. The folder the app opens in, and a new tab that
  // shows a folder another tab already shows, are not choices and don't count.
  const shown = new Map<number, string>();
  let started = false;
  createEffect(() => {
    for (const tab of tabs) {
      const path = tab.listing?.path;
      if (!path) continue;
      const before = shown.get(tab.id);
      if (before?.toLowerCase() === path.toLowerCase()) continue;
      shown.set(tab.id, path);
      const elsewhere = tabs.some((other) => other.id !== tab.id && other.path.toLowerCase() === path.toLowerCase());
      if (before === undefined && (!started || elsewhere)) {
        started = true;
        continue;
      }
      started = true;
      void backend
        .visit(path)
        .then((recorded) => {
          if (recorded) void refreshFrequent();
        })
        .catch(() => {});
    }
    for (const id of shown.keys()) if (!tabs.some((tab) => tab.id === id)) shown.delete(id);
  });

  // Anything that changes listings may have renamed, moved or deleted tagged or pinned items.
  let timer: number | undefined;
  createEffect(() => {
    for (const tab of tabs) void tab.revision;
    void status().generation;
    clearTimeout(timer);
    timer = window.setTimeout(() => void Promise.all([refreshTags(), refreshBoard()]), SETTLE_MS);
  });
  // Coming back to the window, as after renaming things in Explorer.
  const onFocus = () => void Promise.all([refreshTags(), refreshBoard(), refreshFrequent()]);
  window.addEventListener("focus", onFocus);
  onCleanup(() => {
    clearTimeout(timer);
    window.removeEventListener("focus", onFocus);
  });
}

// ---------- test hook ----------

// Development builds let the end-to-end tests read what the sidebar is built from, and tidy up
// what they made. Production builds compile this out.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelShortcuts: {
      api: backend,
      board: () => board(),
      frequent: () => frequent(),
      searches: () => searches(),
      tags: () => ({ tags: tags(), items: [...tagMap()].map(([path, ids]) => ({ path, tags: ids })) }),
      workspaces: () => workspaces(),
      refresh: refreshAll,
    },
  });
}
