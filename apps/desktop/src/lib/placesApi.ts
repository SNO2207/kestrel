/**
 * The backend commands of This PC, the Recycle Bin, the folder tree and the address bar
 * (`thispc.rs`, `recyclebin.rs`, `foldertree.rs`, `address.rs`). Kept apart from api.ts so the
 * feature's additions stay in files of its own.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { Folder } from "./api";
import { showProtected } from "./folderOptions";

export type DriveKind = "fixed" | "removable" | "optical" | "network" | "ram";
export type DriveState = "ready" | "reading" | "no-media" | "disconnected" | "unavailable";

/** A drive as This PC shows it. Mirrors `thispc::Drive`. */
export interface DriveInfo {
  /** The root, such as `C:\`. */
  path: string;
  letter: string;
  kind: DriveKind;
  label: string;
  file_system: string;
  total: number;
  /** Free space this account can use. */
  free: number;
  /** For a mapped network drive, its share (`\\server\share`). */
  remote: string | null;
  state: DriveState;
  /** The name Explorer gives it, such as `Windows (C:)`. */
  name: string;
}

export interface ThisPcData {
  folders: Folder[];
  drives: DriveInfo[];
}

/** An item in the Recycle Bin. Mirrors `recyclebin::BinItem`. */
export interface BinItem {
  /** The item's own file in the bin, which names it to every command. */
  id: string;
  name: string;
  /** The full path it was deleted from. */
  original: string;
  /** The folder it was deleted from. */
  folder: string;
  /** Milliseconds since the Unix epoch. */
  deleted: number;
  size: number;
  type_name: string;
  dir: boolean;
  ext: string;
}

export interface Restored {
  paths: string[];
  missing: number;
  completed: boolean;
}

export interface Erased {
  erased: number;
  left: number;
}

/** A folder in the tree. Mirrors `foldertree::TreeFolder`. */
export interface TreeFolder {
  name: string;
  hidden: boolean;
  /** Whether it holds folders; null when that isn't known without reading the disk. */
  children: boolean | null;
}

export interface TreeListing {
  path: string;
  folders: TreeFolder[];
  live: boolean;
  error: string | null;
}

export interface SidebarState {
  collapsed: string[];
  expand_to_open_folder: boolean;
}

export type Resolved =
  | { kind: "folder"; path: string }
  | { kind: "file"; path: string; folder: string; name: string }
  | { kind: "unknown"; path: string };

export interface Suggestion {
  path: string;
  name: string;
  hint: string | null;
}

export interface Suggestions {
  parent: string | null;
  items: Suggestion[];
}

export const placesApi = {
  thisPc: () => invoke<ThisPcData>("this_pc"),
  ejectDrive: (path: string) => invoke<void>("eject_drive", { path }),
  /** The Windows menu for items given by path, as for the selection in the list. */
  shellPathsMenu: (paths: string[], x: number, y: number, dark: boolean, extended: boolean) =>
    invoke<{ verb: string | null; invoked: boolean }>("shell_paths_menu", { paths, x, y, dark, extended }),
  onDrivesChanged: (handler: () => void): Promise<UnlistenFn> => listen("drives-changed", () => handler()),
  onDriveDetails: (handler: (drive: DriveInfo) => void): Promise<UnlistenFn> =>
    listen<DriveInfo>("drive-details", (event) => handler(event.payload)),

  binItems: () => invoke<BinItem[]>("recycle_bin_items"),
  binStamp: () => invoke<string>("recycle_bin_stamp"),
  /** Restores the items with these ids, as one undoable operation. */
  binRestore: (ids: string[]) => invoke<Restored>("recycle_bin_restore", { ids }),
  /** Deletes the items with these ids for good. Ask first. */
  binDelete: (ids: string[]) => invoke<Erased>("recycle_bin_delete", { ids }),
  /** Empties the whole bin. Ask first. */
  binEmpty: () => invoke<void>("recycle_bin_empty"),

  treeChildren: (paths: string[], showHidden: boolean) => invoke<TreeListing[]>("tree_children", { paths, showHidden, showProtected: showProtected() }),
  treeReveal: (path: string, showHidden: boolean) => invoke<TreeListing[]>("tree_reveal", { path, showHidden, showProtected: showProtected() }),
  treeSubtree: (path: string, showHidden: boolean) => invoke<string>("tree_subtree", { path, showHidden, showProtected: showProtected() }),
  onFoldersChanged: (handler: (revision: number) => void): Promise<UnlistenFn> =>
    listen<number>("folders-changed", (event) => handler(event.payload)),
  sidebarState: () => invoke<SidebarState>("sidebar_state"),
  setSidebarSection: (section: string, collapsed: boolean) => invoke<void>("set_sidebar_section", { section, collapsed }),
  setExpandToOpenFolder: (on: boolean) => invoke<void>("set_expand_to_open_folder", { on }),

  addressResolve: (text: string, base: string | null) => invoke<Resolved>("address_resolve", { text, base }),
  addressSuggest: (text: string, base: string | null, showHidden: boolean) =>
    invoke<Suggestions>("address_suggest", { text, base, showHidden, showProtected: showProtected() }),
};
