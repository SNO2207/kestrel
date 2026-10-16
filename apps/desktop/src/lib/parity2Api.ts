/**
 * The backend commands of Explorer parity's second round (`src-tauri/src/parity2`, and adding to a
 * zip in `archive/add.rs`), kept beside `api.ts` so that file stays the core's.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** One of the shell's libraries (`parity2/libraries.rs`). */
export interface Library {
  name: string;
  /** `shell:Libraries\<name>`, which lists its folders as one. */
  address: string;
  /** Its `.library-ms` file. */
  file: string;
  folders: string[];
  save_folder: string | null;
  pinned: boolean;
  kind: "documents" | "music" | "pictures" | "videos" | "library";
  /** Locations it names that are not there now. */
  missing: string[];
}

/** A computer of the Network location. */
export interface Computer {
  name: string;
  /** `\\NAME`. */
  path: string;
  group: string;
  comment: string;
}

export interface Neighbourhood {
  computers: Computer[];
  /** The network has been walked to the end; otherwise more may come (`network-changed`). */
  complete: boolean;
  error: string | null;
}

/** A network location made by "Add a network location". */
export interface NetworkPlace {
  name: string;
  target: string;
}

/** A dialog the development hook recorded instead of showing. */
export interface RecordedDialog {
  function: string;
  subject: string | null;
}

/** What "Clear File Explorer history" removed. */
export interface Cleared {
  visits: number;
  opened: number;
  palette: number;
}

/** A tab as it moves between windows. */
export interface TabState {
  path: string;
  history: string[];
  cursor: number;
}

/** Another window asking whether this one takes a tab let go over it. */
export interface TabArrival {
  id: number;
  /** Where it was let go, in this page's CSS pixels. */
  x: number;
  y: number;
  tab: TabState;
}

/** What adding files to a zip did. */
export interface ZipAdded {
  archive: string;
  names: string[];
  replaced: number;
}

export const parity2Api = {
  libraries: () => invoke<Library[]>("libraries_list"),
  /** Development builds: libraries are read from `folder` instead of the account's (null: back). */
  librariesDevFolder: (folder: string | null) => invoke<void>("libraries_dev_folder", { folder }),
  /** Map network drive (Windows' dialog); resolves to the drive mapped, or null if cancelled. */
  mapDrive: (remote: string | null = null) => invoke<string | null>("network_map_drive", { remote }),
  /** Disconnect one network drive, or with null Windows' list of them to choose from. */
  disconnectDrive: (path: string | null = null) => invoke<void>("network_disconnect_drive", { path }),
  addNetworkLocation: () => invoke<void>("network_add_location"),
  computers: (fresh = false) => invoke<Neighbourhood>("network_computers", { fresh }),
  shares: (server: string, showHidden = false) => invoke<string[]>("network_shares", { server, showHidden }),
  networkPlaces: () => invoke<NetworkPlace[]>("network_places"),
  onNetworkChanged: (handler: (found: Neighbourhood) => void): Promise<UnlistenFn> =>
    listen<Neighbourhood>("network-changed", (event) => handler(event.payload)),
  /** Development builds: record the network dialogs instead of showing them (`on`), or stop. */
  networkDevRecord: (on: boolean) => invoke<RecordedDialog[]>("network_dev_record", { on }),
  networkDevTake: () => invoke<RecordedDialog[]>("network_dev_take"),
  /** Development builds: the Network location finds these computers (null: the real network). */
  networkDevComputers: (computers: string[] | null, delayMs = 0) => invoke<void>("network_dev_computers", { computers, delayMs }),
  /** A tab let go outside its strip; `at` stands in for the pointer in development builds. */
  tabDrop: (tab: TabState, alone: boolean, at: { label: string | null; x: number; y: number } | null = null) =>
    invoke<"moved" | "window" | "none">("tab_drop", { tab, alone, at }),
  tabArrival: (id: number, taken: boolean) => invoke<void>("tab_arrival", { id, taken }),
  onTabArrival: (handler: (arrival: TabArrival) => void): Promise<UnlistenFn> =>
    listen<TabArrival>("kestrel:tab-arrive", (event) => handler(event.payload), { target: getCurrentWindow().label }),
  clearHistory: () => invoke<Cleared>("privacy_clear_history"),
  onHistoryCleared: (handler: () => void): Promise<UnlistenFn> => listen("history-cleared", () => handler()),
  /** Adds files to a zip: `target` is the zip, or a folder of one open in a tab. */
  addToZip: (target: string, paths: string[]) => invoke<ZipAdded>("archive_add", { target, paths }),
};
