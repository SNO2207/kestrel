/**
 * The palette's actions: every command the app has, each with its shortcut.
 *
 * Actions run in the main window. The quick launcher lists them too, minus those that act on
 * the selected files, and hands a chosen one to the main window (palette.ts).
 */
import type { ListCommand } from "../components/FileList";
import { focusTargets } from "../components/Toolbar";
import { paneOpen, setQuickLookOpen, focusedItem, togglePane } from "../previewState";
import {
  activeTab,
  closeTab,
  cycleTab,
  everywhere,
  goBack,
  goForward,
  goUp,
  isSearch,
  openTab,
  refresh,
  setEverywhere,
  setSortSpec,
  startsDescending,
  tabs,
} from "../state";
import { redoLast, undoLast } from "../undo";
import { recheckAccess } from "./access";
import type { SortKey } from "./api";
import type { PaletteContext, PaletteHit, PaletteItem, PaletteProvider } from "./palette";
import { paletteApi } from "./paletteApi";
import { VIEW_MODES, locationKey, setOption, setViewMode, showExtensions, showHidden } from "./views";

/** What actions need from the main window. */
export interface ActionHost {
  /** Carries out a file list command on the active tab. */
  runCommand: (command: ListCommand) => void;
  /** Gives the keyboard back to the active tab's list. */
  focusList: () => void;
  /** Opens the quick launcher settings dialog. */
  openSettings: () => void;
}

interface Action {
  id: string;
  title: string;
  hint?: string;
  icon?: string;
  keywords?: string;
  /** Acts on the selected items, so it is listed only while some are selected. */
  selection?: boolean;
  /** Acts on the folder on screen, so it is listed only while a folder (not a search) is. */
  folder?: boolean;
  /** Has no meaning in the quick launcher. */
  mainOnly?: boolean;
  run: (host: ActionHost) => void;
}

const ID = "actions";
/** The host of actions run outside the main window, which only quitting does. */
const DETACHED: ActionHost = { runCommand: () => {}, focusList: () => {}, openSettings: () => {} };

const SORTS: { key: SortKey; label: string }[] = [
  { key: "name", label: "name" },
  { key: "modified", label: "date modified" },
  { key: "type", label: "type" },
  { key: "size", label: "size" },
];

/** Sends a key to the active tab's list, for commands that live inside the list. */
function pressInList(host: ActionHost, key: string, init: KeyboardEventInit = {}) {
  host.focusList();
  const list = document.querySelector<HTMLElement>(".tab-panel.active .list-viewport");
  list?.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init }));
}

const withTab = (run: (tabId: number) => void) => () => {
  const tab = activeTab();
  if (tab) run(tab.id);
};

/** Every action, in the order an empty palette lists them. */
function actions(): Action[] {
  const tab = activeTab();
  const sort = tab?.sort;
  return [
    { id: "new-tab", title: "New tab", hint: "Ctrl+T", icon: "plus", run: () => openTab() },
    { id: "close-tab", title: "Close tab", hint: "Ctrl+W", icon: "close", mainOnly: true, run: withTab(closeTab) },
    { id: "next-tab", title: "Next tab", hint: "Ctrl+Tab", keywords: "switch tab", run: () => cycleTab(1) },
    { id: "previous-tab", title: "Previous tab", hint: "Ctrl+Shift+Tab", keywords: "switch tab", run: () => cycleTab(-1) },
    { id: "back", title: "Back", hint: "Alt+Left", icon: "back", run: withTab(goBack) },
    { id: "forward", title: "Forward", hint: "Alt+Right", icon: "forward", run: withTab(goForward) },
    { id: "up", title: "Up one folder", hint: "Alt+Up", icon: "up", keywords: "parent", run: withTab(goUp) },
    { id: "refresh", title: "Refresh", hint: "F5", icon: "refresh", keywords: "reload", run: withTab((id) => (recheckAccess(), refresh(id))) },
    { id: "address", title: "Edit the address", hint: "Ctrl+L", keywords: "location path go to", run: () => focusTargets.address?.() },
    { id: "search", title: "Search", hint: "Ctrl+F", icon: "search", keywords: "find", run: () => focusTargets.search?.() },
    {
      id: "search-scope",
      title: everywhere() ? "Search this folder only" : "Search everywhere",
      icon: everywhere() ? "folder" : "globe",
      keywords: "search scope everywhere folder",
      run: () => setEverywhere(!everywhere()),
    },
    ...VIEW_MODES.map(
      (info): Action => ({
        id: `view-${info.mode}`,
        title: info.label,
        hint: `Ctrl+Shift+${info.digit}`,
        icon: info.mode === "details" || info.mode === "list" ? "view-details" : info.mode === "tiles" ? "view-tiles" : "view-icons",
        keywords: "view layout",
        run: () => {
          const current = activeTab();
          if (current) setViewMode(locationKey(current), info.mode);
        },
      }),
    ),
    ...SORTS.map(
      ({ key, label }): Action => ({
        id: `sort-${key}`,
        title: `Sort by ${label}`,
        keywords: "order",
        run: withTab((id) => setSortSpec(id, { key, desc: sort?.key === key ? sort.desc : startsDescending(key) })),
      }),
    ),
    {
      id: "sort-direction",
      title: sort?.desc ? "Sort ascending" : "Sort descending",
      keywords: "order reverse ascending descending",
      run: withTab((id) => {
        const current = activeTab()?.sort;
        if (current) setSortSpec(id, { key: current.key, desc: !current.desc });
      }),
    },
    {
      id: "toggle-hidden",
      title: showHidden() ? "Hide hidden items" : "Show hidden items",
      keywords: "toggle hidden files system",
      run: () => {
        setOption("hidden", !showHidden());
        // Hidden items are left out by the engine, so every tab lists its folder again.
        tabs.forEach((each) => refresh(each.id));
      },
    },
    {
      id: "toggle-extensions",
      title: showExtensions() ? "Hide file extensions" : "Show file extensions",
      keywords: "toggle extensions names",
      run: () => setOption("extensions", !showExtensions()),
    },
    {
      id: "toggle-preview",
      title: paneOpen() ? "Hide the preview pane" : "Show the preview pane",
      hint: "Alt+P",
      keywords: "toggle preview pane",
      run: togglePane,
    },
    {
      id: "quick-look",
      title: "Quick Look",
      hint: "Space",
      keywords: "preview large",
      selection: true,
      run: () => {
        if (focusedItem()) setQuickLookOpen(true);
      },
    },
    { id: "new-folder", title: "New folder", hint: "Ctrl+Shift+N", icon: "folder", folder: true, keywords: "create make directory", run: (host) => host.runCommand("new-folder") },
    { id: "paste", title: "Paste", hint: "Ctrl+V", folder: true, run: (host) => host.runCommand("paste") },
    { id: "undo", title: "Undo", hint: "Ctrl+Z", keywords: "revert", run: withTab(undoLast) },
    { id: "redo", title: "Redo", hint: "Ctrl+Y", run: withTab(redoLast) },
    { id: "cut", title: "Cut", hint: "Ctrl+X", selection: true, keywords: "move", run: (host) => host.runCommand("cut") },
    { id: "copy", title: "Copy", hint: "Ctrl+C", selection: true, run: (host) => host.runCommand("copy") },
    { id: "copy-path", title: "Copy as path", hint: "Ctrl+Shift+C", selection: true, run: (host) => host.runCommand("copy-path") },
    { id: "rename", title: "Rename", hint: "F2", selection: true, run: (host) => pressInList(host, "F2") },
    { id: "delete", title: "Delete", hint: "Del", selection: true, keywords: "remove recycle bin", run: (host) => host.runCommand("delete") },
    { id: "open-with", title: "Open with…", selection: true, keywords: "application program", run: (host) => host.runCommand("open-with") },
    { id: "properties", title: "Properties", hint: "Alt+Enter", selection: true, run: (host) => host.runCommand("properties") },
    { id: "folder-properties", title: "Folder properties", folder: true, run: (host) => host.runCommand("folder-properties") },
    { id: "select-all", title: "Select all", hint: "Ctrl+A", mainOnly: true, run: (host) => pressInList(host, "a", { ctrlKey: true }) },
    {
      id: "more-options",
      title: "Show more options",
      hint: "Shift+F10",
      keywords: "windows context menu shell",
      mainOnly: true,
      run: (host) => pressInList(host, "F10", { shiftKey: true }),
    },
    { id: "launcher-settings", title: "Quick launcher settings", keywords: "shortcut hotkey tray startup preferences options", run: (host) => host.openSettings() },
    { id: "show-kestrel", title: "Show Kestrel", keywords: "window open main", run: () => {} },
    { id: "quit", title: "Quit Kestrel", keywords: "exit close", run: () => void paletteApi.quit() },
  ];
}

/** Whether an action belongs in the palette now. */
function available(action: Action, context: PaletteContext): boolean {
  if (context.window === "quick") return !action.selection && !action.mainOnly;
  if (action.id === "show-kestrel") return false;
  const tab = activeTab();
  if (action.selection && (tab?.selected ?? 0) === 0) return false;
  if (action.folder && (!tab || isSearch(tab))) return false;
  return true;
}

function toItem(action: Action): PaletteItem {
  return { provider: ID, id: action.id, title: action.title, hint: action.hint, icon: action.icon, keywords: action.keywords ?? null, boost: 10 };
}

/**
 * The actions provider. The main window passes `host`, which actions need to run; the quick
 * launcher lists actions without one and hands a chosen action to the main window.
 */
export function actionsProvider(host?: ActionHost): PaletteProvider {
  return {
    id: ID,
    prefix: ">",
    label: () => "Action",
    listWhenEmpty: true,
    items: (context) => actions().filter((action) => available(action, context)).map(toItem),
    run: (hit: PaletteHit) => {
      const action = actions().find((each) => each.id === hit.id);
      if (action) action.run(host ?? DETACHED);
    },
    // Quitting needs no window; everything else acts on the main window's tabs.
    runsAnywhere: (hit) => hit.id === "quit",
    complete: (hit) => `>${hit.title}`,
  };
}
