/**
 * The command palette's and the quick launcher's backend commands (palette.rs, launcher.rs).
 * Kept apart from api.ts so the palette's additions stay in files of its own.
 */
import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Something the palette can show and the user can choose. Mirrors `palette::Item`. */
export interface PaletteItem {
  /** The provider that handles it when chosen (see `PaletteProvider` in palette.ts). */
  provider: string;
  /** The provider's id for it: a path for files and folders, a command name for actions. */
  id: string;
  /** What is shown, and what the query is matched against. */
  title: string;
  /** A second line, such as the folder a file is in. */
  subtitle?: string | null;
  /** Shown at the right: a keyboard shortcut. */
  hint?: string | null;
  /** An icon name from Icon.tsx, for items that are not files or folders. */
  icon?: string | null;
  /** The file or folder the item stands for; shown with its system icon. */
  path?: string | null;
  dir?: boolean;
  /** More words the query may match, never shown. */
  keywords?: string | null;
  /** A percentage added to the match score (negative to take away). */
  boost?: number;
}

/** An item that matched. */
export interface PaletteHit extends PaletteItem {
  /** What it ranks by. */
  score: number;
  /** UTF-16 offsets of the title's matched characters. */
  highlights: number[];
  /** It was chosen or visited before. */
  recent: boolean;
}

export interface PaletteResults {
  hits: PaletteHit[];
  scope: string | null;
  /** The backend providers that took part. */
  providers: string[];
  took_us: number;
}

/** What is remembered about a chosen item. Mirrors `kestrel_db::palette::Use`. */
export interface PaletteUse {
  provider: string;
  id: string;
  title: string;
  subtitle?: string | null;
  path?: string | null;
  dir?: boolean;
}

export interface HotkeyStatus {
  /** The shortcut chosen, or null if turned off. */
  shortcut: string | null;
  /** The shortcut registered with Windows now. */
  registered: string | null;
  state: "registered" | "failed" | "disabled" | "off";
  message: string | null;
}

export interface LauncherSettings {
  hotkey: HotkeyStatus;
  default_hotkey: string;
  keep_in_tray: boolean;
  start_with_windows: boolean;
}

/** How a chosen item opens. */
export type OpenMode = "here" | "new-tab" | "reveal";

/** What the quick launcher hands the main window to carry out. */
export interface PaletteRun {
  hit: PaletteHit;
  mode: OpenMode;
}

export const PALETTE_RUN = "palette-run";
export const LAUNCHER_SHOWN = "launcher-shown";
export const LAUNCHER_SETTINGS = "launcher-settings";

/** Which of its page's queries a request is. Mirrors `palette::Sent`. */
export interface PaletteSent {
  /** Different at every load of the page. */
  page: string;
  /** Counts up with each query the page sends. */
  number: number;
}

export const paletteApi = {
  query: (request: { text: string; session: string; items: PaletteItem[]; show_hidden: boolean; limit?: number }, sent?: PaletteSent) =>
    invoke<PaletteResults>("palette_query", { request, sent: sent ?? null }),
  record: (item: PaletteUse) => invoke<void>("palette_record", { item }),
  forget: (provider: string, id: string) => invoke<void>("palette_forget", { provider, id }),

  settings: () => invoke<LauncherSettings>("launcher_settings"),
  /** Changes the quick launcher's shortcut, or turns it off with null. */
  setHotkey: (shortcut: string | null) => invoke<HotkeyStatus>("set_launcher_hotkey", { shortcut }),
  setKeepInTray: (keep: boolean) => invoke<void>("set_keep_in_tray", { keep }),
  /** Resolves to whether the sign-in entry exists afterwards. */
  setStartWithWindows: (start: boolean) => invoke<boolean>("set_start_with_windows", { start }),
  /** Brings the app window used last forward; resolves to its label. */
  showMainWindow: () => invoke<string | null>("show_main_window"),
  hideQuickWindow: () => invoke<void>("hide_quick_window"),
  quit: () => invoke<void>("quit_app"),

  /**
   * From the quick launcher: has an app window carry out a choice; `window` is its label, the
   * one `showMainWindow` brought forward (`main` unless Ctrl+N opened others).
   */
  runInMain: (run: PaletteRun, window = "main") => emitTo(window, PALETTE_RUN, run),
  /** Choices handed to this window, and to no other. */
  onRun: (handler: (run: PaletteRun) => void): Promise<UnlistenFn> =>
    listen<PaletteRun>(PALETTE_RUN, (event) => handler(event.payload), { target: getCurrentWindow().label }),
  onShown: (handler: () => void): Promise<UnlistenFn> => listen(LAUNCHER_SHOWN, () => handler()),
  /** The tray's settings item, sent to the window it brought forward. */
  onOpenSettings: (handler: () => void): Promise<UnlistenFn> =>
    listen(LAUNCHER_SETTINGS, () => handler(), { target: getCurrentWindow().label }),
};
