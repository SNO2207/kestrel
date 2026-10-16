import { For, Show } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { samePlace } from "../places";
import { activeTab, navigate, openTab } from "../state";
import { wslDistros } from "../wsl";
import { SearchIcon } from "./SearchIcon";

/**
 * The sidebar's Linux section: each WSL distribution, opening its root. It shows only when WSL
 * is installed, and folds from its heading like the other sections.
 */
export function WslPlaces() {
  const current = () => activeTab()?.path ?? "";
  const open = (path: string, event: MouseEvent) => {
    const tab = activeTab();
    // Ctrl-click and middle-click open a new tab, as they do for places.
    if (event.ctrlKey || event.button === 1 || !tab) openTab(path);
    else navigate(tab.id, path);
  };
  return (
    <Show when={wslDistros().length > 0}>
      <section aria-label="Linux">
        <h2>Linux</h2>
        <For each={wslDistros()}>
          {(distro) => (
            <button
              class="place wsl-place"
              classList={{ current: samePlace(current(), distro.path), "drop-target": isDropPlace(distro.path) }}
              title={`${distro.path}${distro.default ? " (default)" : ""}`}
              data-place={distro.path}
              data-place-name={distro.name}
              onClick={[open, distro.path]}
              onAuxClick={[open, distro.path]}
            >
              <SearchIcon name="linux" />
              <span>{distro.name}</span>
            </button>
          )}
        </For>
      </section>
    </Show>
  );
}
