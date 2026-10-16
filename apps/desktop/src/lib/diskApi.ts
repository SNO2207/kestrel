import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** What type of data a file is (`kestrel-analyze`'s categories, in its order). */
export type Category =
  | "video"
  | "images"
  | "audio"
  | "documents"
  | "archives"
  | "code"
  | "programs"
  | "disk_images"
  | "build_output"
  | "system"
  | "other";

/** The categories in the backend's order: a treemap tile names its category by position here. */
export const CATEGORIES: { id: Category; label: string }[] = [
  { id: "video", label: "Video" },
  { id: "images", label: "Images" },
  { id: "audio", label: "Audio" },
  { id: "documents", label: "Documents" },
  { id: "archives", label: "Archives" },
  { id: "code", label: "Code" },
  { id: "programs", label: "Programs and libraries" },
  { id: "disk_images", label: "Disk images and virtual machines" },
  { id: "build_output", label: "Developer build output" },
  { id: "system", label: "System" },
  { id: "other", label: "Other" },
];

export const categoryLabel = (category: Category | null) =>
  CATEGORIES.find((each) => each.id === category)?.label ?? "Folder";

/** Bytes on disk, bytes of content and a count of files. */
export interface Amount {
  bytes: number;
  logical: number;
  files: number;
}

export interface Totals extends Amount {
  folders: number;
  /** Files kept online only: they take no space here. */
  cloud_files: number;
  cloud_bytes: number;
}

export interface CategoryRow extends Amount {
  category: Category;
  label: string;
  cloud_files: number;
  cloud_bytes: number;
}

export interface AgeRow extends Amount {
  label: string;
  max_days: number | null;
}

export interface DiskItem {
  entry: number;
  path: string;
  name: string;
  bytes: number;
  logical: number;
  /** Milliseconds since the Unix epoch; 0 when unknown. */
  mtime: number;
  category: Category | null;
}

export interface FolderActivity {
  path: string;
  bytes: number;
  files: number;
}

/** Files changed in the last `days` days. */
export interface Recent {
  days: number;
  bytes: number;
  files: number;
  folders: FolderActivity[];
}

/** One of Windows' own files in a drive's root, and whether the index counts it. */
export interface SystemFile {
  name: string;
  bytes: number;
  counted: boolean;
}

/** What the drive says about itself, beside what the index holds for it. */
export interface VolumeInfo {
  root: string;
  label: string;
  file_system: string;
  total: number;
  free: number;
  cluster: number;
  /** Bytes of NTFS's own file table, when the drive says. */
  mft: number | null;
  used: number;
  indexed: number;
  /** Files the index holds for the whole drive; 0 unless the overview is of the drive. */
  indexed_files: number;
  system_files: SystemFile[];
}

export interface DiskOverview {
  path: string;
  name: string;
  is_drive: boolean;
  is_dir: boolean;
  revision: number;
  took_ms: number;
  indexing: boolean;
  totals: Totals;
  categories: CategoryRow[];
  ages: AgeRow[];
  undated: Amount;
  largest_files: DiskItem[];
  largest_folders: DiskItem[];
  recent: Recent[];
  volume: VolumeInfo | null;
}

/** A treemap's tiles, one array per field (index `i` across them all is tile `i`). */
export interface TreemapData {
  path: string;
  total: number;
  truncated: boolean;
  revision: number;
  took_ms: number;
  entry: number[];
  /** The tile each lies in, or -1. */
  parent: number[];
  x: number[];
  y: number[];
  w: number[];
  h: number[];
  depth: number[];
  /** 0 a file, 1 a folder, 2 smaller items. */
  kind: number[];
  /** Position in `CATEGORIES`, or -1. */
  category: number[];
  /** 1: open folder; 2: with a strip for its name; 4: contents in the cloud. */
  flags: number[];
  bytes: number[];
  logical: number[];
  count: number[];
  name: string[];
}

export type Measure = "allocated" | "logical";

export interface Change {
  path: string;
  /** Size on disk at the sample; null when it was under the sample's threshold. */
  before: number | null;
  now: number;
  /** Exact when `before` is known, otherwise the least it grew. */
  grew: number;
  file: boolean;
  mtime: number;
}

export interface Growth {
  since_day: number;
  before: number | null;
  now: number;
  folders: Change[];
  files: Change[];
  new_files: Change[];
}

export interface GrowthWindow {
  days: number;
  label: string;
  growth: Growth | null;
}

export interface GrowthView {
  path: string;
  /** Days since the Unix epoch (UTC). */
  today: number;
  first_day: number | null;
  samples: number;
  folder_min: number;
  file_min: number;
  windows: GrowthWindow[];
}

export const diskApi = {
  overview: (path: string) => invoke<DiskOverview>("disk_overview", { path }),
  treemap: (path: string, width: number, height: number, measure: Measure) =>
    invoke<TreemapData>("disk_treemap", { path, width, height, measure }),
  growth: (path: string) => invoke<GrowthView>("disk_growth", { path }),
  /** This window's hub opened or closed; while open, `onChanged` fires as the index changes. */
  watch: (open: boolean) => invoke<void>("disk_watch", { open }),
  /** Sends items to the Recycle Bin as one undoable operation; false if nothing was. */
  recycle: (paths: string[]) => invoke<boolean>("disk_recycle", { paths }),
  /** The index changed while this window's hub is open (each window's hub is told on its own). */
  onChanged: (handler: (revision: number) => void) =>
    listen<number>("disk-changed", (event) => handler(event.payload), { target: getCurrentWindow().label }),
};
