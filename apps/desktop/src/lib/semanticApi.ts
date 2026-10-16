/**
 * The backend's AI search commands (`src-tauri/src/semantic/`): the models and their
 * downloads, the background describing, the relevance slider, and where in a video a search
 * matched.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export interface ModelFile {
  path: string;
  size: number;
  sha256: string;
  url: string;
}

export interface Device {
  gpu: boolean;
  name: string;
  note: string | null;
}

export interface SelfTestCheck {
  subject: string;
  expected: string;
  best: string;
  passed: boolean;
  expectedScore: number;
  runnerUp: number;
}

export interface SelfTestReport {
  passed: boolean;
  checks: SelfTestCheck[];
  imagesPerSecond: number;
  device: Device;
  tookMs: number;
  error: string | null;
}

export interface DownloadProgress {
  /** `downloading`, `testing`, `done`, `failed` or `cancelled`. */
  state: "downloading" | "testing" | "done" | "failed" | "cancelled";
  done: number;
  total: number;
  file: string;
  bytesPerSecond: number;
  message: string | null;
}

export interface ModelView {
  id: string;
  title: string;
  summary: string;
  repo: string;
  revision: string;
  page: string;
  licence: string;
  author: string;
  convertedBy: string;
  files: ModelFile[];
  totalSize: number;
  imageSize: number;
  dim: number;
  installed: boolean;
  installedAt: number | null;
  active: boolean;
  selftest: SelfTestReport | null;
  download: DownloadProgress | null;
}

/** The media library's pass, as far as AI search's embedding stage goes. */
export interface Indexing {
  state: "off" | "loading" | "working" | "waiting" | "done" | "paused" | "failed";
  files: number;
  media: number;
  queued: number;
  done: number;
  unreadable: number;
  current: string | null;
  error: string | null;
}

export interface SemanticState {
  models: ModelView[];
  active: string | null;
  runtime: { state: "off" | "loading" | "ready" | "failed"; model: string | null; device: Device | null; error: string | null };
  indexing: Indexing;
  paused: boolean;
  cutoff: number;
  scopes: string[];
  defaultScopes: boolean;
  /** This copy downloads only from this computer (a test instance). */
  localOnly: boolean;
}

export const semanticApi = {
  state: () => invoke<SemanticState>("semantic_state"),
  /** Downloads a model: only the AI search page's Download button calls this. */
  download: (model: string) => invoke<void>("semantic_download", { model }),
  cancelDownload: (model: string) => invoke<void>("semantic_cancel_download", { model }),
  remove: (model: string) => invoke<void>("semantic_delete", { model }),
  selfTest: (model: string) => invoke<SelfTestReport>("semantic_selftest", { model }),
  setActive: (model: string) => invoke<void>("semantic_set_active", { model }),
  setPaused: (paused: boolean) => invoke<void>("semantic_set_paused", { paused }),
  setCutoff: (value: number) => invoke<void>("semantic_set_cutoff", { value }),
  /** The folders to describe, which are the media library's; `null` goes back to its defaults. */
  setScopes: (scopes: string[] | null) => invoke<void>("semantic_set_scopes", { scopes }),
  /** Where in each video of a result set the description matched, in ms, by lower-case path. */
  moments: (set: number) => invoke<Record<string, number>>("semantic_moments", { set }),
  /** Fires whenever the state changed and should be asked for again. */
  onChange: (handler: () => void) => listen("semantic-state", () => handler()),
};
