/**
 * The developer tools' state: the terminal pane docked at the bottom of the window (its tabs,
 * whether it shows, its height), the ways in (Ctrl+`, "Open terminal here", the palette, the
 * projects hub's Run), and the settings the Developer tools page edits.
 *
 * Each terminal tab is drawn by a `TerminalView` (TerminalPane.tsx), which owns its xterm.js
 * terminal and its shell in the backend (`devtools/terminal.rs`) and registers itself here, so
 * actions can reach the active one. A tab is closed by removing it, which ends its shell.
 * Hiding the pane keeps the shells running; closing the window ends them (the backend does).
 */
import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

import { devtoolsApi, type DevtoolsSettings, type ScriptRef } from "./lib/devtoolsApi";
import { inFolder } from "./parity";
import { activeTab, navigate, notify, places } from "./state";

export interface TerminalTab {
  /** The front end's own number for the tab, known before its shell has started. */
  key: number;
  /** What it starts with. */
  cwd: string | null;
  shell: string | null;
  script: ScriptRef | null;
  /** Filled in once the shell runs. */
  title: string;
  session: number | null;
  pid: number | null;
  /** Set once the shell has exited, to its exit code (`null` when unknown). */
  exited?: number | null;
}

/** What a terminal view offers the rest of the app. */
export interface TerminalHandle {
  focus: () => void;
  /** The text of the terminal's buffer, for the e2e hook. */
  text: () => string;
  /** Columns and rows, for the e2e hook. */
  size: () => { cols: number; rows: number };
  /** Fits the terminal to its box, as after the pane is shown or resized. */
  fit: () => void;
  copy: () => void;
  hasSelection: () => boolean;
  paste: () => void;
  selectAll: () => void;
  clear: () => void;
}

const [terminals, setTerminals] = createStore<TerminalTab[]>([]);
const [activeKey, setActiveKey] = createSignal<number | null>(null);
const [paneShown, setPaneShown] = createSignal(false);
const handles = new Map<number, TerminalHandle>();
let nextKey = 1;

/**
 * Shells this window's page had before it loaded again are ended first: nothing draws them
 * now. Terminals open once this is done.
 */
export const terminalsReady: Promise<void> = devtoolsApi.reset().catch(() => {});

/** The Developer tools page's address (a special location, `devtoolsSetup.ts`). */
export const DEVELOPER_TOOLS = "Developer tools";

/** Opens the Developer tools page in the active tab. */
export function openDeveloperTools() {
  const tab = activeTab();
  if (tab) navigate(tab.id, DEVELOPER_TOOLS);
}

export { terminals, activeKey, setActiveKey, paneShown };

export const registerTerminal = (key: number, handle: TerminalHandle) => {
  handles.set(key, handle);
  return () => {
    if (handles.get(key) === handle) handles.delete(key);
  };
};

export const handleOf = (key: number | null) => (key === null ? undefined : handles.get(key));
export const activeTerminal = () => terminals.find((tab) => tab.key === activeKey());

/** Updates a tab, if it is still open. */
export function patchTerminal(key: number, change: Partial<TerminalTab>) {
  const at = terminals.findIndex((tab) => tab.key === key);
  if (at >= 0) setTerminals(at, change);
}

// ---------- the pane's height ----------

const HEIGHT_KEY = "terminal-height";
export const MIN_HEIGHT = 120;

function rememberedHeight(): number {
  try {
    const value = Number(localStorage.getItem(HEIGHT_KEY));
    return Number.isFinite(value) && value >= MIN_HEIGHT ? value : 280;
  } catch {
    return 280;
  }
}

const [paneHeight, setPaneHeightSignal] = createSignal(rememberedHeight());
export { paneHeight };

/** The tallest the pane may be: the file list keeps some room. */
export const maxHeight = () => Math.max(MIN_HEIGHT, window.innerHeight - 260);

export function setPaneHeight(height: number, remember: boolean) {
  const clamped = Math.round(Math.min(maxHeight(), Math.max(MIN_HEIGHT, height)));
  setPaneHeightSignal(clamped);
  if (!remember) return;
  try {
    localStorage.setItem(HEIGHT_KEY, String(clamped));
  } catch {
    // Not remembered, which is harmless.
  }
}

// ---------- where terminals open ----------

/** The folder the active tab shows, if it shows one (not a search, a special location or an archive). */
export function currentFolder(): string | null {
  const tab = activeTab();
  return inFolder(tab) ? (tab.listing?.path ?? null) : null;
}

/** Gives the active terminal the keyboard, once it is drawn. */
export function focusTerminal() {
  requestAnimationFrame(() => handleOf(activeKey())?.focus());
}

/** Shows the pane and opens a terminal in `cwd` (the folder on screen if not given, else the user folder). */
export function openTerminal(options: { cwd?: string | null; shell?: string | null; script?: ScriptRef | null } = {}) {
  const key = nextKey++;
  const cwd = options.cwd ?? currentFolder() ?? (places().home || null);
  const tab: TerminalTab = {
    key,
    cwd,
    shell: options.shell ?? null,
    script: options.script ?? null,
    title: options.script ? "Script" : "Terminal",
    session: null,
    pid: null,
  };
  setTerminals(terminals.length, tab);
  setActiveKey(key);
  setPaneShown(true);
  focusTerminal();
  return key;
}

/** "Open terminal here": a new terminal in `folder`. */
export const terminalHere = (folder: string) => void openTerminal({ cwd: folder });

/** Closes a terminal tab, ending its shell. The pane hides with its last tab. */
export function closeTerminal(key: number, focusAfter?: () => void) {
  const at = terminals.findIndex((tab) => tab.key === key);
  if (at < 0) return;
  const next = terminals[at + 1] ?? terminals[at - 1];
  setTerminals((all) => all.filter((tab) => tab.key !== key));
  if (activeKey() === key) setActiveKey(next?.key ?? null);
  if (terminals.length === 0) {
    setPaneShown(false);
    focusAfter?.();
  } else {
    focusTerminal();
  }
}

/** Ctrl+`: shows the pane (with a terminal in it), gives it the keyboard, or hides it. */
export function toggleTerminal(focusList?: () => void) {
  if (!paneShown()) {
    if (terminals.length === 0) return void openTerminal();
    setPaneShown(true);
    requestAnimationFrame(() => handleOf(activeKey())?.fit());
    return focusTerminal();
  }
  const inside = document.activeElement?.closest(".terminal-pane");
  if (!inside && terminals.length > 0) return focusTerminal();
  hideTerminal(focusList);
}

/** Hides the pane; its terminals go on running. */
export function hideTerminal(focusList?: () => void) {
  setPaneShown(false);
  focusList?.();
}

/** "cd to this folder": takes the active terminal's shell to the folder on screen. */
export async function cdToFolder() {
  const folder = currentFolder();
  const tab = activeTerminal();
  if (!folder || !tab?.session || tab.exited !== undefined) return;
  try {
    await devtoolsApi.cd(tab.session, folder);
  } catch (error) {
    notify(error);
  }
  focusTerminal();
}

// ---------- settings ----------

const [settings, setSettings] = createSignal<DevtoolsSettings | null>(null);
export { settings };

/** Reads the settings again (the Developer tools page, and the projects hub's Run, use them). */
export async function loadSettings(): Promise<DevtoolsSettings | null> {
  try {
    const state = await devtoolsApi.state();
    setSettings(state.settings);
    return state.settings;
  } catch (error) {
    console.error("developer tools settings:", error);
    return null;
  }
}

export const settingsChanged = (next: DevtoolsSettings) => setSettings(next);

/**
 * The projects hub's Run: in the terminal pane when the settings say so (returns true), else
 * the caller starts Windows Terminal.
 */
export async function runScriptHere(root: string, script: { source: string; name: string }): Promise<boolean> {
  const current = settings() ?? (await loadSettings());
  if (!current?.scripts_in_terminal) return false;
  openTerminal({ cwd: root, script: { root, source: script.source, name: script.name } });
  return true;
}

// ---------- the palette's actions ----------

export function devtoolsActions() {
  return [
    {
      id: "toggle-terminal",
      title: paneShown() ? "Hide the terminal" : "Show the terminal",
      hint: "Ctrl+`",
      keywords: "terminal console shell powershell command prompt cmd wsl panel",
      mainOnly: true,
      run: () => toggleTerminal(),
    },
    {
      id: "new-terminal",
      title: "New terminal",
      hint: "Ctrl+Shift+`",
      keywords: "terminal console shell powershell command prompt cmd wsl tab",
      mainOnly: true,
      run: () => void openTerminal(),
    },
    {
      id: "terminal-here",
      title: "Open terminal here",
      folder: true,
      mainOnly: true,
      keywords: "terminal console shell powershell command prompt cmd wsl folder pane",
      run: () => void openTerminal({ cwd: currentFolder() }),
    },
    {
      id: "terminal-cd",
      title: "cd to this folder",
      folder: true,
      mainOnly: true,
      keywords: "terminal change directory folder follow",
      when: () => paneShown() && !!activeTerminal()?.session,
      run: () => void cdToFolder(),
    },
  ];
}

// ---------- keys ----------

/** Whether a key belongs to the app even while a terminal has the keyboard. */
export function passesThrough(event: KeyboardEvent): boolean {
  const key = event.key.toLowerCase();
  if (event.code === "Backquote" && event.ctrlKey) return true;
  if (key === "f11" && !event.ctrlKey && !event.altKey) return true;
  // Hubs and the palette, as VS Code's terminal lets its own Ctrl+Shift+P through.
  if (event.ctrlKey && event.shiftKey && !event.altKey && (key === "p" || key === "u" || key === "d")) return true;
  if (event.ctrlKey && !event.altKey && key === "tab") return true;
  return false;
}

/** Ctrl+` and Ctrl+Shift+`, from anywhere, as in VS Code. Captured, so they work from any field. */
export function listenForTerminalKeys(focusList: () => void): () => void {
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.code !== "Backquote" || !event.ctrlKey || event.altKey || event.metaKey) return;
    // Not behind a dialog, the palette or Quick Look, which hold the keyboard.
    if (document.querySelector(".palette-backdrop, .dialog-backdrop, .quicklook-backdrop")) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.shiftKey) openTerminal();
    else toggleTerminal(focusList);
  };
  window.addEventListener("keydown", onKeyDown, true);
  return () => window.removeEventListener("keydown", onKeyDown, true);
}

// ---------- the end-to-end tests' hook ----------

declare global {
  interface Window {
    __kestrelTerminal?: {
      tabs: () => { key: number; session: number | null; pid: number | null; title: string; active: boolean; exited?: number | null }[];
      text: (key: number) => string;
      size: (key: number) => { cols: number; rows: number } | null;
      shown: () => boolean;
    };
  }
}

if (import.meta.env.DEV) {
  window.__kestrelTerminal = {
    tabs: () => terminals.map((tab) => ({ key: tab.key, session: tab.session, pid: tab.pid, title: tab.title, active: tab.key === activeKey(), exited: tab.exited })),
    text: (key) => handles.get(key)?.text() ?? "",
    size: (key) => handles.get(key)?.size() ?? null,
    shown: () => paneShown(),
  };
}
