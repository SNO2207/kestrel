import { For, Show, onCleanup, onMount } from "solid-js";

import { api, type Row } from "./lib/api";
import { locationKey, setViewMode, shortcutView } from "./lib/views";
import { LOCKED_MESSAGE, isLocked, recheckAccess } from "./lib/access";
import { BatchRenameDialog, openBatchRename } from "./components/BatchRename";
import { CommandBar } from "./components/CommandBar";
import { DetailsPane } from "./components/DetailsPane";
import { HomeView } from "./components/HomeView";
import { ParityLayer } from "./components/ParityLayer";
import { showsHome } from "./lib/home";
import { registerSelector } from "./lib/selectionOps";
import { isDropFolder, listenForDrops } from "./dragdrop";
import { DropHint } from "./components/DropHint";
import { FileList, type ListCommand } from "./components/FileList";
import { LauncherSettingsDialog } from "./components/LauncherSettings";
import { PaletteOverlay } from "./components/PaletteOverlay";
import { PreviewPane } from "./components/PreviewPane";
import { ProjectsHub } from "./components/ProjectsHub";
import { QuickLook } from "./components/QuickLook";
import { ShortcutsLayer, runShortcutCommand } from "./components/ShortcutsLayer";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { TitleBar } from "./components/TitleBar";
import { Toolbar, focusTargets } from "./components/Toolbar";
import {
  activeId,
  activeTab,
  clearPending,
  closeTab,
  copyPaths,
  copySelection,
  createFolder,
  cycleTab,
  deleteSelection,
  dismissNotice,
  goBack,
  goForward,
  goUp,
  navigate,
  notice,
  noticeTone,
  notify,
  openTab,
  openWith,
  paste,
  refresh,
  rename,
  reportSelection,
  setRenaming,
  setSort,
  showProperties,
  showShellMenu,
  start,
  tabs,
} from "./state";
import { redoLast, undoLast } from "./undo";
import { registerStepper, setFocusedRow } from "./previewState";

export function App() {
  /** Focus functions for each tab's list, so shortcuts can hand focus back to the files. */
  const listFocus = new Map<number, () => void>();
  const focusList = () => listFocus.get(activeId())?.();

  const open = (tabId: number, row: Row, inNewTab: boolean) => {
    if (isLocked(row)) notify(LOCKED_MESSAGE, "info");
    else if (row.dir && inNewTab) openTab(row.path);
    else if (row.dir) navigate(tabId, row.path);
    else void api.openPath(row.path).catch(notify);
  };

  const runCommand = (tabId: number, command: ListCommand) => {
    switch (command) {
      case "cut":
        return void copySelection(tabId, true);
      case "copy":
        return void copySelection(tabId, false);
      case "paste":
        return paste(tabId);
      case "copy-path":
        return void copyPaths(tabId);
      case "delete":
        return deleteSelection(tabId, false);
      case "erase":
        return deleteSelection(tabId, true);
      case "new-folder":
        return createFolder(tabId);
      case "properties":
        return void showProperties(tabId, false);
      case "folder-properties":
        return void showProperties(tabId, true);
      case "refresh":
        recheckAccess();
        return refresh(tabId);
      case "undo":
        return undoLast(tabId);
      case "redo":
        return redoLast(tabId);
      case "batch-rename":
        return void openBatchRename(tabId);
      case "open-with":
        return void openWith(tabId);
      case "pin":
      case "pin-folder":
      case "tags":
        return runShortcutCommand(tabId, command);
    }
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const tab = activeTab();
    if (!tab) return;
    const typing = event.target instanceof HTMLInputElement;
    const key = event.key.toLowerCase();
    const handled = () => event.preventDefault();

    if (event.altKey && key === "arrowleft") return handled(), goBack(tab.id);
    if (event.altKey && key === "arrowright") return handled(), goForward(tab.id);
    if (event.altKey && key === "arrowup") return handled(), goUp(tab.id);
    if ((event.altKey && key === "d") || (event.ctrlKey && key === "l")) return handled(), focusTargets.address?.();
    // Ctrl+Shift+E is the folder tree's (see ParityLayer), not the search box's.
    if ((event.ctrlKey && !event.shiftKey && (key === "f" || key === "e")) || key === "f3") return handled(), focusTargets.search?.();
    if (event.ctrlKey && key === "t") return handled(), openTab();
    if (event.ctrlKey && key === "w") return handled(), closeTab(tab.id);
    if (event.ctrlKey && key === "tab") return handled(), cycleTab(event.shiftKey ? -1 : 1);
    if (key === "f5") return handled(), recheckAccess(), refresh(tab.id);
    if (key === "backspace" && !typing) return handled(), goBack(tab.id);
    const view = shortcutView(event);
    if (view) return handled(), setViewMode(locationKey(tab), view);
    // Text fields keep their own undo.
    if (event.ctrlKey && !typing && key === "z" && !event.shiftKey) return handled(), undoLast(tab.id);
    if (event.ctrlKey && !typing && (key === "y" || (key === "z" && event.shiftKey))) return handled(), redoLast(tab.id);
  };

  // The web view's own context menu has no place in a file manager; text fields keep theirs.
  const onContextMenu = (event: MouseEvent) => {
    if (!(event.target instanceof HTMLInputElement)) event.preventDefault();
  };

  onMount(() => {
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("contextmenu", onContextMenu);
    onCleanup(() => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("contextmenu", onContextMenu);
    });
    void start().then(focusList);
    const stopDrops = listenForDrops();
    onCleanup(() => void stopDrops.then((stop) => stop()));
  });

  return (
    <div class="app">
      <TitleBar />
      <Toolbar focusList={focusList} />
      <CommandBar runCommand={(command) => runCommand(activeId(), command)} focusList={focusList} />
      <Sidebar />
      <main class="content">
        <For each={tabs}>
          {(tab) => (
            <div class="tab-panel" classList={{ active: tab.id === activeId(), "drop-target": isDropFolder(tab.id) }}>
              <Show
                when={!showsHome(tab)}
                fallback={
                  <HomeView
                    tabId={tab.id}
                    focusRef={(focus) => {
                      listFocus.set(tab.id, focus);
                      onCleanup(() => listFocus.get(tab.id) === focus && listFocus.delete(tab.id));
                    }}
                  />
                }
              >
              {/* Not indented under the Show, so other branches' changes to these props merge cleanly. */}
              <FileList
                listing={tab.listing}
                revision={tab.revision}
                sort={tab.sort}
                query={tab.query}
                error={tab.error}
                restore={tab.restore}
                pending={tab.pending}
                onSort={(key) => setSort(tab.id, key)}
                onOpen={(row, inNewTab) => open(tab.id, row, inNewTab)}
                onStale={() => refresh(tab.id)}
                onSelectionChange={(selection, focus) => reportSelection(tab.id, selection, focus)}
                onPendingDone={() => clearPending(tab.id)}
                onRenaming={(active) => setRenaming(tab.id, active)}
                onRename={(row, name) => rename(tab.id, row, name)}
                onCommand={(command) => runCommand(tab.id, command)}
                onShellMenu={(x, y, onItems, extended) => showShellMenu(tab.id, x, y, onItems, extended)}
                focusRef={(focus) => {
                  listFocus.set(tab.id, focus);
                  onCleanup(() => listFocus.delete(tab.id));
                }}
                onFocusRow={(row) => setFocusedRow(tab.id, row)}
                stepRef={(step) => onCleanup(registerStepper(tab.id, step))}
                selectRef={(control) => onCleanup(registerSelector(tab.id, control))}
              />
              </Show>
            </div>
          )}
        </For>
        <PreviewPane />
        <DetailsPane />
        <ProjectsHub />
        <DropHint />
        <Show when={notice()}>
          <div class="notice" classList={{ info: noticeTone() === "info" }} role="alert">
            <span>{notice()}</span>
            <button aria-label="Dismiss" onClick={dismissNotice}>
              Dismiss
            </button>
          </div>
        </Show>
      </main>
      <StatusBar />
      <BatchRenameDialog onClose={focusList} />
      <QuickLook focusList={focusList} />
      <ShortcutsLayer focusList={focusList} />
      <ParityLayer focusList={focusList} />
      <PaletteOverlay runCommand={(command) => runCommand(activeId(), command)} focusList={focusList} />
      <LauncherSettingsDialog onClose={focusList} />
    </div>
  );
}
