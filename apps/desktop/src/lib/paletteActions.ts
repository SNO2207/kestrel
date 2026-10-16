/**
 * The palette's actions: every command the app has, each with its shortcut.
 *
 * Actions run in the main window. The quick launcher lists them too, minus those that act on
 * the selected files, and hands a chosen one to the main window (palette.ts).
 */
import { inArchive } from "../archives";
import type { ListCommand } from "../components/FileList";
import { openGoto } from "../components/GoToFolder";
import { askName } from "../components/ShortcutDialogs";
import { focusTargets } from "../components/Toolbar";
import { projectsOpen, toggleProjects } from "../projects";
import { saveSearch, saveWorkspace } from "../shortcuts";
import { paneOpen, setQuickLookOpen, focusedItem, togglePane } from "../previewState";
import {
  activeTab,
  closeTab,
  cycleTab,
  dual,
  everywhere,
  goBack,
  goForward,
  goUp,
  isSearch,
  navigate,
  openTab,
  refresh,
  selectedRows,
  setEverywhere,
  setGrouping,
  setSortSpec,
  startsDescending,
  tabs,
} from "../state";
import { redoLast, undoLast } from "../undo";
import { openFilter, sendToOtherPane, toggleSecondPane } from "../panes";
import {
  detailsOpen,
  invertSelection,
  openInTerminal,
  openNewWindow,
  revealInFolder,
  selectNone,
  setOptionsOpen,
  toggleDetails,
  toggleFullScreen,
} from "../parity";
import { HOME_PATH } from "./home";
import { recheckAccess } from "./access";
import type { SortKey } from "./api";
import type { GroupBy } from "./layoutApi";
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
  /** Listed only while this holds, in the main window. */
  when?: () => boolean;
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

const GROUPINGS: { by: GroupBy; label: string }[] = [
  { by: "name", label: "name" },
  { by: "modified", label: "date modified" },
  { by: "type", label: "type" },
  { by: "size", label: "size" },
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

/** Saves the active tab's search to the sidebar, under a name asked for. */
async function saveTheSearch() {
  const tab = activeTab();
  if (!tab || tab.query.trim() === "") return;
  const answer = await askName({ title: "Save search", label: "Name", initial: tab.query.trim(), action: "Save" });
  if (answer) await saveSearch(tab.id, answer.name);
}

/** Saves the open tabs as a workspace, under a name asked for. */
async function saveTheTabs() {
  const answer = await askName({ title: "Save tabs as a workspace", label: "Name", initial: "", placeholder: "Writing, Taxes 2026…", action: "Save" });
  if (answer) await saveWorkspace(answer.name);
}

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
    { id: "go-to-folder", title: "Go to folder", hint: "Ctrl+G", icon: "folder", keywords: "jump frequent recent visited", run: () => openGoto() },
    {
      id: "projects",
      title: projectsOpen() ? "Close projects" : "Open projects",
      hint: "Ctrl+Shift+P",
      icon: "project:projects",
      keywords: "projects repositories git code hub",
      run: toggleProjects,
    },
    { id: "search", title: "Search", hint: "Ctrl+F", icon: "search", keywords: "find", run: () => focusTargets.search?.() },
    {
      id: "search-scope",
      title: everywhere() ? "Search this folder only" : "Search everywhere",
      icon: everywhere() ? "folder" : "globe",
      keywords: "search scope everywhere folder",
      run: () => setEverywhere(!everywhere()),
    },
    {
      id: "save-search",
      title: "Save this search",
      icon: "shortcut:save",
      keywords: "saved searches sidebar keep",
      mainOnly: true,
      when: () => (activeTab()?.query.trim() ?? "") !== "",
      run: () => void saveTheSearch(),
    },
    ...VIEW_MODES.map(
      (info): Action => ({
        id: `view-${info.mode}`,
        title: info.label,
        hint: `Ctrl+Shift+${info.digit}`,
        icon: info.mode === "details" || info.mode === "list" ? "view-details" : info.mode === "tiles" || info.mode === "content" ? "view-tiles" : "view-icons",
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
    // The second pane, the filter box and grouping (panes.ts, layout.rs).
    {
      id: "second-pane",
      title: dual() ? "Hide the second pane" : "Show the second pane",
      hint: "Ctrl+Shift+D",
      keywords: "dual two panes split side by side",
      mainOnly: true,
      run: toggleSecondPane,
    },
    { id: "filter", title: "Filter by name", hint: "Ctrl+Shift+L", keywords: "narrow names wildcard", mainOnly: true, run: withTab(openFilter) },
    {
      id: "copy-to-other-pane",
      title: "Copy to other pane",
      hint: "Alt+C",
      selection: true,
      mainOnly: true,
      when: dual,
      run: withTab((id) => void sendToOtherPane(id, "copy")),
    },
    {
      id: "move-to-other-pane",
      title: "Move to other pane",
      hint: "Alt+X",
      selection: true,
      mainOnly: true,
      when: dual,
      run: withTab((id) => void sendToOtherPane(id, "move")),
    },
    ...GROUPINGS.map(
      ({ by, label }): Action => ({
        id: `group-${by}`,
        title: `Group by ${label}`,
        keywords: "groups arrange",
        mainOnly: true,
        run: withTab((id) => setGrouping(id, { by, desc: by === "modified" || by === "size" })),
      }),
    ),
    { id: "ungroup", title: "Ungroup", keywords: "groups none", mainOnly: true, run: withTab((id) => setGrouping(id, null)) },
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
    { id: "pin", title: "Pin to sidebar", hint: "Ctrl+D", icon: "shortcut:pin", selection: true, keywords: "pinned places favourites bookmark", run: (host) => host.runCommand("pin") },
    { id: "pin-folder", title: "Pin this folder", icon: "shortcut:pin", folder: true, keywords: "pinned places favourites bookmark sidebar", run: (host) => host.runCommand("pin-folder") },
    { id: "tags", title: "Tags…", icon: "shortcut:tag", selection: true, keywords: "tag label colour color", run: (host) => host.runCommand("tags") },
    { id: "compress-zip", title: "Compress to ZIP file", selection: true, folder: true, mainOnly: true, keywords: "zip archive pack", when: () => !inArchive(activeTab()?.listing), run: (host) => host.runCommand("compress-zip") },
    { id: "compress-to", title: "Compress to…", selection: true, folder: true, mainOnly: true, keywords: "zip 7z tar gz archive pack", when: () => !inArchive(activeTab()?.listing), run: (host) => host.runCommand("compress-to") },
    { id: "extract-all", title: "Extract all…", mainOnly: true, keywords: "unzip unpack archive", when: () => inArchive(activeTab()?.listing), run: (host) => host.runCommand("extract-all") },
    { id: "save-workspace", title: "Save tabs as a workspace", icon: "shortcut:workspace", keywords: "workspaces session tabs keep", run: () => void saveTheTabs() },
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
    // Explorer parity (parity.ts).
    { id: "home", title: "Go to Home", icon: "home", keywords: "start page quick access favorites recent", run: withTab((id) => navigate(id, HOME_PATH)) },
    { id: "new-window", title: "New window", hint: "Ctrl+N", keywords: "open another window", run: () => openNewWindow() },
    {
      id: "toggle-details",
      title: detailsOpen() ? "Hide the details pane" : "Show the details pane",
      hint: "Alt+Shift+P",
      keywords: "toggle details pane properties info",
      run: toggleDetails,
    },
    { id: "full-screen", title: "Full screen", hint: "F11", keywords: "maximize window", mainOnly: true, run: () => void toggleFullScreen() },
    { id: "select-none", title: "Select none", selection: true, mainOnly: true, keywords: "deselect clear selection", run: withTab(selectNone) },
    { id: "invert-selection", title: "Invert selection", mainOnly: true, keywords: "reverse selection", run: withTab(invertSelection) },
    {
      id: "open-terminal",
      title: "Open in Terminal",
      folder: true,
      keywords: "console shell command prompt powershell windows terminal",
      run: withTab((id) => {
        const current = tabs.find((each) => each.id === id);
        if (current) openInTerminal(current.path);
      }),
    },
    {
      id: "open-file-location",
      title: "Open file location",
      selection: true,
      keywords: "show in folder reveal",
      when: () => {
        const current = activeTab();
        return !!current && isSearch(current);
      },
      run: withTab(async (id) => {
        const [row] = await selectedRows(id, 1);
        if (row) revealInFolder(row.path);
      }),
    },
    { id: "folder-options", title: "Folder options", keywords: "settings preferences hidden protected files single click full path sync", run: () => setOptionsOpen(true) },
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
  if (action.when && !action.when()) return false;
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
