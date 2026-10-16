import { For, Show, onCleanup, onMount } from "solid-js";

import { api, type Row } from "./lib/api";
import { locationKey, setViewMode, shortcutView } from "./lib/views";
import { LOCKED_MESSAGE, isLocked, recheckAccess } from "./lib/access";
import { BatchRenameDialog, openBatchRename } from "./components/BatchRename";
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
import { groupByEntry } from "./components/GroupMenu";
import { PaneBar, Splitter } from "./components/Panes";
import type { MenuEntry } from "./components/ContextMenu";
import { bindListFocus, layoutKey, openLayout, otherPaneFolder, sendToOtherPane, split, startPanes } from "./panes";
import {
  activateTab,
  dual,
  setFilter,
  shownTab,
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
  bindListFocus((tabId) => listFocus.get(tabId)?.());
  startPanes();

  /** The item menu's commands for the other pane, while there is one. */
  const otherPaneEntries = (tabId: number): MenuEntry[] => {
    if (!dual()) return [];
    const none = otherPaneFolder(tabId) === null;
    return [
      { label: "Copy to other pane", hint: "Alt+C", disabled: none, run: () => void sendToOtherPane(tabId, "copy") },
      { label: "Move to other pane", hint: "Alt+X", disabled: none, run: () => void sendToOtherPane(tabId, "move") },
    ];
  };

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

    // The second pane, the filter box and copying or moving to the other pane (panes.ts).
    if (layoutKey(event)) return handled();
    if (event.altKey && key === "arrowleft") return handled(), goBack(tab.id);
    if (event.altKey && key === "arrowright") return handled(), goForward(tab.id);
    if (event.altKey && key === "arrowup") return handled(), goUp(tab.id);
    if ((event.altKey && key === "d") || (event.ctrlKey && key === "l")) return handled(), focusTargets.address?.();
    if ((event.ctrlKey && (key === "f" || key === "e")) || key === "f3") return handled(), focusTargets.search?.();
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
    void start(openLayout).then(focusList);
    const stopDrops = listenForDrops();
    onCleanup(() => void stopDrops.then((stop) => stop()));
  });

  return (
    <div class="app">
      <TitleBar />
      <Toolbar focusList={focusList} />
      <Sidebar />
      <main class="content" classList={{ dual: dual() }} style={{ "--split": String(split()) }}>
        <For each={tabs}>
          {(tab) => (
            <div
              class="tab-panel"
              classList={{
                active: tab.id === activeId(),
                current: dual() && tab.id === shownTab(tab.pane),
                "drop-target": isDropFolder(tab.id),
              }}
              data-pane={tab.pane}
              data-panel-tab={tab.id}
              // A click or the keyboard arriving in the other pane gives that pane the keyboard.
              onFocusIn={() => tab.id !== activeId() && dual() && activateTab(tab.id)}
              onMouseDown={() => tab.id !== activeId() && dual() && activateTab(tab.id)}
            >
              <FileList
                listing={tab.listing}
                revision={tab.revision}
                sort={tab.sort}
                query={tab.query}
                error={tab.error}
                restore={tab.restore}
                pending={tab.pending}
                groups={tab.groups}
                filter={tab.filter}
                unfilteredTotal={tab.unfiltered?.total ?? null}
                onFilter={(text) => setFilter(tab.id, text)}
                menuExtras={{ items: () => otherPaneEntries(tab.id), folder: () => [groupByEntry(tab.id)] }}
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
              />
            </div>
          )}
        </For>
        <Show when={dual()}>
          <PaneBar pane={0} />
          <PaneBar pane={1} />
          <Splitter />
        </Show>
        <PreviewPane />
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
      <PaletteOverlay runCommand={(command) => runCommand(activeId(), command)} focusList={focusList} />
      <LauncherSettingsDialog onClose={focusList} />
    </div>
  );
}
