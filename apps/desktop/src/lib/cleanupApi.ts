import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Backend commands of cleanup recommendations and the duplicate finder (`src-tauri/src/cleanup`). */

export type Tier = "safe" | "review" | "info";

export type Rule =
  | "temp"
  | "crash"
  | "build"
  | "package-cache"
  | "app-cache"
  | "downloads"
  | "large-old"
  | "disk-images"
  | "recycle-bin"
  | "windows";

export interface CleanupItem {
  path: string;
  dir: boolean;
  /** Bytes on disk; for a folder, everything in it. */
  bytes: number;
  /** The newest change in it, in milliseconds since the Unix epoch; 0 when unknown. */
  modified: number;
}

export interface CleanupTool {
  program: string;
  args: string[];
  /** The folder it runs in; the owner's profile folder when null. */
  dir: string | null;
  /** The project it acts on, for showing, when that is not the folder it runs in. */
  project: string | null;
  /** The command exactly as it runs, such as `cargo clean`. */
  command: string;
}

export type CleanupAction = { kind: "recycle" } | ({ kind: "tool" } & CleanupTool) | { kind: "none" };

export interface Recommendation {
  id: string;
  rule: Rule;
  title: string;
  reason: string;
  tier: Tier;
  action: CleanupAction;
  items: CleanupItem[];
  /** Bytes on disk it would free (at most, for a tool or Info). */
  bytes: number;
  advice: string | null;
  note: string | null;
  parts: { label: string; bytes: number }[];
  /** `shell:RecycleBinFolder`, `ms-settings:storagesense` or `cleanmgr`. */
  links: { label: string; target: string }[];
}

export interface ScanReport {
  recommendations: Recommendation[];
  /** The index is still being built, so there may be more. */
  indexing: boolean;
  /** Bytes the Safe and Review recommendations would free. */
  total: number;
  took_ms: number;
}

export interface Skipped {
  path: string;
  reason: string;
}

export interface RecycleReport {
  dry_run: boolean;
  /** What went (or, in a dry run, would go) to the Recycle Bin. */
  recycled: string[];
  /** What the shell erased instead, after asking. */
  erased: string[];
  skipped: Skipped[];
  bytes: number;
  completed: boolean;
  message: string;
}

export interface ToolReport {
  command: string;
  dir: string | null;
  dry_run: boolean;
  /** The test hook recorded it instead of running it. */
  recorded: boolean;
  status: number | null;
  timed_out: boolean;
  output: string;
}

export interface DupeFile {
  path: string;
  /** Milliseconds since the Unix epoch. */
  modified: number;
  /** Other names of the same file (hard links), which go with it. */
  links: string[];
}

export interface DupeGroup {
  id: string;
  size: number;
  /** Bytes on disk of one copy. */
  bytes_each: number;
  /** What keeping only one copy would free. */
  wasted: number;
  files: DupeFile[];
}

export interface DupeReport {
  groups: DupeGroup[];
  candidates: number;
  hashed_files: number;
  hashed_bytes: number;
  reused: number;
  placeholders: number;
  skipped: number;
  wasted: number;
  took_ms: number;
}

/** A photo among similar ones: it looks nearly the same as the others, without being a copy. */
export interface SimilarFile {
  path: string;
  /** Milliseconds since the Unix epoch. */
  modified: number;
  size: number;
  /** Bytes on disk. */
  bytes: number;
}

/** Photos that look alike (AI search's vectors), never chosen for removal by Kestrel. */
export interface SimilarGroup {
  id: string;
  /** How alike the least alike pair linked in the group is (cosine). */
  score: number;
  files: SimilarFile[];
}

export interface SimilarReport {
  groups: SimilarGroup[];
  /** The model compared with; `null` when none is in use. */
  model: string | null;
  compared: number;
  threshold: number;
}

export type DupePhase = "checking" | "partial" | "full";

export interface DupeProgress {
  phase: DupePhase;
  done: number;
  total: number;
}

export interface DupeDefaults {
  roots: string[];
  exclude: string[];
  min_size: number;
}

export const cleanupApi = {
  /** Runs every rule; with `root`, keeps only what lies beneath it. */
  scan: (root: string | null) => invoke<ScanReport>("cleanup_scan", { root }),
  /** Sends ticked items to the Recycle Bin, or with `dryRun` says what would happen. */
  recycle: (chosen: { id: string; paths: string[] }[], dryRun: boolean) =>
    invoke<RecycleReport>("cleanup_recycle", { chosen, dryRun }),
  runTool: (id: string, dryRun: boolean) => invoke<ToolReport>("cleanup_run_tool", { id, dryRun }),
  /** Opens Storage Sense (`ms-settings:storagesense`) or Disk Cleanup (`cleanmgr`). */
  open: (target: string) => invoke<void>("cleanup_open", { target }),
  dupesDefaults: () => invoke<DupeDefaults>("dupes_defaults"),
  /** Resolves to `null` when the search was cancelled. */
  dupesScan: (roots: string[], exclude: string[], minSize: number) =>
    invoke<DupeReport | null>("dupes_scan", { request: { roots, exclude, min_size: minSize } }),
  /** Similar photos where the duplicates were looked for; `null` when cancelled. */
  dupesSimilar: (roots: string[], exclude: string[], minSize: number) =>
    invoke<SimilarReport | null>("dupes_similar", { request: { roots, exclude, min_size: minSize } }),
  dupesCancel: () => invoke<void>("dupes_cancel"),
  dupesRecycle: (removals: { keep: string; remove: string[] }[], dryRun: boolean) =>
    invoke<RecycleReport>("dupes_recycle", { removals, dryRun }),
  onDupesProgress: (handler: (progress: DupeProgress) => void) =>
    listen<DupeProgress>("cleanup-dupes-progress", (event) => handler(event.payload)),
};
