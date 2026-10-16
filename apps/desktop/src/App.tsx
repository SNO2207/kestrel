import { For, Show, onCleanup, onMount } from "solid-js";
import { Dynamic } from "solid-js/web";

import { api, type Row } from "./lib/api";
import { locationOf } from "./lib/locations";
import { startPlaces } from "./placesSetup";
import { setDiskUsageHandler } from "./places";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { locationKey, setViewMode, shortcutView } from "./lib/views";
import { LOCKED_MESSAGE, isLocked, recheckAccess } from "./lib/access";
import { BatchRenameDialog, openBatchRename } from "./components/BatchRename";
import { CommandBar } from "./components/CommandBar";
import { DetailsPane } from "./components/DetailsPane";
import { ParityLayer } from "./components/ParityLayer";
import { registerSelector } from "./lib/selectionOps";
import { copyFromArchive, isArchiveFile, openEntry, runArchiveCommand, tabInArchive } from "./archives";
import { ArchiveDialogs, ArchiveJobs } from "./components/ArchiveDialogs";
import { DiskHub } from "./components/DiskHub";
import { analysableFolder, openDiskUsage } from "./disk";
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
import { SearchBanner } from "./components/SearchBanner";
import { startSearch, stillSearching } from "./search";
import { startTriage } from "./triage";
import { startWsl } from "./wsl";

export function App() {
  /** Focus functions for each tab's list, so shortcuts can hand focus back to the files. */
  const listFocus = new Map<number, () => void>();
  /** The same for the page of a tab at a special location (lib/locations.ts), which wins. */
  const pageFocus = new Map<number, () => void>();
  const focusList = () => (pageFocus.get(activeId()) ?? listFocus.get(activeId()))?.();
  /** The special location a tab's page shows, unless it shows search results or a failure. */
  const pageOf = (tab: (typeof tabs)[number]) => (tab.query === "" && tab.listing === null && !tab.error ? locationOf(tab.path) : null);
  bindListFocus((tabId) => (pageFocus.get(tabId) ?? listFocus.get(tabId))?.());
  startPanes();

  /** The item menu's commands for the other pane, while there is one. */
  const otherPaneEntries = (tabId: number): MenuEntry[] => {
    if (!dual()) return [];
    const none = otherPaneFolder(tabId) === null;
    const copy: MenuEntry = { label: "Copy to other pane", hint: "Alt+C", disabled: none, run: () => void sendToOtherPane(tabId, "copy") };
    // Items inside an archive are extracted there; nothing moves out of an archive.
    if (tabInArchive(tabId)) return [copy];
    return [copy, { label: "Move to other pane", hint: "Alt+X", disabled: none, run: () => void sendToOtherPane(tabId, "move") }];
  };

  const open = (tabId: number, row: Row, inNewTab: boolean) => {
    // An archive opens as a folder, as in Explorer (archives.ts).
    const folder = row.dir || isArchiveFile(row);
    if (isLocked(row)) notify(LOCKED_MESSAGE, "info");
    else if (folder && inNewTab) openTab(row.path);
    else if (folder) navigate(tabId, row.path);
    else if (tabInArchive(tabId)) void openEntry(tabId, row);
    else void api.openPath(row.path).catch(notify);
  };

  const runCommand = (tabId: number, command: ListCommand) => {
    switch (command) {
      case "cut":
        return void copySelection(tabId, true);
      case "copy":
        return void (tabInArchive(tabId) ? copyFromArchive(tabId) : copySelection(tabId, false));
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
      case "extract-all":
      case "extract-here":
      case "extract-items":
      case "compress-zip":
      case "compress-to":
        return runArchiveCommand(tabId, command);
      case "disk-usage":
        return openDiskUsage(analysableFolder(tabs.find((tab) => tab.id === tabId)) ?? undefined);
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
    // Phrases and searches inside files, the Downloads triage page's count, WSL's distributions.
    startSearch();
    startTriage();
    startWsl();
    void start(openLayout).then(focusList);
    startPlaces();
    // This PC's drives and the folder tree's drives open the disk usage hub.
    setDiskUsageHandler((path) => openDiskUsage(path));
    const stopDrops = listenForDrops();
    onCleanup(() => void stopDrops.then((stop) => stop()));
  });

  return (
    <div class="app">
      <TitleBar />
      <Toolbar focusList={focusList} />
      <CommandBar runCommand={(command) => runCommand(activeId(), command)} focusList={focusList} />
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
                "location-panel": pageOf(tab) !== null,
              }}
              data-pane={tab.pane}
              data-panel-tab={tab.id}
              // A click or the keyboard arriving in the other pane gives that pane the keyboard.
              onFocusIn={() => tab.id !== activeId() && dual() && activateTab(tab.id)}
              onMouseDown={() => tab.id !== activeId() && dual() && activateTab(tab.id)}
            >
              <Show when={pageOf(tab)}>
                {(location) => (
                  <Dynamic
                    component={location().page}
                    tabId={tab.id}
                    address={tab.path}
                    active={tab.id === activeId()}
                    revision={tab.revision}
                    focusRef={(focus: () => void) => {
                      pageFocus.set(tab.id, focus);
                      onCleanup(() => {
                        if (pageFocus.get(tab.id) === focus) pageFocus.delete(tab.id);
                      });
                    }}
                  />
                )}
              </Show>
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
                // A location's page reports its own focused item (HomeView); the list beneath is hidden.
                onFocusRow={(row) => !pageOf(tab) && setFocusedRow(tab.id, row)}
                stepRef={(step) => onCleanup(registerStepper(tab.id, step))}
                selectRef={(control) => onCleanup(registerSelector(tab.id, control))}
                // How the search was read, and a search inside files' progress (search.ts).
                banner={<SearchBanner tabId={tab.id} />}
                emptyMessage={stillSearching(tab.id) ? "Searching inside files…" : null}
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
        <DetailsPane />
        <ProjectsHub />
        <DiskHub />
        <DropHint />
        <ArchiveJobs />
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
      <ConfirmDialog onClose={focusList} />
      <ArchiveDialogs focusList={focusList} />
    </div>
  );
}
