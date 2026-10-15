/**
 * The backend commands of the shortcuts feature (pins, frequent folders, Go to folder, saved
 * searches, tags and workspaces), kept beside `api.ts` so that file stays the core's.
 */
import { invoke } from "@tauri-apps/api/core";

import type { SortSpec } from "./api";

/** Colours a group or tag may have; `shortcuts.css` gives each a swatch for both themes. */
export const COLORS = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "gray"] as const;
export type Color = (typeof COLORS)[number];

export interface PinGroup {
  id: number;
  name: string;
  color: Color | null;
  collapsed: boolean;
}

export interface Pin {
  id: number;
  /** `null` for pins above every group. */
  group: number | null;
  path: string;
  volume: string;
  file_id: number;
  dir: boolean;
  alias: string | null;
  /** The number key it claimed explicitly, if any. */
  slot: number | null;
  /** The number key (1 to 9) that opens it, claimed or given in order. */
  key: number | null;
  /** What it points at is no longer where it was. */
  missing: boolean;
}

export interface Board {
  groups: PinGroup[];
  /** In sidebar order: ungrouped first, then each group's. */
  pins: Pin[];
}

export interface Frequent {
  path: string;
  score: number;
}

export interface GotoHit {
  path: string;
  /** One of the frequent and recent folders, rather than only in the index. */
  frecent: boolean;
}

export interface SavedSearch {
  id: number;
  name: string;
  query: string;
  /** The folder it searches beneath, or `null` for everywhere. */
  scope: string | null;
}

export interface Tag {
  id: number;
  name: string;
  color: Color;
}

export interface TagsState {
  tags: Tag[];
  /** Every tagged item that could be found, at its current path. */
  items: { path: string; tags: number[] }[];
}

export interface WorkspaceTab {
  path: string;
  view: string | null;
  sort: SortSpec | null;
}

export interface Workspace {
  id: number;
  name: string;
  tabs: WorkspaceTab[];
  active: number;
  updated_at: number;
}

export const shortcutsApi = {
  board: () => invoke<Board>("pins_board"),
  /** Pins at the end of `group` (`null`: above every group); already pinned paths stay put. */
  pin: (paths: string[], group: number | null = null) =>
    invoke<{ added: number; existing: number }>("pins_add", { paths, group }),
  unpin: (id: number) => invoke<void>("pins_remove", { id }),
  setAlias: (id: number, alias: string | null) => invoke<void>("pins_set_alias", { id, alias }),
  /** Gives a pin its own number key, or `null` for the next one free. */
  setKey: (id: number, key: number | null) => invoke<void>("pins_set_key", { id, key }),
  movePin: (id: number, group: number | null, index: number) => invoke<void>("pins_move", { id, group, index }),
  /** Finds a missing pin's target by its file id and resolves to where it is now. */
  locatePin: (id: number) => invoke<string>("pins_locate", { id }),
  createGroup: (name: string, color: Color | null = null) => invoke<PinGroup>("pins_create_group", { name, color }),
  /** Changes a group; a colour of `""` takes its colour away. */
  updateGroup: (id: number, change: { name?: string; color?: Color | ""; collapsed?: boolean }) =>
    invoke<void>("pins_update_group", { id, ...change }),
  moveGroup: (id: number, index: number) => invoke<void>("pins_move_group", { id, index }),
  deleteGroup: (id: number) => invoke<void>("pins_delete_group", { id }),

  /** Records a visit to a folder; resolves to false for scratch folders, which are not kept. */
  visit: (path: string) => invoke<boolean>("frecency_visit", { path }),
  frequent: (limit: number) => invoke<Frequent[]>("frecency_top", { limit }),
  forgetFolder: (path: string) => invoke<boolean>("frecency_forget", { path }),
  goto: (query: string, limit?: number) => invoke<GotoHit[]>("goto_folders", { query, limit }),

  searches: () => invoke<SavedSearch[]>("searches_list"),
  saveSearch: (name: string, query: string, scope: string | null) =>
    invoke<SavedSearch>("searches_save", { name, query, scope }),
  renameSearch: (id: number, name: string) => invoke<void>("searches_rename", { id, name }),
  deleteSearch: (id: number) => invoke<void>("searches_delete", { id }),

  tags: () => invoke<TagsState>("tags_state"),
  createTag: (name: string, color: Color) => invoke<Tag>("tags_create", { name, color }),
  updateTag: (id: number, change: { name?: string; color?: Color }) => invoke<void>("tags_update", { id, ...change }),
  deleteTag: (id: number) => invoke<void>("tags_delete", { id }),
  /** Puts a tag on the paths, or takes it off with `on` false. */
  assignTag: (paths: string[], tag: number, on: boolean) => invoke<void>("tags_assign", { paths, tag, on }),

  workspaces: () => invoke<Workspace[]>("workspaces_list"),
  saveWorkspace: (name: string, tabs: WorkspaceTab[], active: number) =>
    invoke<Workspace>("workspaces_save", { name, tabs, active }),
  renameWorkspace: (id: number, name: string) => invoke<void>("workspaces_rename", { id, name }),
  deleteWorkspace: (id: number) => invoke<void>("workspaces_delete", { id }),
};
