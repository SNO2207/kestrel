/**
 * Explorer parity, second round: the view options Explorer has that Kestrel lacked (compact view,
 * item check boxes, icons instead of thumbnails, compressed and encrypted files in colour, empty
 * drives hidden, the navigation pane hidden, libraries in it), the privacy switches and Clear
 * history, tab commands (duplicate, close others, close to the right, open in a new window, drag
 * out to a window of its own or into another window), the network's dialogs and the shell's
 * Libraries, and adding files to a zip.
 *
 * The options are kept in local storage, which every window shares, and apply at once, through
 * classes on the document's root for the ones that are only looks (`parity2.css`). The backend
 * is `src-tauri/src/parity2`; the components are `Parity2Layer`, `LibrariesPage`, `NetworkPage`,
 * `LibrariesSection` and `ItemCheck`.
 */
import { createSignal } from "solid-js";

import type { MenuEntry } from "./components/ContextMenu";
import type { DriveInfo } from "./lib/placesApi";
import { parentPath } from "./lib/format";
import { parity2Api, type Library, type TabArrival, type TabState } from "./lib/parity2Api";
import { parityApi } from "./lib/parityApi";
import { locationOf } from "./lib/locations";
import { THIS_PC, loadThisPc } from "./places";
import { refreshFrequent } from "./shortcuts";
import { activePane, addTabWith, closeTab, notify, selectInFolder, tabs, tabsIn, type Tab } from "./state";

// ---------- options ----------

export interface Parity2Options {
  /** Explorer's "Compact view": tighter rows and cells in every view. */
  compact: boolean;
  /** "Item check boxes": a check box on items under the pointer and on selected items. */
  checkboxes: boolean;
  /** "Always show icons, never thumbnails". */
  alwaysIcons: boolean;
  /** "Show encrypted or compressed NTFS files in colour". */
  ntfsColour: boolean;
  /** "Hide empty drives": card readers and disc drives with nothing in them. */
  hideEmptyDrives: boolean;
  /** "Show libraries" in the navigation pane. */
  showLibraries: boolean;
  /** The navigation pane (the sidebar) is shown. */
  navPane: boolean;
  /** Privacy: Home lists recently used files. */
  showRecent: boolean;
  /** Privacy: Home and the sidebar list frequently used folders. */
  showFrequent: boolean;
}

const KEY = "kestrel.parity2";
/** Explorer's own defaults. */
export const DEFAULT_PARITY2: Parity2Options = {
  compact: false,
  checkboxes: false,
  alwaysIcons: false,
  ntfsColour: true,
  hideEmptyDrives: true,
  showLibraries: false,
  navPane: true,
  showRecent: true,
  showFrequent: true,
};

function load(): Parity2Options {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (stored === null || typeof stored !== "object") return { ...DEFAULT_PARITY2 };
    const value = stored as Partial<Parity2Options>;
    const options = { ...DEFAULT_PARITY2 };
    for (const name of Object.keys(DEFAULT_PARITY2) as (keyof Parity2Options)[]) {
      if (typeof value[name] === "boolean") options[name] = value[name] as boolean;
    }
    return options;
  } catch {
    return { ...DEFAULT_PARITY2 };
  }
}

const [options, setOptionsSignal] = createSignal<Parity2Options>(load());
export const parity2Options = options;

/** The options that are only looks, as classes on the document's root (parity2.css). */
function apply(current: Parity2Options) {
  const root = document.documentElement.classList;
  root.toggle("nav-pane-hidden", !current.navPane);
  root.toggle("ntfs-colour", current.ntfsColour);
}
apply(options());

export function setParity2Options(change: Partial<Parity2Options>) {
  setOptionsSignal((current) => ({ ...current, ...change }));
  apply(options());
  try {
    localStorage.setItem(KEY, JSON.stringify(options()));
  } catch {
    // Storage is unavailable; the options still last for this session.
  }
}

/** Flips one option. */
export const toggleOption = (name: keyof Parity2Options) => setParity2Options({ [name]: !options()[name] });

// Another window changed them.
window.addEventListener("storage", (event) => {
  if (event.key !== KEY) return;
  setOptionsSignal(load());
  apply(options());
});

export const compactView = () => options().compact;
export const itemCheckBoxes = () => options().checkboxes;
export const alwaysIcons = () => options().alwaysIcons;

// ---------- compressed and encrypted files ----------

const COMPRESSED = 0x800;
const ENCRYPTED = 0x4000;

/**
 * How a name is coloured, as Explorer colours it: green for an encrypted file, blue for a
 * compressed one; nothing while the option is off.
 */
export function ntfsTint(attrs: number | undefined): "encrypted" | "compressed" | undefined {
  if (!attrs || !options().ntfsColour) return undefined;
  if (attrs & ENCRYPTED) return "encrypted";
  if (attrs & COMPRESSED) return "compressed";
  return undefined;
}

// ---------- drives ----------

/** The drives to show: with "Hide empty drives", those with nothing in them are left out. */
export const shownDrives = (drives: DriveInfo[]) => (options().hideEmptyDrives ? drives.filter((drive) => drive.state !== "no-media") : drives);

// ---------- the network ----------

/** Map network drive: Windows' own dialog; This PC is read again once a drive is mapped. */
export async function mapNetworkDrive(remote: string | null = null) {
  try {
    const drive = await parity2Api.mapDrive(remote);
    if (drive) {
      notify(`Mapped the network drive ${drive}`, "info");
      void loadThisPc();
    }
  } catch (error) {
    notify(error);
  }
}

/** Disconnect network drive: one drive, or Windows' list of them to choose from. */
export async function disconnectNetworkDrive(path: string | null = null) {
  try {
    await parity2Api.disconnectDrive(path);
    void loadThisPc();
  } catch (error) {
    notify(error);
  }
}

export const addNetworkLocation = () => void parity2Api.addNetworkLocation().catch(notify);

/** The network commands This PC's "..." menu and context menu offer, as Explorer's do. */
export function networkEntries(): MenuEntry[] {
  return [
    { label: "Map network drive…", run: () => void mapNetworkDrive() },
    { label: "Disconnect network drive…", run: () => void disconnectNetworkDrive() },
    { label: "Add a network location", run: addNetworkLocation },
  ];
}

/** Whether the "..." menu offers the network's commands at `address`: This PC and the Network. */
export function showsNetworkCommands(address: string): boolean {
  const location = locationOf(address)?.address;
  return location === THIS_PC || location === "shell:NetworkPlacesFolder";
}

/** The menu entry that disconnects one mapped drive. */
export const disconnectEntry = (drive: DriveInfo): MenuEntry => ({ label: "Disconnect", run: () => void disconnectNetworkDrive(drive.path) });

// ---------- libraries ----------

const [libraries, setLibraries] = createSignal<Library[]>([]);
export { libraries };

let readingLibraries: Promise<void> | null = null;

/** Reads the libraries again; calls made meanwhile share the read. */
export function loadLibraries(): Promise<void> {
  if (readingLibraries) return readingLibraries;
  const reading = parity2Api
    .libraries()
    .then((found) => void setLibraries(found))
    .catch((error) => console.error("libraries:", error))
    .finally(() => (readingLibraries = null));
  readingLibraries = reading;
  return reading;
}

// ---------- privacy ----------

/** Clear File Explorer history: Kestrel's own records only (`parity2/privacy.rs`). */
export async function clearHistory(): Promise<boolean> {
  try {
    const cleared = await parity2Api.clearHistory();
    void refreshFrequent();
    const count = cleared.visits + cleared.opened + cleared.palette;
    notify(count > 0 ? "Cleared Kestrel's history of folders, files and the palette" : "There was no history to clear", "info");
    return true;
  } catch (error) {
    notify(error);
    return false;
  }
}

// ---------- tabs ----------

const find = (tabId: number) => tabs.find((tab) => tab.id === tabId);

/** A tab as it travels: where it is and where it has been. */
export function tabStateOf(tab: Tab): TabState {
  return { path: tab.path, history: [...tab.history], cursor: tab.cursor };
}

/** Duplicate tab: a new tab beside it, in the same place with the same history. */
export function duplicateTab(tabId: number) {
  const tab = find(tabId);
  if (tab) addTabWith(tabStateOf(tab), tab.pane, tabId);
}

/** Closes every other tab of the tab's pane. */
export function closeOtherTabs(tabId: number) {
  const tab = find(tabId);
  if (tab) tabsIn(tab.pane).filter((other) => other.id !== tabId).forEach((other) => closeTab(other.id));
}

/** Closes the tabs after it in its pane's strip. */
export function closeTabsToRight(tabId: number) {
  const tab = find(tabId);
  if (!tab) return;
  const strip = tabsIn(tab.pane);
  strip.slice(strip.findIndex((other) => other.id === tabId) + 1).forEach((other) => closeTab(other.id));
}

/** Open in new window: another window showing what the tab shows; the tab stays. */
export const openTabInNewWindow = (tabId: number) => {
  const tab = find(tabId);
  if (tab) void parityApi.newWindow(tab.path).catch(notify);
};

/** A tab's context menu, as Explorer's. */
export function tabMenuEntries(tabId: number): MenuEntry[] {
  const tab = find(tabId);
  if (!tab) return [];
  const strip = tabsIn(tab.pane);
  const last = strip[strip.length - 1]?.id === tabId;
  return [
    { label: "Duplicate tab", run: () => duplicateTab(tabId) },
    { label: "Open in new window", run: () => openTabInNewWindow(tabId) },
    "separator",
    { label: "Close tab", hint: "Ctrl+W", run: () => closeTab(tabId) },
    { label: "Close other tabs", disabled: strip.length < 2, run: () => closeOtherTabs(tabId) },
    { label: "Close tabs to the right", disabled: last, run: () => closeTabsToRight(tabId) },
  ];
}

/** How far a tab moves outside its strip before letting go of it moves it out. */
const TEAR_MARGIN = 28;
/** How far the pointer moves before a press on a tab becomes a drag. */
const DRAG_THRESHOLD = 6;

const [draggedTab, setDraggedTab] = createSignal<{ id: number; out: boolean } | null>(null);
/** The tab being dragged, and whether letting go now would take it out of its strip. */
export { draggedTab };

/**
 * Follows a press on a tab: dragged out of its strip and let go, it moves to the window under the
 * pointer if that window's strip is there, or to a new window of its own (`dropTab`).
 */
export function startTabDrag(tabId: number, start: PointerEvent) {
  if (start.button !== 0 || (start.target as Element).closest(".tab-close")) return;
  const element = start.currentTarget as HTMLElement;
  const strip = element.closest<HTMLElement>(".tabs, .pane-tabs") ?? element;
  let dragging = false;
  const outside = (event: PointerEvent) => {
    const box = strip.getBoundingClientRect();
    return (
      event.clientY < box.top - TEAR_MARGIN ||
      event.clientY > box.bottom + TEAR_MARGIN ||
      event.clientX < 0 ||
      event.clientY < 0 ||
      event.clientX > window.innerWidth ||
      event.clientY > window.innerHeight
    );
  };
  const move = (event: PointerEvent) => {
    if ((event.buttons & 1) === 0) return finish(event, false);
    if (!dragging) {
      if (Math.hypot(event.clientX - start.clientX, event.clientY - start.clientY) < DRAG_THRESHOLD) return;
      dragging = true;
      // The pointer is followed outside the window too, until the button comes up.
      element.setPointerCapture(start.pointerId);
    }
    setDraggedTab({ id: tabId, out: outside(event) });
  };
  const finish = (event: PointerEvent, released: boolean) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", cancel);
    const out = dragging && released && outside(event);
    setDraggedTab(null);
    if (element.hasPointerCapture(start.pointerId)) element.releasePointerCapture(start.pointerId);
    if (out) void dropTab(tabId);
  };
  const up = (event: PointerEvent) => finish(event, true);
  const cancel = (event: PointerEvent) => finish(event, false);
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", cancel);
}

/**
 * A tab let go outside its strip: another window takes it if it was let go over that window's
 * strip, otherwise it gets a window of its own; either way it leaves this one (and the window
 * closes with its last tab). `at` stands in for the pointer (development builds, for the tests):
 * a window's label and a point of its page, or no label for nowhere.
 */
export async function dropTab(tabId: number, at: { label: string | null; x: number; y: number } | null = null): Promise<string> {
  const tab = find(tabId);
  if (!tab) return "none";
  try {
    const result = await parity2Api.tabDrop(tabStateOf(tab), tabs.length === 1, at);
    if (result !== "none") closeTab(tabId);
    return result;
  } catch (error) {
    notify(error);
    return "none";
  }
}

/**
 * Another window asks whether this one takes a tab let go at a point of its page: it does when
 * the point is on its tab strip (the title bar's, or a pane's), opening the tab in that pane.
 */
export async function receiveTab(arrival: TabArrival) {
  const element = document.elementFromPoint(arrival.x, arrival.y);
  const bar = element?.closest<HTMLElement>(".pane-bar, .titlebar");
  const taken = !!bar && !element?.closest(".window-controls");
  if (taken) {
    const pane = bar.classList.contains("pane-bar") ? Number(bar.dataset.pane ?? 0) : activePane();
    // The backend brings the window forward once it hears the tab was taken.
    addTabWith(arrival.tab, pane);
  }
  await parity2Api.tabArrival(arrival.id, taken).catch(() => {});
}

// ---------- adding to a zip ----------

/**
 * Adds files to a zip, as Explorer does with files dropped on one: `target` is the zip or a folder
 * of one open in a tab. The new items are selected wherever the archive is shown.
 */
export async function addToZip(target: string, paths: string[]) {
  try {
    const added = await parity2Api.addToZip(target, paths);
    // The zip's own folder shows it changed; a tab inside it shows the new items.
    const folder = parentPath(added.archive);
    if (folder) selectInFolder(folder, []);
    selectInFolder(target, added.names);
    return added;
  } catch (error) {
    notify(error);
    return null;
  }
}

/** Whether a listing is of a zip (or a folder in one) open in a tab, which takes pasted files. */
export const isZipListing = (listing: { archive?: string } | null | undefined) => /\.zip$/i.test(listing?.archive ?? "");

/** Whether a row is a zip, which takes files dropped on it. */
export const isZipRow = (row: { dir: boolean; ext: string } | undefined) => !!row && !row.dir && row.ext.toLowerCase() === "zip";
