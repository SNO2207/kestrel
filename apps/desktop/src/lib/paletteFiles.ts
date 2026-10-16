/**
 * The front end half of the palette's files and folders, which the backend finds (the index
 * for plain queries, the disk for paths typed after `/`): what choosing one does.
 *
 * Enter opens a folder in the current tab and a file in its default app; Ctrl+Enter opens a
 * folder in a new tab; Shift+Enter shows either in the folder that holds it, selected. Tab puts
 * the item's path in the input after `/`, so a folder's contents can be listed and narrowed.
 */
import { activeTab, isSearch, navigate, notify, openTab, selectInFolder } from "../state";
import { api } from "./api";
import { baseName, parentPath } from "./format";
import type { PaletteHit, PaletteProvider } from "./palette";

const ID = "files";
/** How long showing an item in its folder waits for the folder to be listed. */
const REVEAL_WAIT_MS = 5000;

const samePath = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
const pathOf = (hit: PaletteHit) => hit.path ?? hit.id;

/** The path with a separator after a folder, ready for its contents to be typed. */
function completion(hit: PaletteHit): string {
  const path = pathOf(hit);
  return `/${hit.dir && !path.endsWith("\\") ? `${path}\\` : path}`;
}

/** Shows `path` in the folder that holds it, selected, in the active tab. */
function reveal(path: string) {
  const parent = parentPath(path);
  const tab = activeTab();
  if (!parent || !tab) return;
  navigate(tab.id, parent);
  const started = Date.now();
  const wait = () => {
    const now = activeTab();
    if (now && now.listing && !isSearch(now) && samePath(now.path, parent)) return selectInFolder(parent, [baseName(path)]);
    if (Date.now() - started < REVEAL_WAIT_MS) window.setTimeout(wait, 50);
  };
  wait();
}

export const filesProvider: PaletteProvider = {
  id: ID,
  label: (hit) => (hit.dir ? "Folder" : "File"),
  run: (hit, mode) => {
    const path = pathOf(hit);
    if (mode === "reveal") return reveal(path);
    if (!hit.dir) return void api.openPath(path).catch(notify);
    if (mode === "new-tab") return openTab(path);
    const tab = activeTab();
    if (tab) navigate(tab.id, path);
    else openTab(path);
  },
  // Opening a file needs no window of Kestrel's; the quick launcher does that itself.
  runsAnywhere: (hit, mode) => !hit.dir && mode !== "reveal",
  complete: completion,
};

// Folder visits are not recorded here: the shortcuts feature records each one once
// (`frecency_visit`), and the palette reads them from there (`kestrel_db::parity`).
