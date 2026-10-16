/**
 * The navigation pane's Libraries, shown with Folder Options' "Show libraries in the navigation
 * pane" (off by default, as in Explorer): Libraries itself, then each library, opening as one
 * folder of all the folders it includes. A library takes files dropped on it into its save folder.
 */
import { For, Show } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { closeDiskUsage } from "../disk";
import { libraries, parity2Options } from "../parity2";
import { openLocation, samePlace } from "../places";
import { closeProjects } from "../projects";
import { activeTab } from "../state";
import { libraryIcon } from "./LibrariesPage";
import { Icon } from "./Icon";

const LIBRARIES = "shell:Libraries";

export function LibrariesSection() {
  const current = () => activeTab()?.path ?? "";
  const open = (address: string, event: MouseEvent) => {
    closeProjects();
    closeDiskUsage();
    openLocation(address, event.ctrlKey || event.button === 1);
  };
  return (
    <Show when={parity2Options().showLibraries}>
      <section aria-label="Libraries" class="libraries-section">
        <h2>Libraries</h2>
        <button class="place" classList={{ current: samePlace(current(), LIBRARIES) }} title="Libraries" onClick={[open, LIBRARIES]} onAuxClick={[open, LIBRARIES]}>
          <Icon name="library" />
          <span>Libraries</span>
        </button>
        <For each={libraries()}>
          {(library) => (
            <button
              class="place library-place"
              classList={{ current: samePlace(current(), library.address), "drop-target": isDropPlace(library.address) }}
              title={[library.name, ...library.folders].join("\n")}
              data-place={library.save_folder ? library.address : undefined}
              data-place-name={library.name}
              onClick={[open, library.address]}
              onAuxClick={[open, library.address]}
            >
              <Icon name={libraryIcon(library)} />
              <span>{library.name}</span>
            </button>
          )}
        </For>
      </section>
    </Show>
  );
}
