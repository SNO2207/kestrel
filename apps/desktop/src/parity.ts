/**
 * Explorer parity: the actions and small pieces of state that the command bar, the menus, the
 * palette and the keyboard share. New > (every type Windows offers), New > Shortcut, Open in
 * Terminal, another window (Ctrl+N), full screen (F11), the details pane (Alt+Shift+P),
 * Select none and Invert selection, Open file location, and Folder Options.
 *
 * The components live in `components/` (`CommandBar`, `DetailsPane`, `HomeView`,
 * `FolderOptions`, `NewShortcutDialog`, `ParityLayer`); the backend in `src-tauri/src/parity`.
 */
import { createSignal } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type { MenuEntry } from "./components/ContextMenu";
import { iconUrl } from "./lib/api";
import { inArchive } from "./archives";
import { locationOf } from "./lib/locations";
import { parityApi, type NewKind } from "./lib/parityApi";
import { selector } from "./lib/selectionOps";
import { paneOpen, setPaneOpen } from "./previewState";
import { baseName, parentPath } from "./lib/format";
import { activeTab, isSearch, navigate, notify, refresh, renameWhenListed, selectInFolder, tabs, type Tab } from "./state";

const find = (tabId: number) => tabs.find((tab) => tab.id === tabId);

/**
 * Whether the tab shows a folder new things can be made in: not search results, a special
 * location (Home, This PC, the Recycle Bin), or the inside of an archive, which is read-only.
 */
export const inFolder = (tab: Tab | undefined): tab is Tab =>
  !!tab && !isSearch(tab) && !locationOf(tab.path) && !!tab.listing && !inArchive(tab.listing);

// ---------- New ----------

const [newKinds, setNewKinds] = createSignal<NewKind[]>([]);
let kindsAsked = 0;

/** The file types the New menu offers, read again at most once a minute. */
export function newMenuKinds(): NewKind[] {
  if (Date.now() - kindsAsked > 60_000) {
    kindsAsked = Date.now();
    void parityApi
      .newMenu()
      .then(setNewKinds)
      .catch(() => (kindsAsked = 0));
  }
  return newKinds();
}

/**
 * The New menu's entries for a tab: a folder, then a shortcut, then every file type Windows
 * offers, each with its icon. `newFolder` makes the folder, as Ctrl+Shift+N does.
 */
export function newEntries(tabId: number, newFolder: () => void): MenuEntry[] {
  const kinds = newMenuKinds();
  const shortcut = kinds.filter((kind) => kind.kind === "shortcut");
  const files = kinds.filter((kind) => kind.kind !== "shortcut");
  const entry = (kind: NewKind): MenuEntry => ({
    label: kind.label,
    // The type's icon: by extension even for types whose files carry their own (shortcuts).
    icon: `http://icon.localhost/ext/${encodeURIComponent(kind.ext)}?s=16`,
    run: () => void createNew(tabId, kind),
  });
  return [
    { label: "Folder", hint: "Ctrl+Shift+N", icon: iconUrl({ dir: true, ext: "", path: "" }, 16), run: newFolder },
    ...shortcut.map(entry),
    ...(files.length > 0 ? (["separator"] as MenuEntry[]) : []),
    ...files.map(entry),
  ];
}

/** The New shortcut dialog: open, for this folder. */
const [shortcutFolder, setShortcutFolder] = createSignal<{ tabId: number; folder: string } | null>(null);
export { shortcutFolder };
export const closeNewShortcut = () => setShortcutFolder(null);

/** Makes a new item of `kind` in the tab's folder and starts renaming it, as Explorer does. */
export async function createNew(tabId: number, kind: NewKind) {
  const tab = find(tabId);
  if (!inFolder(tab)) return;
  if (kind.kind === "shortcut") return setShortcutFolder({ tabId, folder: tab.path });
  try {
    const name = await parityApi.newFile(tab.path, kind.id);
    renameWhenListed(tabId, name);
  } catch (error) {
    notify(error);
  }
  refresh(tabId);
}

/** Makes a shortcut from the New shortcut dialog, then selects it where it was made. */
export async function createShortcut(target: string, name: string) {
  const at = shortcutFolder();
  if (!at) return;
  const made = await parityApi.newShortcut(at.folder, target, name.trim() || null);
  setShortcutFolder(null);
  selectInFolder(at.folder, [made]);
}

// ---------- opening ----------

/** Opens Windows Terminal in `folder`. */
export const openInTerminal = (folder: string) => void parityApi.openInTerminal(folder).catch(notify);

/** Opens another window in `path` (or where the active tab is), as Ctrl+N does in Explorer. */
export function openNewWindow(path?: string) {
  const tab = activeTab();
  const start = path ?? (tab && !isSearch(tab) ? tab.path : null);
  void parityApi.newWindow(start).catch(notify);
}

/** Shows `path` in the folder that holds it, selected, in the active tab: "Open file location". */
export function revealInFolder(path: string) {
  const parent = parentPath(path);
  const tab = activeTab();
  if (!parent || !tab) return;
  navigate(tab.id, parent);
  const started = Date.now();
  const wait = () => {
    const now = find(tab.id);
    // Once the folder is listed, the item is selected there (and the folder listed again,
    // which is what makes a list select what it is waiting for).
    if (now?.listing && !isSearch(now) && now.listing.path.toLowerCase() === parent.toLowerCase()) {
      return selectInFolder(parent, [baseName(path)]);
    }
    if (Date.now() - started < 5000) window.setTimeout(wait, 50);
  };
  wait();
}

// ---------- the window ----------

/** F11: full screen, and back. */
export async function toggleFullScreen() {
  const window = getCurrentWindow();
  try {
    await window.setFullscreen(!(await window.isFullscreen()));
  } catch (error) {
    notify(error);
  }
}

// ---------- selection ----------

export const selectNone = (tabId: number) => selector(tabId)?.none();
export const selectAll = (tabId: number) => selector(tabId)?.all();
export const invertSelection = (tabId: number) => selector(tabId)?.invert();
export const renameSelection = (tabId: number) => selector(tabId)?.rename();

// ---------- the details pane ----------

const DETAILS_KEY = "details-open";
function remembered(): boolean {
  try {
    return localStorage.getItem(DETAILS_KEY) === "1";
  } catch {
    return false;
  }
}

const [detailsOpen, setDetailsOpenSignal] = createSignal(remembered() && !paneOpen());
export { detailsOpen };

/** Shows or hides the details pane, which shares the right of the window with the preview. */
export function setDetailsOpen(open: boolean) {
  setDetailsOpenSignal(open);
  if (open && paneOpen()) setPaneOpen(false);
  try {
    localStorage.setItem(DETAILS_KEY, open ? "1" : "0");
  } catch {
    // Not remembered, which is harmless.
  }
}

export const toggleDetails = () => setDetailsOpen(!detailsOpen());

// ---------- Folder Options ----------

const [optionsOpen, setOptionsOpen] = createSignal(false);
export { optionsOpen, setOptionsOpen };
