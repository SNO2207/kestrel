/**
 * The backend's search commands (`src-tauri/src/search/`, `triage.rs`, `wsl.rs`): the search
 * box's searches (phrases, searches inside files, dates taken), the Downloads triage page and
 * WSL's distributions.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type { Listing, SortSpec } from "./api";
import { showProtected } from "./folderOptions";
import { showHidden } from "./views";

/** How the search box's options say to search (`search::Options`). */
export interface SearchOptions {
  /** "Search inside files": what is typed is looked for in the files' contents. */
  inside: boolean;
  regex: boolean;
  match_case: boolean;
  whole_word: boolean;
  /** `.gitignore` files are honoured inside repositories. */
  gitignore: boolean;
  /** Files larger than this many megabytes are passed over. */
  max_size_mb: number;
}

/** One chip of the search box: a clause and what it means. */
export interface Chip {
  clause: string;
  label: string;
  kind: "kind" | "modified" | "taken" | "size" | "place" | "content" | "name" | "filter";
  /** The words it was read from. */
  phrase: string;
}

/** How far a search inside files got. */
export interface ContentStatus {
  searched: number;
  matched: number;
  binary: number;
  too_large: number;
  cloud: number;
  unreadable: number;
  ignored: number;
  /** It stopped at the limit of files found. */
  full: boolean;
  done: boolean;
  /** Where it looks: a folder's name, or "N files". */
  place: string;
  elapsed_ms: number;
}

/** What a search found, and how its text was read. */
export interface SearchAnswer {
  listing: Listing;
  natural: boolean;
  query: string;
  chips: Chip[];
  content: ContentStatus | null;
}

/** The first matching line of a file a search inside files found (`Row.hit`). */
export interface Hit {
  /** One-based. */
  line: number;
  text: string;
  /** Where the matches are in `text`, as `[start, end)` string offsets. */
  ranges: [number, number][];
  /** Matching lines in the file, up to a limit; `more` when there are more. */
  lines: number;
  more: boolean;
}

export type TriageKind = "installers" | "archives" | "documents" | "images" | "videos" | "other";

export interface TriageSuggestion {
  path: string;
  label: string;
  /** `learned`: similar files went there; `kind`: the known folder for the kind; `project`. */
  why: "learned" | "kind" | "project";
}

export interface TriageItem {
  path: string;
  name: string;
  ext: string;
  dir: boolean;
  kind: TriageKind;
  size: number;
  /** Milliseconds since 1970. */
  modified: number;
  arrived: number;
  attrs: number;
  suggestions: TriageSuggestion[];
}

export interface TriagePageData {
  folder: string;
  /** What arrived after this (milliseconds since 1970) is new. */
  since: number;
  items: TriageItem[];
  new: number;
  total: number;
}

export interface TriageMoved {
  folder: string;
  names: string[];
  completed: boolean;
}

/** A WSL distribution (`wsl::Distro`). */
export interface Distro {
  name: string;
  /** Its root: `\\wsl.localhost\Ubuntu`. */
  path: string;
  version: number;
  default: boolean;
}

export const searchApi = {
  /**
   * Runs a tab's search. With `update`, a search inside files already running for the same
   * text answers with what it found so far.
   */
  run: (request: { tab: number; text: string; scope: string | null; folder: string | null; sort: SortSpec; options: SearchOptions; update: boolean }) =>
    invoke<SearchAnswer>("search_run", { ...request, showHidden: showHidden(), showProtected: showProtected() }),
  /** Stops the tab's search inside files; `forget` drops what it found, as when the tab leaves it. */
  cancel: (tab: number, forget = true) => invoke<void>("search_cancel", { tab, forget }),
  /** A search inside files of this window got further. */
  onProgress: (handler: (tab: number, status: ContentStatus) => void) =>
    listen<{ tab: number; status: ContentStatus }>("search-progress", (event) => handler(event.payload.tab, event.payload.status), {
      target: getCurrentWindow().label,
    }),

  triageList: (since: number | null, all: boolean) => invoke<TriagePageData>("triage_list", { since, all }),
  triageCount: () => invoke<number>("triage_count"),
  /** The page was looked at: what is there is no longer new. Resolves to when. */
  triageMarkSeen: () => invoke<number>("triage_mark_seen"),
  triageMove: (paths: string[], destination: string) => invoke<TriageMoved>("triage_move", { paths, destination }),
  triageRecycle: (paths: string[]) => invoke<boolean>("triage_recycle", { paths }),
  /** How many downloads are new, whenever the Downloads folder changes or the page is seen. */
  onTriageChanged: (handler: (count: number) => void) => listen<number>("triage-changed", (event) => handler(event.payload)),
  /** The Downloads folder changed (only when it did: not when the page is seen). */
  onTriageFolder: (handler: () => void) => listen("triage-folder", () => handler()),

  wslDistros: () => invoke<Distro[]>("wsl_distros"),
  /** Linux permissions (`-rw-r--r--`) of items in a distribution, where WSL gives them. */
  wslModes: (paths: string[]) => invoke<(string | null)[]>("wsl_modes", { paths }),
};
