/**
 * The backend commands of Explorer parity (`src-tauri/src/parity`), kept beside `api.ts` so
 * that file stays the core's.
 */
import { invoke } from "@tauri-apps/api/core";

import type { RowRange } from "./api";

/** One entry of the New menu: a file type Windows offers to create. */
export interface NewKind {
  /** The extension with its dot, such as ".docx", which names the entry. */
  id: string;
  ext: string;
  label: string;
  /** The name a new file gets, before its extension. */
  base: string;
  kind: "empty" | "data" | "template" | "shortcut";
}

/** One of Home's recent files. */
export interface RecentFile {
  path: string;
  name: string;
  location: string;
  /** When it was last opened, in milliseconds since the Unix epoch. */
  used: number;
  source: "windows" | "kestrel";
  /** Its contents are in the cloud only. */
  cloud: boolean;
}

/** A property of a file, as Windows labels and formats it. */
export interface Property {
  label: string;
  value: string;
}

/** What a selection holds, added up. */
export interface SelectionSummary {
  items: number;
  files: number;
  folders: number;
  bytes: number;
  /** Sizes and dates cover every item; huge selections are only counted. */
  complete: boolean;
  modified_from: number;
  modified_to: number;
}

/** A program start recorded by the projects feature's development hook. */
export interface RecordedLaunch {
  file: string;
  params: string;
  dir: string;
}

export const parityApi = {
  newMenu: () => invoke<NewKind[]>("new_menu"),
  /** Makes a new file of the type `kind` in `parent`; resolves to the name it got. */
  newFile: (parent: string, kind: string) => invoke<string>("new_file", { parent, kind }),
  /** Makes a shortcut in `folder` to `target`; resolves to the `.lnk` file's name. */
  newShortcut: (folder: string, target: string, name: string | null) =>
    invoke<string>("new_shortcut", { folder, target, name }),
  /** The name a shortcut to `target` would get, and whether the target exists. */
  shortcutSuggestion: (target: string) => invoke<[string, boolean]>("shortcut_suggestion", { target }),
  /** Home's recent files; `windows: false` leaves Windows' own Recent items out. */
  recent: (limit: number, windows = true) => invoke<RecentFile[]>("home_recent", { limit, windows }),
  recordOpened: (path: string) => invoke<void>("record_opened", { path }),
  forgetRecent: (path: string) => invoke<void>("forget_recent", { path }),
  openInTerminal: (folder: string) => invoke<void>("open_in_terminal", { folder }),
  itemProperties: (path: string) => invoke<Property[]>("item_properties", { path }),
  selectionSummary: (set: number, ranges: RowRange[]) => invoke<SelectionSummary>("selection_summary", { set, ranges }),
  syncRoots: () => invoke<string[]>("sync_roots_list"),
  /** Opens another window, in `path` if given; resolves to its label. */
  newWindow: (path: string | null) => invoke<string>("new_window", { path }),
  /** The folder this window was opened to show, if it was opened by another. */
  windowStart: () => invoke<string | null>("window_start"),
};
