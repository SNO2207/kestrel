import { For, createSignal, onCleanup, onMount } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { baseName } from "../lib/format";
import { activateTab, activeId, closeTab, isSearch, openTab, tabs, type Tab } from "../state";
import { Icon } from "./Icon";

function tabTitle(tab: Tab): string {
  if (isSearch(tab)) return `Search: ${tab.query}`;
  return baseName(tab.path) || tab.path;
}

export function TitleBar() {
  const appWindow = getCurrentWindow();
  const [maximized, setMaximized] = createSignal(false);

  onMount(() => {
    const sync = async () => setMaximized(await appWindow.isMaximized());
    void sync();
    const unlisten = appWindow.onResized(sync);
    onCleanup(() => void unlisten.then((stop) => stop()));
  });

  return (
    <header class="titlebar" data-tauri-drag-region>
      <div class="tabs" role="tablist">
        <For each={tabs}>
          {(tab) => (
            <div
              class="tab"
              classList={{ active: tab.id === activeId() }}
              role="tab"
              aria-selected={tab.id === activeId()}
              title={isSearch(tab) ? tab.query : tab.path}
              onMouseDown={(event) => {
                if (event.button === 0) activateTab(tab.id);
                // Middle-click closes, as in a browser.
                if (event.button === 1) closeTab(tab.id);
              }}
            >
              <span class="tab-title">{tabTitle(tab)}</span>
              <button
                class="tab-close"
                aria-label="Close tab"
                onMouseDown={(event) => event.stopPropagation()}
                onClick={() => closeTab(tab.id)}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          )}
        </For>
        <button class="tab-new" aria-label="New tab" title="New tab (Ctrl+T)" onClick={() => openTab()}>
          <Icon name="plus" size={14} />
        </button>
      </div>
      <div class="window-controls">
        <button aria-label="Minimize" onClick={() => appWindow.minimize()}>
          <Icon name="minimize" />
        </button>
        <button aria-label={maximized() ? "Restore" : "Maximize"} onClick={() => appWindow.toggleMaximize()}>
          <Icon name={maximized() ? "restore" : "maximize"} />
        </button>
        <button class="close" aria-label="Close" onClick={() => appWindow.close()}>
          <Icon name="close" />
        </button>
      </div>
    </header>
  );
}
