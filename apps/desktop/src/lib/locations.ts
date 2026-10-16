/**
 * Special locations: places a tab can show that are not folders, such as This PC and the
 * Recycle Bin.
 *
 * A location is an address, a title, an icon and a page component. A tab at a location keeps
 * the location's address in `Tab.path` (and in its history), with no listing; `navigate` in
 * state.ts recognises the address, or any of its aliases, and the tab panel shows the page
 * instead of the file list. Everything that goes by a tab's path follows on its own: Back and
 * Forward, tabs and their titles, the breadcrumb, the sidebar's highlight, workspaces, and the
 * palette (whose `locations` provider lists every registered location).
 *
 * To add one, call `registerLocation` once at startup from a module of your own, imported by
 * App.tsx (places.ts registers This PC and the Recycle Bin):
 *
 * ```ts
 * registerLocation({
 *   address: "shell:RecycleBinFolder",            // never a file system path
 *   aliases: ["Recycle Bin", "::{645FF040-5081-101B-9F08-00AA002F954E}"],
 *   title: "Recycle Bin",
 *   icon: "bin",                                   // a name from Icon.tsx
 *   page: RecycleBinPage,                          // gets LocationPageProps
 * });
 * ```
 *
 * A family of addresses (inside an archive, say) gives `claims` instead of a single address,
 * and its own `title` and `crumbs` per address. `adopts` makes a location the parent of file
 * system roots: This PC adopts drive roots, so Up from `C:\` goes to This PC and its crumb
 * leads every drive path. Pages report what the status bar says with `setLocationStatus`.
 */
import { createSignal, type Component } from "solid-js";

import type { IconName } from "../components/Icon";
import { baseName, breadcrumbs, parentPath } from "./format";

export interface LocationPageProps {
  tabId: number;
  /** The address the tab shows: the location's own, or for a family, which one. */
  address: string;
  /** Whether the tab is the one on screen. */
  active: boolean;
  /** Bumped by Refresh (F5) and whenever the tab is brought up to date, as when it is activated. */
  revision: number;
  /** Hands the app a way to give the page the keyboard. */
  focusRef: (focus: () => void) => void;
}

export interface SpecialLocation {
  /** The address tabs and history keep; never a file system path. */
  address: string;
  /** What it is called in tabs, the breadcrumb and the palette. */
  title: string | ((address: string) => string);
  icon: IconName;
  page: Component<LocationPageProps>;
  /** Other spellings the address bar takes, compared without case (Explorer's own, say). */
  aliases?: string[];
  /** For a family of addresses: whether `address` is one of them. */
  claims?: (address: string) => boolean;
  /** Where Up goes from it; nowhere if left out. */
  parent?: (address: string) => string | null;
  /** The breadcrumb for an address; one crumb with the title if left out. */
  crumbs?: (address: string) => Crumb[];
  /** File system roots it is the parent of, such as drive roots for This PC. */
  adopts?: (path: string) => boolean;
  /** The name of a root it adopts, for the breadcrumb and tab titles ("OS (C:)"); null if unknown. */
  rootTitle?: (root: string) => string | null;
  /** Listed by the palette; true unless it says otherwise. */
  inPalette?: boolean;
  /** More words the palette matches it by. */
  keywords?: string;
}

export interface Crumb {
  label: string;
  /** The address it goes to. */
  path: string;
  icon?: IconName;
}

const [locations, setLocations] = createSignal<SpecialLocation[]>([]);

/** Every registered location, in registration order. */
export const allLocations = locations;

/** Adds a location. Returns a function that removes it. */
export function registerLocation(location: SpecialLocation): () => void {
  setLocations((all) => [...all.filter((other) => other.address !== location.address), location]);
  return () => setLocations((all) => all.filter((other) => other !== location));
}

const same = (a: string, b: string) => a.trim().toLowerCase() === b.trim().toLowerCase();

/** The location an address belongs to, if it is one (its address, an alias, or one it claims). */
export function locationOf(address: string): SpecialLocation | null {
  if (!address) return null;
  for (const location of locations()) {
    if (same(location.address, address) || location.aliases?.some((alias) => same(alias, address))) return location;
  }
  return locations().find((location) => location.claims?.(address)) ?? null;
}

/**
 * The address to keep for something typed or clicked, when it means a location: the
 * location's own address for any of its spellings, or the address itself for one of a family.
 */
export function resolveLocation(typed: string): string | null {
  const location = locationOf(typed);
  if (!location) return null;
  return location.claims?.(typed) ? typed : location.address;
}

/** What a tab at this address is called. */
export function locationTitle(address: string): string | null {
  const location = locationOf(address);
  if (!location) return null;
  return typeof location.title === "string" ? location.title : location.title(address);
}

/** The location that adopts a file system root, if any (This PC adopts drive roots). */
const adopterOf = (root: string) => locations().find((candidate) => candidate.adopts?.(root));

/** What an adopted root is called, as its adopter names it ("OS (C:)" for `C:\`), or null. */
const rootTitle = (root: string): string | null => adopterOf(root)?.rootTitle?.(root) ?? null;

/**
 * What a tab at a folder or a location is called: the location's title, a drive's name as This
 * PC gives it, or the folder's name.
 */
export const titleOf = (path: string): string => locationTitle(path) ?? rootTitle(path) ?? (baseName(path) || path);

/** Where Up goes from a path or a location's address, or `null` when nowhere. */
export function parentOf(path: string): string | null {
  const location = locationOf(path);
  if (location) return location.parent?.(path) ?? null;
  const parent = parentPath(path);
  if (parent !== null) return parent;
  return locations().find((candidate) => candidate.adopts?.(path))?.address ?? null;
}

/** The breadcrumb for a path or a location's address. */
export function crumbsOf(path: string): Crumb[] {
  const location = locationOf(path);
  if (location) {
    return location.crumbs?.(path) ?? [{ label: locationTitle(path) ?? path, path: location.address, icon: location.icon }];
  }
  const crumbs: Crumb[] = breadcrumbs(path);
  const root = crumbs[0]?.path;
  const adopter = root ? adopterOf(root) : undefined;
  if (adopter && root) {
    crumbs[0] = { ...crumbs[0], label: rootTitle(root) ?? crumbs[0].label };
    crumbs.unshift({ label: locationTitle(adopter.address) ?? adopter.address, path: adopter.address, icon: adopter.icon });
  }
  return crumbs;
}

/**
 * Gives a page the keyboard when nothing on screen has it, as when a tab has just arrived at
 * it from the address bar. Whatever has it otherwise (the sidebar's tree, a field) keeps it.
 */
export function takeFocusIfFree(element: HTMLElement) {
  const holder = document.activeElement as HTMLElement | null;
  if (!holder || holder === document.body || holder.offsetParent === null) element.focus();
}

// ---------- what the status bar says ----------

const [statuses, setStatuses] = createSignal(new Map<number, string[]>(), { equals: false });

/**
 * Sets what the status bar says for a tab at a location, as separate parts ("12 items",
 * "2 selected"); `null` clears it.
 */
export function setLocationStatus(tabId: number, parts: string[] | null) {
  const map = statuses();
  if (JSON.stringify(map.get(tabId) ?? null) === JSON.stringify(parts)) return;
  if (parts === null) map.delete(tabId);
  else map.set(tabId, parts);
  setStatuses(map);
}

export const locationStatus = (tabId: number): string[] | null => statuses().get(tabId) ?? null;
