/**
 * View modes and view options.
 *
 * Each folder remembers its own view mode and sort, as in Explorer, and search results share
 * one memory of their own. The two options, "Show hidden items" and "Show file extensions",
 * apply everywhere. Everything is kept in localStorage, so it survives restarts.
 */
import { createSignal } from "solid-js";

import type { Listing, Row, SortSpec } from "./api";

export type ViewMode = "xl" | "large" | "medium" | "small" | "list" | "details" | "tiles";

export interface ViewModeInfo {
  mode: ViewMode;
  label: string;
  /** The digit of its Ctrl+Shift shortcut, which matches Explorer's. */
  digit: number;
}

/** Largest first, which is also Explorer's order, so Ctrl+wheel steps through them in turn. */
export const VIEW_MODES: readonly ViewModeInfo[] = [
  { mode: "xl", label: "Extra large icons", digit: 1 },
  { mode: "large", label: "Large icons", digit: 2 },
  { mode: "medium", label: "Medium icons", digit: 3 },
  { mode: "small", label: "Small icons", digit: 4 },
  { mode: "list", label: "List", digit: 5 },
  { mode: "details", label: "Details", digit: 6 },
  { mode: "tiles", label: "Tiles", digit: 8 },
];

export const DEFAULT_VIEW: ViewMode = "details";

const isViewMode = (value: unknown): value is ViewMode => VIEW_MODES.some((info) => info.mode === value);

/** The view a Ctrl+Shift+digit shortcut selects, or `null` if the event is not one. */
export function shortcutView(event: KeyboardEvent): ViewMode | null {
  if (!event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return null;
  // The code, not the key: with Shift held, the key is a symbol such as "!".
  const digit = /^Digit(\d)$/.exec(event.code)?.[1];
  return VIEW_MODES.find((info) => String(info.digit) === digit)?.mode ?? null;
}

// ---------- per-folder memory ----------

/** What a folder, or search results, last looked like. */
export interface ViewMemory {
  mode?: ViewMode;
  sort?: SortSpec;
}

type Stored = Record<string, ViewMemory & { at: number }>;

const MEMORY_KEY = "kestrel.views";
/** Folders remembered at most; the ones changed longest ago are forgotten first. */
const MAX_REMEMBERED = 2000;
/** The memory shared by all search results. */
export const SEARCH_LOCATION = "search:";

function load<T>(key: string, fallback: T): T {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(key) ?? "null");
    return parsed !== null && typeof parsed === "object" ? (parsed as T) : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage is full or unavailable; the setting still lasts for this session.
  }
}

const memory: Stored = load<Stored>(MEMORY_KEY, {});
/** Bumped on every change, so readers of the memory update. */
const [revision, setRevision] = createSignal(0);

/** A folder's key in the memory: its path in lower case, without a trailing separator. */
export function folderKey(path: string): string {
  const trimmed = path.replace(/\//g, "\\").replace(/\\+$/, "");
  return (trimmed.endsWith(":") ? `${trimmed}\\` : trimmed).toLowerCase();
}

/** Where a tab is, for the memory: its folder, or search results once it shows them. */
export function locationKey(tab: { path: string; listing: Listing | null }): string {
  return tab.listing?.path === "" ? SEARCH_LOCATION : folderKey(tab.path);
}

/** The location a listing shows. */
export const listingLocation = (listing: Listing | null): string | null =>
  listing === null ? null : listing.path === "" ? SEARCH_LOCATION : folderKey(listing.path);

const isSort = (value: unknown): value is SortSpec =>
  typeof value === "object" &&
  value !== null &&
  ["name", "size", "modified", "type"].includes((value as SortSpec).key) &&
  typeof (value as SortSpec).desc === "boolean";

/** What is remembered for a location; empty for one never changed. */
export function recall(location: string): ViewMemory {
  revision();
  // Stored values are checked, since an older or hand-edited store may hold anything.
  const stored: ViewMemory = memory[location] ?? {};
  return {
    mode: isViewMode(stored.mode) ? stored.mode : undefined,
    sort: isSort(stored.sort) ? stored.sort : undefined,
  };
}

export function remember(location: string, change: ViewMemory) {
  memory[location] = { ...memory[location], ...change, at: Date.now() };
  const keys = Object.keys(memory);
  if (keys.length > MAX_REMEMBERED) {
    keys
      .sort((a, b) => (memory[a].at ?? 0) - (memory[b].at ?? 0))
      .slice(0, keys.length - MAX_REMEMBERED)
      .forEach((key) => delete memory[key]);
  }
  save(MEMORY_KEY, memory);
  setRevision((n) => n + 1);
}

/** The view mode of a location; details for one never changed. */
export const viewModeAt = (location: string | null): ViewMode =>
  (location === null ? undefined : recall(location).mode) ?? DEFAULT_VIEW;

export const setViewMode = (location: string, mode: ViewMode) => remember(location, { mode });

// ---------- options that apply everywhere ----------

interface Options {
  /** Hidden and system items are listed. */
  hidden: boolean;
  /** File names are shown with their extensions. */
  extensions: boolean;
}

const OPTIONS_KEY = "kestrel.view-options";
const DEFAULT_OPTIONS: Options = { hidden: false, extensions: true };
const storedOptions = load<Partial<Options>>(OPTIONS_KEY, {});
const [options, setOptions] = createSignal<Options>({
  hidden: typeof storedOptions.hidden === "boolean" ? storedOptions.hidden : DEFAULT_OPTIONS.hidden,
  extensions: typeof storedOptions.extensions === "boolean" ? storedOptions.extensions : DEFAULT_OPTIONS.extensions,
});

export const showHidden = () => options().hidden;
export const showExtensions = () => options().extensions;

export function setOption(name: keyof Options, value: boolean) {
  setOptions((current) => ({ ...current, [name]: value }));
  save(OPTIONS_KEY, options());
}

/** A row's name as displayed: without its extension when extensions are hidden. */
export function displayName(row: Pick<Row, "name" | "dir">, withExtension: boolean): string {
  if (withExtension || row.dir) return row.name;
  const dot = row.name.lastIndexOf(".");
  return dot > 0 ? row.name.slice(0, dot) : row.name;
}
