import { For, Show, onCleanup, onMount } from "solid-js";

import { api, type Row } from "./lib/api";
import { isDropFolder, listenForDrops } from "./dragdrop";
import { DropHint } from "./components/DropHint";
import { FileList, type ListCommand } from "./components/FileList";
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
  notify,
  openTab,
  paste,
  refresh,
  rename,
  reportSelection,
  setRenaming,
  setSort,
  showProperties,
  start,
  tabs,
} from "./state";

export function App() {
  /** Focus functions for each tab's list, so shortcuts can hand focus back to the files. */
  const listFocus = new Map<number, () => void>();
  const focusList = () => listFocus.get(activeId())?.();

  const open = (tabId: number, row: Row, inNewTab: boolean) => {
    if (row.dir && inNewTab) openTab(row.path);
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
        return refresh(tabId);
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
    if ((event.ctrlKey && (key === "f" || key === "e")) || key === "f3") return handled(), focusTargets.search?.();
    if (event.ctrlKey && key === "t") return handled(), openTab();
    if (event.ctrlKey && key === "w") return handled(), closeTab(tab.id);
    if (event.ctrlKey && key === "tab") return handled(), cycleTab(event.shiftKey ? -1 : 1);
    if (key === "f5") return handled(), refresh(tab.id);
    if (key === "backspace" && !typing) return handled(), goBack(tab.id);
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
      <Sidebar />
      <main class="content">
        <For each={tabs}>
          {(tab) => (
            <div class="tab-panel" classList={{ active: tab.id === activeId(), "drop-target": isDropFolder(tab.id) }}>
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
                focusRef={(focus) => {
                  listFocus.set(tab.id, focus);
                  onCleanup(() => listFocus.delete(tab.id));
                }}
              />
            </div>
          )}
        </For>
        <DropHint />
        <Show when={notice()}>
          <div class="notice" role="alert">
            <span>{notice()}</span>
            <button aria-label="Dismiss" onClick={dismissNotice}>
              Dismiss
            </button>
          </div>
        </Show>
      </main>
      <StatusBar />
    </div>
  );
}
