import { For, Show } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { formatSize } from "../lib/format";
import { activeTab, navigate, openTab, places } from "../state";
import { closeProjects, projectsOpen, toggleProjects } from "../projects";
import { closeDiskUsage, diskOpen } from "../disk";
import { DiskEntry } from "./DiskEntry";
import { Icon, hasIcon } from "./Icon";
import { PinnedPlaces } from "./PinnedPlaces";
import { ProjectIcon } from "./ProjectIcon";
import { ShortcutLists } from "./ShortcutLists";

export function Sidebar() {
  // While the projects or disk usage hub covers the list, no place is the current one.
  const current = () => (projectsOpen() || diskOpen() ? "" : (activeTab()?.path.toLowerCase() ?? ""));
  const open = (path: string, event: MouseEvent) => {
    closeProjects();
    closeDiskUsage();
    const tab = activeTab();
    // Ctrl-click and middle-click open a new tab, as they do for links.
    if (event.ctrlKey || event.button === 1 || !tab) openTab(path);
    else navigate(tab.id, path);
  };

  return (
    <nav class="sidebar" aria-label="Places">
      <section>
        <button
          class="projects-entry"
          classList={{ current: projectsOpen() }}
          title="Projects (Ctrl+Shift+P)"
          aria-pressed={projectsOpen()}
          onClick={toggleProjects}
        >
          <ProjectIcon name="projects" />
          <span>Projects</span>
        </button>
        <DiskEntry />
      </section>
      <PinnedPlaces />

      <section>
        <h2>Places</h2>
        <Show when={places().home}>
          <button
            class="place"
            classList={{ current: current() === places().home.toLowerCase(), "drop-target": isDropPlace(places().home) }}
            data-place={places().home}
            data-place-name="Home"
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
              classList={{ current: current() === folder.path.toLowerCase(), "drop-target": isDropPlace(folder.path) }}
              title={folder.path}
              data-place={folder.path}
              data-place-name={folder.name}
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
                classList={{ current: current() === drive.path.toLowerCase(), "drop-target": isDropPlace(drive.path) }}
                data-place={drive.path}
                data-place-name={`${drive.label || "Local Disk"} (${drive.path.slice(0, 2)})`}
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
      <ShortcutLists />
    </nav>
  );
}
