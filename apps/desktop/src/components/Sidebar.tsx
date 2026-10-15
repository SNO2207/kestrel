import { For, Show } from "solid-js";

import { formatSize } from "../lib/format";
import { activeTab, navigate, openTab, places } from "../state";
import { Icon, hasIcon } from "./Icon";

export function Sidebar() {
  const current = () => activeTab()?.path.toLowerCase() ?? "";
  const open = (path: string, event: MouseEvent) => {
    const tab = activeTab();
    // Ctrl-click and middle-click open a new tab, as they do for links.
    if (event.ctrlKey || event.button === 1 || !tab) openTab(path);
    else navigate(tab.id, path);
  };

  return (
    <nav class="sidebar" aria-label="Places">
      <section>
        <h2>Places</h2>
        <Show when={places().home}>
          <button
            class="place"
            classList={{ current: current() === places().home.toLowerCase() }}
            onClick={[open, places().home]}
            onAuxClick={[open, places().home]}
          >
            <Icon name="home" />
            <span>Home</span>
          </button>
        </Show>
        <For each={places().folders}>
          {(folder) => (
            <button
              class="place"
              classList={{ current: current() === folder.path.toLowerCase() }}
              title={folder.path}
              onClick={[open, folder.path]}
              onAuxClick={[open, folder.path]}
            >
              <Icon name={hasIcon(folder.kind) ? folder.kind : "folder"} />
              <span>{folder.name}</span>
            </button>
          )}
        </For>
      </section>

      <section>
        <h2>Drives</h2>
        <For each={places().drives}>
          {(drive) => {
            const used = () => (drive.total > 0 ? (drive.total - drive.free) / drive.total : 0);
            return (
              <button
                class="place drive"
                classList={{ current: current() === drive.path.toLowerCase() }}
                onClick={[open, drive.path]}
                onAuxClick={[open, drive.path]}
              >
                <Icon name="drive" />
                <span class="drive-body">
                  <span class="drive-name">
                    {drive.label || "Local Disk"} ({drive.path.slice(0, 2)})
                  </span>
                  <Show when={drive.total > 0}>
                    <span class="meter" classList={{ full: used() > 0.9 }}>
                      <span style={{ width: `${used() * 100}%` }} />
                    </span>
                    <span class="drive-free">{formatSize(drive.free)} free of {formatSize(drive.total)}</span>
                  </Show>
                </span>
              </button>
            );
          }}
        </For>
      </section>
    </nav>
  );
}
