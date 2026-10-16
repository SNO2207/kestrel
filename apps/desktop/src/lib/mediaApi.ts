/**
 * The media library's commands (`src-tauri/src/media/`): where the background pass stands, its
 * scopes, pausing it, the Gallery's data, and what the details view and pane show of a photo or
 * video. Events: `media-progress` while the pass runs, `media-changed` when the Gallery has
 * something new to show.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface Skipped {
  reason: string;
  count: number;
  example: string;
}

export interface StageCount {
  name: string;
  done: number;
  failed: number;
}

export interface MediaProgress {
  /** `starting` (waiting for Kestrel to settle), `idle`, `running`, `paused` or `waiting`. */
  state: "starting" | "idle" | "running" | "paused" | "waiting";
  reason: string | null;
  total: number;
  photos: number;
  videos: number;
  done: number;
  queued: number;
  skipped: Skipped[];
  working: string[];
  stages: StageCount[];
}

export interface MediaScope {
  path: string;
  exists: boolean;
}

export interface MediaStatus {
  progress: MediaProgress;
  scopes: MediaScope[];
  /** The owner chose the scopes; otherwise they are the defaults. */
  custom: boolean;
  /** Development builds: a test's scratch folder stands in for the scopes until the next start. */
  sandboxed: boolean;
  defaults: string[];
  /** The languages text is read in, such as `en-US`. */
  languages: string[];
  stages: string[];
}

export type MediaKind = "photo" | "video";

export interface GalleryFilter {
  kind: MediaKind | null;
  folder: string | null;
}

export interface GalleryData {
  /** A result set of the items in the Gallery's order: rows come from `api.rows`. */
  set: number;
  total: number;
  /** Each item's date, taken or else last changed, in milliseconds since the Unix epoch. */
  dates: number[];
  /** Each item's width over height as shown; 0 while unknown. */
  ratios: number[];
  /** Positions of the videos and their lengths in seconds. */
  videos: [number, number][];
  folders: { path: string; count: number }[];
  scopes: string[];
  /** The thumbnail size the media pass files; ask for the same. */
  thumb: number;
  /** Where each of the paths asked to be kept is now, or -1. */
  kept: number[];
}

/** What the details view's media columns show. */
export interface ItemFacts {
  /** Milliseconds since the Unix epoch. */
  taken: number | null;
  width: number | null;
  height: number | null;
  duration_ms: number | null;
}

export const mediaApi = {
  status: () => invoke<MediaStatus>("media_status"),
  setPaused: (paused: boolean) => invoke<MediaProgress>("media_set_paused", { paused }),
  /** Sets the folders the library looks in; `null` goes back to the defaults. */
  setScopes: (scopes: string[] | null) => invoke<MediaStatus>("media_set_scopes", { scopes }),
  /** The Gallery's items; `keep` names paths whose new positions come back in `kept`. */
  gallery: (filter: GalleryFilter, keep: string[] = []) => invoke<GalleryData>("gallery_open", { filter, keep }),
  text: (path: string) => invoke<string | null>("media_text", { path }),
  facts: (paths: string[]) => invoke<(ItemFacts | null)[]>("media_facts", { paths }),
  /** Reads these items next, if they are still waiting (the Gallery's items on screen). */
  hurry: (paths: string[]) => invoke<number>("media_hurry", { paths }),
  /** Windows' folder picker, for a folder to add; null if cancelled. */
  pickFolder: (start: string) => invoke<string | null>("media_pick_folder", { start }),
  onProgress: (handler: (progress: MediaProgress) => void) => listen<MediaProgress>("media-progress", (event) => handler(event.payload)),
  onChanged: (handler: () => void) => listen("media-changed", () => handler()),
};

const PHOTO = new Set(["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "avif", "jxr", "wdp", "jxl", "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf"]);
const VIDEO = new Set(["mp4", "m4v", "mov", "avi", "wmv", "asf", "mkv", "webm", "mpg", "mpeg", "3gp", "3g2", "m2ts"]);

/** What the media library makes of a file with this extension (as `kestrel_media` decides). */
export function mediaKindOf(ext: string): MediaKind | null {
  const lower = ext.toLowerCase();
  return PHOTO.has(lower) ? "photo" : VIDEO.has(lower) ? "video" : null;
}

/** A length such as `0:42`, `3:05` or `1:02:09`. */
export function formatLength(seconds: number): string {
  const whole = Math.max(0, Math.round(seconds));
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = String(whole % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}
