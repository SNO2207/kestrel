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

/** Modifier keys held during a drag, which decide between copying and moving. */
export interface DropKeys {
  ctrl: boolean;
  shift: boolean;
}

export type DropEffect = "copy" | "move" | "link" | "none";

export type DragRehearsal = "prepare" | "cancel" | "hold";

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
   * Starts a native drag of the rows while the mouse button is down, resolving when it ends.
   * A `rehearsal` (honoured by dev builds only) lets tests run it without a mouse: `prepare`
   * stops short of the drag, `cancel` calls the drag off as it begins, and `hold` calls it
   * off after a couple of seconds.
   */
  startDrag: (set: number, ranges: RowRange[], rehearsal?: DragRehearsal) =>
    invoke<{ effect: DropEffect; items: number }>("start_drag", { set, ranges, rehearsal }),
  /**
   * Copies or moves dropped files into `destination`, by the keys held at the drop. `keys`
   * (honoured by dev builds only) stands in for the keyboard in tests.
   */
  dropItems: (paths: string[], destination: string, keys?: DropKeys) =>
    invoke<{ effect: DropEffect; names: string[] }>("drop_items", { paths, destination, keys }),
  /** What dropping `paths` on `destination` would do with the keys held now. */
  dropEffect: (paths: string[], destination: string, keys?: DropKeys) =>
    invoke<DropEffect>("drop_effect", { paths, destination, keys }),
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
