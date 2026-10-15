import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type SortKey = "name" | "size" | "modified" | "type";

export interface SortSpec {
  key: SortKey;
  desc: boolean;
}

export interface Listing {
  /** Handle for fetching rows. */
  set: number;
  total: number;
  /** Folder shown, as capitalised on disk. Empty for search results. */
  path: string;
  max_size: number;
  took_us: number;
}

/** An inclusive range of row positions. */
export type RowRange = [number, number];

/** The selected rows and the focused row of a result set. */
export interface Marks {
  ranges: RowRange[];
  focus: number | null;
}

export interface Row {
  name: string;
  path: string;
  /** Containing folder; present for search results only. */
  location: string | null;
  dir: boolean;
  size: number;
  /** Milliseconds since the Unix epoch. */
  mtime: number;
  ext: string;
  hidden: boolean;
}

export interface IndexStatus {
  state: "loading" | "scanning" | "ready";
  entries: number;
  refreshing: boolean;
  generation: number;
}

export interface Folder {
  name: string;
  kind: string;
  path: string;
}

export interface Drive {
  path: string;
  label: string;
  kind: "fixed" | "removable" | "network" | "optical";
  total: number;
  free: number;
}

export interface Places {
  home: string;
  folders: Folder[];
  drives: Drive[];
}

/** What the user chose from the Windows context menu. */
export interface ShellChoice {
  /** Canonical verb of the chosen command, such as "copy", if it has one. */
  verb: string | null;
  /** False if nothing was chosen, or if the command is Kestrel's to carry out (rename). */
  invoked: boolean;
}

export const api = {
  indexStatus: () => invoke<IndexStatus>("index_status"),
  /**
   * Lists a folder. With `watch`, the folder is watched from then on, replacing any earlier
   * watch, and `onDirChanged` fires when its contents change.
   */
  listDir: (path: string, sort: SortSpec, watch: boolean) =>
    invoke<Listing>("list_dir", { path, sort, watch }),
  search: (query: string, scope: string | null, sort: SortSpec) =>
    invoke<Listing>("search", { query, scope, sort }),
  /** Maps a selection from one result set to another that shows the same files. */
  carryMarks: (from: number, to: number, marks: Marks) => invoke<Marks>("carry_marks", { from, to, marks }),
  /** Frees a result set. Call only once the UI shows something else. */
  releaseSet: (set: number) => invoke<void>("release_set", { set }),
  rows: (set: number, start: number, count: number) => invoke<Row[]>("rows", { set, start, count }),
  /** Re-sorts a set in place and resolves to where the marked rows moved. */
  sortSet: (set: number, sort: SortSpec, marks: Marks) => invoke<Marks>("sort_set", { set, sort, marks }),
  locateNames: (set: number, names: string[]) => invoke<RowRange[]>("locate_names", { set, names }),
  /** Recycles the rows, or erases them when `permanent`. Resolves to false if cancelled. */
  deleteItems: (set: number, ranges: RowRange[], permanent: boolean) =>
    invoke<boolean>("delete_items", { set, ranges, permanent }),
  renameItem: (path: string, newName: string) => invoke<boolean>("rename_item", { path, newName }),
  /** Creates a folder and resolves to the name it was given. */
  newFolder: (parent: string) => invoke<string>("new_folder", { parent }),
  /** Puts the rows on the clipboard as files; `cut` marks them to be moved on paste. */
  copyItems: (set: number, ranges: RowRange[], cut: boolean) =>
    invoke<number>("copy_items", { set, ranges, cut }),
  canPaste: () => invoke<boolean>("can_paste"),
  /** Pastes the clipboard's files and resolves to the names pasted. */
  pasteItems: (destination: string) => invoke<string[]>("paste_items", { destination }),
  showProperties: (path: string) => invoke<void>("show_properties", { path }),
  onDirChanged: (handler: (path: string) => void) =>
    listen<string>("dir-changed", (event) => handler(event.payload)),
  findPrefix: (set: number, prefix: string, from: number) =>
    invoke<number | null>("find_prefix", { set, prefix, from }),
  places: () => invoke<Places>("places"),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  onIndexStatus: (handler: (status: IndexStatus) => void) =>
    listen<IndexStatus>("index-status", (event) => handler(event.payload)),
  /**
   * Shows the Windows context menu for the rows at a point in the window, from a helper
   * process, and resolves once the chosen command is done. `dark` matches it to the window.
   */
  shellMenu: (set: number, ranges: RowRange[], x: number, y: number, dark: boolean, extended: boolean) =>
    invoke<ShellChoice>("shell_menu", { set, ranges, x, y, dark, extended }),
  /** Shows the Windows context menu of the folder itself, as for a click on empty space. */
  shellFolderMenu: (path: string, x: number, y: number, dark: boolean, extended: boolean) =>
    invoke<ShellChoice>("shell_folder_menu", { path, x, y, dark, extended }),
  openWith: (path: string) => invoke<void>("open_with", { path }),
};

/** Extensions whose files each carry their own icon rather than sharing one per type. */
const OWN_ICON = new Set(["exe", "lnk", "ico", "url", "msi", "scr", "cur", "ani", "appref-ms"]);

/** URL of the system icon for a row, served by the app's `icon` protocol. */
export function iconUrl(row: Pick<Row, "dir" | "ext" | "path">, size = 32): string {
  const base = "http://icon.localhost";
  if (row.dir) return `${base}/folder?s=${size}`;
  if (OWN_ICON.has(row.ext)) return `${base}/file?s=${size}&p=${encodeURIComponent(row.path)}`;
  return `${base}/ext/${encodeURIComponent(row.ext || "_")}?s=${size}`;
}
