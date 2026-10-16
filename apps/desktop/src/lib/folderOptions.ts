/**
 * The Folder Options that matter, as Explorer's Options dialog has them: where new windows
 * open, single- or double-click to open, the full path in the title bar (the tabs, here),
 * hiding protected operating system files, and showing sync status for cloud files.
 *
 * They are kept in local storage, which every window of the app shares, and apply at once.
 */
import { createSignal } from "solid-js";

/** Where the app opens: Home, This PC, or your user folder (as Kestrel always has). */
export type StartLocation = "home" | "this-pc" | "profile";

export interface FolderOptions {
  start: StartLocation;
  /** One click opens an item (and pointing at one underlines its name). */
  singleClick: boolean;
  /** Tabs show their folder's full path rather than its name. */
  fullPath: boolean;
  /** Hidden and system files stay out even while hidden items are shown. */
  hideProtected: boolean;
  /** Items in OneDrive and other synced folders show whether they are on this device. */
  syncStatus: boolean;
}

const KEY = "kestrel.folder-options";
export const DEFAULT_OPTIONS: FolderOptions = {
  start: "profile",
  singleClick: false,
  fullPath: false,
  hideProtected: true,
  syncStatus: true,
};

function load(): FolderOptions {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(KEY) ?? "null");
    if (stored === null || typeof stored !== "object") return { ...DEFAULT_OPTIONS };
    const value = stored as Partial<FolderOptions>;
    const flag = (name: keyof Omit<FolderOptions, "start">) =>
      typeof value[name] === "boolean" ? (value[name] as boolean) : DEFAULT_OPTIONS[name];
    return {
      start: value.start === "home" || value.start === "this-pc" || value.start === "profile" ? value.start : DEFAULT_OPTIONS.start,
      singleClick: flag("singleClick"),
      fullPath: flag("fullPath"),
      hideProtected: flag("hideProtected"),
      syncStatus: flag("syncStatus"),
    };
  } catch {
    return { ...DEFAULT_OPTIONS };
  }
}

const [options, setOptions] = createSignal<FolderOptions>(load());

export const folderOptions = options;

export function setFolderOptions(change: Partial<FolderOptions>) {
  setOptions((current) => ({ ...current, ...change }));
  try {
    localStorage.setItem(KEY, JSON.stringify(options()));
  } catch {
    // Storage is unavailable; the options still last for this session.
  }
}

// Another window changed them.
window.addEventListener("storage", (event) => {
  if (event.key === KEY) setOptions(load());
});

/** Whether listings should include protected operating system files. */
export const showProtected = () => !options().hideProtected;
