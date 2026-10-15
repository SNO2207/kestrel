import { createEffect, createRoot, createSignal, on } from "solid-js";

import { diskApi, type DiskOverview, type GrowthView, type Measure } from "./lib/diskApi";
import { baseName, parentPath } from "./lib/format";
import { closeProjects, projectsOpen } from "./projects";
import { activeId, activeTab, navigate, notify, openTab, places, selectInFolder } from "./state";
import { undoLast } from "./undo";

/**
 * The disk usage hub: whether it is open, the folder or drive it analyses, the way back out of
 * the folders zoomed into, and what the backend said about it. Everything is worked out from
 * the index; while the hub is open the backend says when the index changes, and it is asked
 * again. The tab and the treemap's measure are remembered in local storage.
 */

const PREFS_KEY = "kestrel.disk";

function loadPrefs(): { tab: string; measure: Measure } {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
    return {
      tab: typeof saved.tab === "string" ? saved.tab : "overview",
      measure: saved.measure === "logical" ? "logical" : "allocated",
    };
  } catch {
    return { tab: "overview", measure: "allocated" };
  }
}

function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ tab: tab(), measure: measure() }));
  } catch {
    // A convenience only; without storage the defaults return.
  }
}

const prefs = loadPrefs();
const [open, setOpen] = createSignal(false);
/** The folder or drive analysed, as asked for. */
const [root, setRoot] = createSignal("");
/** The folders zoomed out of, most recent last: what Back returns to. */
const [trail, setTrail] = createSignal<string[]>([]);
const [tab, setTabSignal] = createSignal(prefs.tab);
const [measure, setMeasureSignal] = createSignal<Measure>(prefs.measure);
const [overview, setOverview] = createSignal<DiskOverview | null>(null);
const [growth, setGrowth] = createSignal<GrowthView | null>(null);
const [error, setError] = createSignal<string | null>(null);
const [loading, setLoading] = createSignal(false);
/** Bumped whenever the index changes while the hub is open; every view reloads on it. */
const [revision, setRevision] = createSignal(0);
/** What the hub last did, such as recycling an item, with a way to undo it. */
const [status, setStatus] = createSignal<{ text: string; undo: boolean } | null>(null);

export {
  open as diskOpen,
  root as diskRoot,
  trail as diskTrail,
  tab as diskTab,
  measure as diskMeasure,
  overview as diskOverview,
  growth as diskGrowth,
  error as diskError,
  loading as diskLoading,
  revision as diskRevision,
  status as diskStatus,
};

export const setDiskTab = (next: string) => (setTabSignal(next), savePrefs());
export const setDiskMeasure = (next: Measure) => (setMeasureSignal(next), savePrefs());
export const dismissDiskStatus = () => setStatus(null);

/** Whether two paths name the same place, however they are written. */
export const samePath = (a: string, b: string) => {
  const plain = (path: string) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
  return plain(a) === plain(b);
};

/** The drive a path is on, such as `C:\`, or the path itself when it names none. */
export const driveOf = (path: string) => (/^[A-Za-z]:/.test(path) ? `${path.slice(0, 2).toUpperCase()}\\` : path);

/** The folder the active tab shows (or searched from). */
const currentFolder = () => activeTab()?.path || null;

// ---------- loading ----------

let listening = false;
let request = 0;

function listen() {
  if (listening) return;
  listening = true;
  void diskApi.onChanged(() => {
    if (open()) setRevision((n) => n + 1);
  });
}

async function load(path: string) {
  const id = ++request;
  setLoading(true);
  try {
    const [found, grew] = await Promise.all([diskApi.overview(path), diskApi.growth(path).catch(() => null)]);
    if (id !== request) return;
    setOverview(found);
    setGrowth(grew);
    setError(null);
  } catch (failure) {
    if (id !== request) return;
    setOverview(null);
    setGrowth(null);
    setError(String(failure));
  } finally {
    if (id === request) setLoading(false);
  }
}

createRoot(() => {
  createEffect(
    on([open, root, revision], ([isOpen, path]) => {
      if (isOpen && path) void load(path);
    }),
  );
  // One hub at a time: the projects hub opening closes this one.
  createEffect(on(projectsOpen, (projects) => projects && open() && closeDiskUsage(), { defer: true }));
});

// ---------- opening, zooming, closing ----------

/**
 * Opens the disk usage hub on `path`, a drive or a folder; by default the folder on screen. The
 * places page (This PC) opens its drives with this.
 */
export function openDiskUsage(path?: string) {
  const target = path ?? currentFolder() ?? places().drives[0]?.path ?? "C:\\";
  listen();
  closeProjects();
  if (!samePath(target, root())) {
    setTrail([]);
    setRoot(target);
  }
  setStatus(null);
  setOpen(true);
  void diskApi.watch(true).catch(() => {});
}

export function closeDiskUsage() {
  if (!open()) return;
  setOpen(false);
  setStatus(null);
  void diskApi.watch(false).catch(() => {});
}

/** The hub's shortcut: open on the folder on screen, or close. */
export const toggleDiskUsage = () => (open() ? closeDiskUsage() : openDiskUsage());

/** Looks inside a folder, remembering where to come back to. */
export function zoomTo(path: string) {
  if (samePath(path, root())) return;
  setTrail([...trail(), root()]);
  setRoot(path);
}

/** Back: out of the folder last zoomed into. */
export function zoomBack() {
  const previous = trail();
  if (previous.length === 0) return;
  setTrail(previous.slice(0, -1));
  setRoot(previous[previous.length - 1]);
}

export const canZoomBack = () => trail().length > 0;

// ---------- leaving for the files ----------

/** Shows a folder in the current tab (or a new one) and closes the hub. */
export function showFolder(path: string) {
  const tab = activeTab();
  closeDiskUsage();
  if (tab) navigate(tab.id, path);
  else openTab(path);
}

/** Shows an item in its folder, selected. */
export async function revealItem(path: string) {
  const folder = parentPath(path);
  if (!folder) return showFolder(path);
  showFolder(folder);
  // Once the folder is on screen, the item is selected as a pasted one would be.
  for (let attempt = 0; attempt < 100; attempt++) {
    const tab = activeTab();
    if (tab?.listing && samePath(tab.path, folder)) return selectInFolder(folder, [baseName(path)]);
    await new Promise((resolve) => setTimeout(resolve, 30));
  }
}

/**
 * Sends items to the Recycle Bin through the app's file operations, so the undo history has
 * them; the hub has asked first. `label` names them in the message.
 */
export async function recycleItems(paths: string[], label: string) {
  try {
    const recycled = await diskApi.recycle(paths);
    if (!recycled) return;
    setStatus({ text: `Moved ${label} to the Recycle Bin.`, undo: true });
    // Tabs showing the folder list it again; the hub hears from the index.
    for (const folder of new Set(paths.map((path) => parentPath(path)).filter((f): f is string => !!f))) {
      selectInFolder(folder, []);
    }
  } catch (failure) {
    notify(failure);
  }
}

/** Undoes the latest file operation, as Ctrl+Z does in a folder. */
export function undoInDisk() {
  setStatus(null);
  undoLast(activeId());
}
