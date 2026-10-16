import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { showProtected } from "./folderOptions";
import type { Hit } from "./searchApi";
import { showHidden } from "./views";

/** The last four sort by the details view's extra columns (lib/columns.ts). */
export type SortKey = "name" | "size" | "modified" | "type" | "alloc" | "attributes" | "extension" | "path";

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
  /** For a folder inside an archive, the archive's path on disk; such a listing is read-only. */
  archive?: string;
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
  /** Bytes allocated on disk. */
  alloc: number;
  /** Windows file attribute bits: the Attributes column, and a cloud file's sync state (`lib/syncStatus.ts`). */
  attrs: number;
  /** For a result of a search inside files, its first matching line (`lib/searchApi.ts`). */
  hit?: Hit;
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

/** What Undo and Redo would do now, such as "rename of report.txt". */
export interface UndoState {
  undo: string | null;
  redo: string | null;
  /** Set when the latest operation was a permanent deletion, which can't be undone. */
  erased: string | null;
}

/** What an undo or redo did, and where the items it put back now are. */
export interface UndoReport {
  message: string;
  folder: string | null;
  names: string[];
}

export type RenameCase = "keep" | "lower" | "upper" | "title";
export type RenamePart = "name" | "extension" | "both";

/** How a batch rename makes each new name. */
export interface RenameRules {
  find: string;
  replace: string;
  regex: boolean;
  matchCase: boolean;
  prefix: string;
  suffix: string;
  /** Replaces the name when set: `#` runs take the number, `*` the current name. */
  numbering: string;
  start: number;
  case: RenameCase;
  part: RenamePart;
}

/** One item's name before and after a batch rename, and why the new one can't be used. */
export interface RenamePreview {
  path: string;
  old: string;
  new: string;
  problem: string | null;
}

/** The start of a file as text, for the preview pane. */
export interface TextPreview {
  text: string;
  /** Only the first megabyte was read. */
  truncated: boolean;
  encoding: "utf-8" | "utf-16le" | "utf-16be";
  /** Invalid bytes were replaced. */
  lossy: boolean;
  /** The file does not look like text; `text` is empty. */
  binary: boolean;
  size: number;
}

/** What the preview pane's details block shows about a file or folder. */
export interface PreviewDetails {
  name: string;
  path: string;
  dir: boolean;
  /** Bytes; for a folder, the total from the index, or null if the index lacks it. */
  size: number | null;
  /** Milliseconds since the Unix epoch, or 0 when unknown. */
  created: number;
  modified: number;
  accessed: number;
  attributes: string[];
  /** The contents live in the cloud, and reading them downloads the file. */
  online_only: boolean;
  type_name: string;
  dimensions: [number, number] | null;
  folder: { files: number; folders: number; readable: boolean } | null;
}

/** Modifier keys held during a drag, which decide between copying and moving. */
export interface DropKeys {
  ctrl: boolean;
  shift: boolean;
  /** Makes shortcuts, as in Explorer. */
  alt?: boolean;
}

export type DropEffect = "copy" | "move" | "link" | "none";

export type DragRehearsal = "prepare" | "cancel" | "hold";

/**
 * Where the preview's own webview goes, in CSS pixels of the page, and what it shows: a
 * `preview.localhost` page. See `previewview.rs`.
 */
export interface SurfacePlacement {
  url: string;
  x: number;
  y: number;
  width: number;
  height: number;
  visible: boolean;
}

/** What the user chose from the Windows context menu. */
export interface ShellChoice {
  /** Canonical verb of the chosen command, such as "copy", if it has one. */
  verb: string | null;
  /** False if nothing was chosen, or if the command is Kestrel's to carry out (rename). */
  invoked: boolean;
}

/**
 * The last command that puts files on the clipboard, until it is done. Commands run on a pool of
 * threads, so a paste sent while a copy was still on its way (Ctrl+C, then Ctrl+V at once) could
 * read the clipboard first and paste what was on it before; every paste waits for it.
 */
let clipboardWrite: Promise<unknown> = Promise.resolve();
/** Clipboard writes on their way, pastes waiting for them, and pastes done (development builds show them). */
let writing = 0;
let waiting = 0;
let pasted = 0;
/** Development builds: clipboard writes wait here while a test holds them, before they are sent. */
let writeGate: Promise<void> | null = null;
let openWriteGate = () => {};

/** Sends a command that puts files on the clipboard (`send`), as the write pastes wait for. */
export function writesClipboard<T>(send: () => Promise<T>): Promise<T> {
  writing++;
  const write = import.meta.env.DEV && writeGate ? writeGate.then(send) : send();
  clipboardWrite = write.then(
    () => void writing--,
    () => void writing--,
  );
  return write;
}

/** Waits for the clipboard's last write before pasting. */
function afterClipboardWrite<T>(paste: () => Promise<T>): Promise<T> {
  waiting++;
  return clipboardWrite
    .then(() => {
      waiting--;
      return paste();
    })
    .finally(() => void pasted++);
}

if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelClipboard: {
      /** Holds clipboard writes before they are sent, or sends those held, so a test can paste while a copy is on its way. */
      hold: (hold: boolean) => {
        if (hold && !writeGate) writeGate = new Promise((resolve) => (openWriteGate = resolve));
        if (!hold) {
          writeGate = null;
          openWriteGate();
        }
      },
      writing: () => writing,
      waiting: () => waiting,
      pasted: () => pasted,
    },
  });
}

export const api = {
  indexStatus: () => invoke<IndexStatus>("index_status"),
  /**
   * Lists a folder. With `watch`, the folder is watched from then on, replacing any earlier
   * watch, and `onDirChanged` fires when its contents change. Hidden items are
   * included only when `hidden` is true, which defaults to the "Show hidden items" option.
   * `ticket` (from `nextJobId`) lets the window cancel the listing with `archiveApi.cancel`:
   * listing a large compressed archive reads it through.
   */
  listDir: (path: string, sort: SortSpec, watch: boolean, hidden: boolean = showHidden(), ticket?: number) =>
    invoke<Listing>("list_dir", { path, sort, watch, showHidden: hidden, showProtected: showProtected(), ticket: ticket ?? null }),
  search: (query: string, scope: string | null, sort: SortSpec, hidden: boolean = showHidden()) =>
    invoke<Listing>("search", { query, scope, sort, showHidden: hidden, showProtected: showProtected() }),
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
  /** Renames an item and resolves to the name it ended up with, or null if nothing changed. */
  renameItem: (path: string, newName: string) => invoke<string | null>("rename_item", { path, newName }),
  /** Creates a folder and resolves to the name it was given. */
  newFolder: (parent: string) => invoke<string>("new_folder", { parent }),
  /** Puts the rows on the clipboard as files; `cut` marks them to be moved on paste. */
  copyItems: (set: number, ranges: RowRange[], cut: boolean) =>
    writesClipboard(() => invoke<number>("copy_items", { set, ranges, cut })),
  canPaste: () => invoke<boolean>("can_paste"),
  /** Pastes the clipboard's files, once any copy on its way is done, and resolves to the names pasted. */
  pasteItems: (destination: string) => afterClipboardWrite(() => invoke<string[]>("paste_items", { destination })),
  showProperties: (path: string) => invoke<void>("show_properties", { path }),
  onDirChanged: (handler: (path: string) => void) =>
    listen<string>("dir-changed", (event) => handler(event.payload)),
  findPrefix: (set: number, prefix: string, from: number) =>
    invoke<number | null>("find_prefix", { set, prefix, from }),
  places: () => invoke<Places>("places"),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  /** Reads at most the first megabyte of a file as text. */
  previewText: (path: string) => invoke<TextPreview>("preview_text", { path }),
  previewDetails: (path: string) => invoke<PreviewDetails>("preview_details", { path }),
  /** Opens an http, https or mailto link in the default app; anything else is refused. */
  openLink: (url: string) => invoke<void>("open_link", { url }),
  onIndexStatus: (handler: (status: IndexStatus) => void) =>
    listen<IndexStatus>("index-status", (event) => handler(event.payload)),
  undoState: () => invoke<UndoState>("undo_state"),
  /** Reverses the latest file operation, with the shell's progress and conflict windows. */
  undo: () => invoke<UndoReport>("undo"),
  redo: () => invoke<UndoReport>("redo"),
  previewRenames: (paths: string[], rules: RenameRules) =>
    invoke<RenamePreview[]>("preview_renames", { paths, rules }),
  /** Renames every item by the rules as one undoable operation; resolves to the new names. */
  batchRename: (paths: string[], rules: RenameRules) => invoke<string[]>("batch_rename", { paths, rules }),
  /** For each folder, whether this account can't open it. */
  lockedFolders: (paths: string[]) => invoke<boolean[]>("locked_folders", { paths }),
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
  /** Places the preview's own webview and shows a page in it, or with `null` empties and hides it. */
  previewSurface: (placement: SurfacePlacement | null) => invoke<void>("preview_surface", { placement }),
  /** Takes the keyboard back from the preview's webview, if the window is still in front. */
  previewSurfaceReturnFocus: () => invoke<void>("preview_surface_return_focus"),
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

/**
 * URL of a file's thumbnail, at most `size` pixels square, served by the app's `thumb`
 * protocol. Files without one get their icon. The modification time is part of the URL, so a
 * changed file gets a fresh thumbnail while an unchanged one comes from the cache.
 */
export function thumbUrl(row: Pick<Row, "path" | "mtime">, size: number): string {
  return `http://thumb.localhost/?p=${encodeURIComponent(row.path)}&s=${size}&m=${row.mtime}`;
}
