import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { writesClipboard } from "./api";

let nextJob = 1;

/**
 * A number for a job of this window's: an archive job, or a listing it may cancel. The backend
 * keeps jobs by window and number, so every window counts from 1 on its own; the numbers come
 * from one counter so a window's archive jobs and listings never share one.
 */
export const nextJobId = () => nextJob++;

/** What a cancelled listing fails with (`Failure::Cancelled`). */
export const CANCELLED = "Cancelled.";

/** Why an archive job stopped, as the backend says it (`archive/job.rs`). */
export interface ArchiveProblem {
  kind: "password" | "wrong-password" | "unsupported" | "corrupt" | "unsafe" | "space" | "bomb" | "cancelled" | "io" | "other";
  message: string;
}

/** What an extraction made, and where to show and select it. */
export interface ExtractReport {
  /** The folder holding the new items, and their names there. */
  folder: string;
  names: string[];
  /** Where the files went. */
  destination: string;
  links_skipped: number;
  renamed: number;
  /** Nothing was skipped when asked about conflicts. */
  completed: boolean;
}

export interface CompressReport {
  path: string;
  name: string;
}

export type ArchiveKind = "zip" | "7z" | "tar-gz";
export type ArchiveLevel = "store" | "fast" | "normal" | "maximum";

export interface ArchivePreferences {
  show_extracted: boolean;
  format: ArchiveKind;
  level: ArchiveLevel;
}

/** What a running job reports. */
export interface ArchiveProgress {
  job: number;
  phase: "reading" | "extracting" | "placing" | "compressing";
  done: number;
  total: number;
  item: string;
}

/** A rejected command's value as a problem, whatever shape it came in. */
export function asProblem(error: unknown): ArchiveProblem {
  if (error && typeof error === "object" && "kind" in error && "message" in error) return error as ArchiveProblem;
  return { kind: "other", message: error instanceof Error ? error.message : String(error) };
}

export const archiveApi = {
  /**
   * Extracts the archive at or around `source` into `destination` (made if needed), or with
   * `items`, only those items of one folder inside an archive. Progress arrives for `job`.
   */
  extract: (job: number, source: string, destination: string, items: string[] | null, password: string | null) =>
    invoke<ExtractReport>("archive_extract", { job, source, items, destination, password }),
  /** Compresses items into `folder`; without a name, after the first item. */
  compress: (job: number, paths: string[], folder: string, name: string | null, kind: ArchiveKind, level: ArchiveLevel) =>
    invoke<CompressReport>("archive_compress", { job, paths, folder, name, kind, level }),
  /** Cancels a job of this window's: an archive job, or a listing given this number as its ticket. */
  cancel: (job: number) => invoke<boolean>("archive_cancel", { job }),
  /** Opens a file inside an archive from a copy in the temp folder. Progress arrives for `job`. */
  open: (job: number, path: string, password: string | null) => invoke<void>("archive_open", { job, path, password }),
  /** Puts items inside an archive on the clipboard, copied out first; a paste meanwhile waits for it. */
  copy: (job: number, paths: string[], password: string | null) =>
    writesClipboard(() => invoke<number>("archive_copy", { job, paths, password })),
  /** Windows' folder picker; null if cancelled. */
  pickFolder: (start: string) => invoke<string | null>("archive_pick_folder", { start }),
  preferences: () => invoke<ArchivePreferences>("archive_preferences"),
  setPreferences: (preferences: ArchivePreferences) => invoke<void>("archive_set_preferences", { preferences }),
  /** Progress of this window's jobs; other windows' jobs, numbered alike, are theirs to show. */
  onProgress: (handler: (progress: ArchiveProgress) => void) =>
    listen<ArchiveProgress>("archive-progress", (event) => handler(event.payload), { target: getCurrentWindow().label }),
  /** Development builds: the files Copy or a drag would carry for these paths. */
  devMaterialize: (paths: string[]) => invoke<string[]>("archive_dev_materialize", { paths }),
};
