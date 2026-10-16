/**
 * The backend commands behind the window's layout (`src-tauri/src/layout.rs`): grouping, the
 * filter box, copying and moving to the other pane, image sizes for the Content view, the second
 * pane's folder watch, and the layout kept in the database between runs.
 */
import { invoke } from "@tauri-apps/api/core";

import type { Listing, Marks, RowRange, SortSpec } from "./api";

/** Explorer's "Group by" choices, apart from none. */
export type GroupBy = "name" | "modified" | "type" | "size";

export interface GroupSpec {
  by: GroupBy;
  /** Newest, largest or Z first. */
  desc: boolean;
}

/** A group of a listing, as its header shows it. */
export interface GroupHeader {
  label: string;
  start: number;
  count: number;
}

export const layoutApi = {
  /**
   * Sorts a set and groups it, or ungroups it with `null`, resolving to where the marked rows
   * went and where each group starts.
   */
  groupSet: (set: number, sort: SortSpec, group: GroupSpec | null, marks: Marks) =>
    invoke<{ marks: Marks; starts: number[] }>("group_set", { set, sort, group, marks }),
  /** The names and sizes of `count` groups from the group `from`. */
  groupHeaders: (set: number, from: number, count: number) =>
    invoke<GroupHeader[]>("group_headers", { set, from, count }),
  /**
   * A new set with the rows of `base` whose names contain `text` (or match it, with `*` or `?`),
   * sorted and grouped as given. `base` is left as it is.
   */
  filterSet: (base: number, text: string, sort: SortSpec, group: GroupSpec | null) =>
    invoke<{ listing: Listing; starts: number[] }>("filter_set", { base, text, sort, group }),
  /** Copies or moves rows into a folder, as one undoable operation; resolves to the names they got. */
  transferItems: (set: number, ranges: RowRange[], destination: string, mode: "copy" | "move") =>
    invoke<string[]>("transfer_items", { set, ranges, destination, mode }),
  /** Pixel sizes of images, read from their headers; null for anything else or online-only files. */
  imageSizes: (paths: string[]) => invoke<([number, number] | null)[]>("image_sizes", { paths }),
  /** Watches the other pane's folder from now on, or nothing with `null`. */
  watchOtherPane: (path: string | null) => invoke<void>("watch_other_pane", { path }),
  /** Watches a folder as the active tab's, as listing it with `watch` would, without listing it. */
  watchActiveFolder: (path: string) => invoke<void>("watch_active_folder", { path }),
  /** For each path, whether it is a folder that exists. */
  foldersExist: (paths: string[]) => invoke<boolean[]>("folders_exist", { paths }),
  /** Every saved part of the layout, as `[name, json]`. */
  load: () => invoke<[string, string][]>("layout_load"),
  /** Saves one part of the layout; an empty value forgets it. */
  save: (name: string, value: string) => invoke<void>("layout_save", { name, value }),
};
