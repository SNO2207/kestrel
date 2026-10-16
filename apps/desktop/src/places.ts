/**
 * Places: the data and actions behind This PC, the Recycle Bin page, the sidebar's folder
 * tree and the address bar. This PC and the Recycle Bin are special locations, registered in
 * `placesSetup.ts` (see lib/locations.ts for the mechanism).
 *
 * This PC's drives are asked for when something shows them and kept current: when a drive
 * comes or goes the backend says so (`drives-changed`), and the details of a drive that took
 * its time (an unreachable network drive) follow when they come (`drive-details`).
 */
import { createSignal } from "solid-js";

import { isArchiveFile } from "./archives";
import { locationOf, resolveLocation } from "./lib/locations";
import { placesApi, type DriveInfo, type ThisPcData } from "./lib/placesApi";
import { activeTab, isSearch, navigate, notify, openTab, selectInFolder, tabs } from "./state";

/** This PC's address; Explorer's own name for it. */
export const THIS_PC = "shell:MyComputerFolder";
/** The Recycle Bin's address; Explorer's own name for it. */
export const RECYCLE_BIN = "shell:RecycleBinFolder";

/** Whether a path is a drive's root, such as `C:\`. */
export const isDriveRoot = (path: string) => /^[a-z]:\\?$/i.test(path.replace(/\//g, "\\"));

const comparable = (path: string) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
export const samePlace = (a: string, b: string) => comparable(a) === comparable(b);

// ---------- This PC ----------

const [thisPc, setThisPc] = createSignal<ThisPcData>({ folders: [], drives: [] });
const [thisPcLoaded, setThisPcLoaded] = createSignal(false);
/** The user's folders and every drive, as last read. */
export { thisPc, thisPcLoaded };

let reading: Promise<void> | null = null;
let readAgain = false;

/** Reads This PC again. Calls made while a read is under way share it, and one more follows. */
export function loadThisPc(): Promise<void> {
  if (reading) {
    readAgain = true;
    return reading;
  }
  reading = placesApi
    .thisPc()
    .then((data) => {
      setThisPc(data);
      setThisPcLoaded(true);
    })
    .catch((error) => console.error("This PC:", error))
    .finally(() => {
      reading = null;
      if (readAgain) {
        readAgain = false;
        void loadThisPc();
      }
    });
  return reading;
}

/** Takes in the details of a drive that answered late. */
function updateDrive(drive: DriveInfo) {
  setThisPc((data) => ({ ...data, drives: data.drives.map((known) => (known.letter === drive.letter ? drive : known)) }));
}

/** A drive's name as This PC shows it ("OS (C:)"), once This PC has been read; null until then. */
export function driveTitle(root: string): string | null {
  const letter = root.slice(0, 1).toUpperCase();
  return thisPc().drives.find((drive) => drive.letter.toUpperCase().startsWith(letter))?.name || null;
}

/** How full a drive is, from 0 to 1; 0 when unknown. */
export const usedShare = (drive: DriveInfo) => (drive.total > 0 ? Math.min(1, (drive.total - drive.free) / drive.total) : 0);
/** Explorer turns a drive's bar red once it is 90 % full. */
export const isNearlyFull = (drive: DriveInfo) => drive.total > 0 && usedShare(drive) >= 0.9;
export const canEject = (drive: DriveInfo) => drive.kind === "removable" || drive.kind === "optical";

export async function ejectDrive(drive: DriveInfo) {
  try {
    await placesApi.ejectDrive(drive.path);
    notify(`${drive.name} can be removed now.`, "info");
  } catch (error) {
    notify(error);
  }
  void loadThisPc();
}

// ---------- the disk usage hub ----------

/**
 * Opens the disk usage hub on a drive or folder. Another feature provides the hub: it calls
 * `setDiskUsageHandler` at startup, and This PC and the folder tree then offer "Show disk
 * usage" on every drive.
 */
const [diskUsage, setDiskUsage] = createSignal<((path: string) => void) | null>(null);

export function setDiskUsageHandler(open: ((path: string) => void) | null) {
  setDiskUsage(() => open);
}

/** Whether "Show disk usage" can be offered. */
export const canAnalyse = () => diskUsage() !== null;

export function analyseDiskUsage(path: string) {
  diskUsage()?.(path);
}

// ---------- opening places ----------

/** Opens a folder or location in the active tab, or a new one. */
export function openLocation(address: string, inNewTab = false) {
  const tab = activeTab();
  if (inNewTab || !tab) openTab(address);
  else navigate(tab.id, address);
}

/**
 * Goes to what was typed in the address bar: a special location by any of its names, or a
 * path written any way Explorer takes it (lib/placesApi's `addressResolve`). A file opens its
 * folder with the file selected, as in Explorer. Paths are relative to the tab's folder.
 */
export async function openAddress(tabId: number, typed: string) {
  const text = typed.trim();
  if (!text) return;
  const location = resolveLocation(text);
  if (location !== null) return navigate(tabId, location);
  const tab = tabs.find((candidate) => candidate.id === tabId);
  const base = tab && !locationOf(tab.path) ? tab.path : null;
  try {
    const resolved = await placesApi.addressResolve(text, base);
    // An archive opens as a folder, as a double-click does (archives.ts); other files are shown in theirs.
    if (resolved.kind === "file" && !isArchiveFile({ dir: false, name: resolved.name })) {
      return void (await revealFile(tabId, resolved.folder, resolved.name));
    }
    navigate(tabId, resolved.path);
  } catch (error) {
    notify(error);
  }
}

/** How long a file typed in the address bar waits for its folder to be listed. */
const REVEAL_WAIT_MS = 5000;

/** Opens `folder` in the tab and selects `name` there once it is listed. */
async function revealFile(tabId: number, folder: string, name: string) {
  navigate(tabId, folder);
  for (let waited = 0; waited < REVEAL_WAIT_MS; waited += 50) {
    const tab = tabs.find((candidate) => candidate.id === tabId);
    if (!tab) return;
    // Arriving clears any selection asked for before, so it is asked for once there.
    if (tab.listing && !isSearch(tab) && samePlace(tab.listing.path, folder)) return selectInFolder(folder, [name]);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

// ---------- the sidebar's settings ----------

const [collapsed, setCollapsed] = createSignal<ReadonlySet<string>>(new Set());
const [expandToOpen, setExpandToOpenSignal] = createSignal(true);
/** Sidebar sections the user has collapsed, by name. */
export { collapsed as collapsedSections, expandToOpen };

export function setSectionCollapsed(section: string, closed: boolean) {
  const next = new Set(collapsed());
  if (closed) next.add(section);
  else next.delete(section);
  setCollapsed(next);
  void placesApi.setSidebarSection(section, closed).catch((error) => console.error("sidebar:", error));
}

/** "Expand to open folder": the tree reveals the folder the tab shows. */
export function setExpandToOpen(on: boolean) {
  setExpandToOpenSignal(on);
  void placesApi.setExpandToOpenFolder(on).catch(notify);
}

let started = false;

/** Reads what places need and follows the drives. Call once, in the main window. */
export function startPlaces() {
  if (started) return;
  started = true;
  void placesApi
    .sidebarState()
    .then((state) => {
      setCollapsed(new Set(state.collapsed));
      setExpandToOpenSignal(state.expand_to_open_folder);
    })
    .catch((error) => console.error("sidebar:", error));
  void loadThisPc();
  void placesApi.onDrivesChanged(() => void loadThisPc());
  void placesApi.onDriveDetails(updateDrive);
  // Free space changes with use; coming back to the window brings it up to date.
  window.addEventListener("focus", () => void loadThisPc());
  // Development builds let the end-to-end tests read the bin and the drives, to check that
  // the items they did not touch are still there. Reading only: no hook acts on the bin.
  if (import.meta.env.DEV) {
    Object.assign(window, {
      __kestrelPlaces: {
        binItems: () => placesApi.binItems(),
        thisPc: () => thisPc(),
        sidebar: () => [...collapsed()],
        // Stands in for the disk usage hub, which another feature provides.
        setDiskUsageHandler,
      },
    });
  }
}
