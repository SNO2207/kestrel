import { For, Show, onMount } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { collapsibleSections } from "../lib/sidebarSections";
import { baseName } from "../lib/format";
import { RECYCLE_BIN, collapsedSections, samePlace, setSectionCollapsed } from "../places";
import { activeTab, navigate, openTab, places } from "../state";
import { closeProjects, projectsOpen, toggleProjects } from "../projects";
import { closeDiskUsage, diskOpen } from "../disk";
import { DiskEntry } from "./DiskEntry";
import { FolderTree } from "./FolderTree";
import { GalleryEntry } from "./GalleryEntry";
import { HomeEntry } from "./HomeEntry";
import { Icon, hasIcon } from "./Icon";
import { PinnedPlaces } from "./PinnedPlaces";
import { ParityIcon } from "./ParityIcon";
import { ProjectIcon } from "./ProjectIcon";
import { ShortcutLists } from "./ShortcutLists";
import { SidebarResizer } from "./SidebarResizer";
import { TriageEntry } from "./TriageEntry";
import { WslPlaces } from "./WslPlaces";

/**
 * The sidebar: the projects hub, pinned places, places, frequent folders, saved searches, tags
 * and workspaces above, each section folding away from its heading; and below them the folder
 * tree, This PC and its drives, which scrolls on its own so a deep tree never pushes the
 * sections above out of reach.
 */
export function Sidebar() {
  let shelves!: HTMLDivElement;
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

  onMount(() => collapsibleSections(shelves, collapsedSections, setSectionCollapsed));

  return (
    <nav class="sidebar" aria-label="Places">
      <div ref={shelves} class="sidebar-shelves">
        <section>
          <HomeEntry />
          <GalleryEntry />
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
          <TriageEntry />
        </section>
        <PinnedPlaces />

        <section aria-label="Places">
          <h2>Places</h2>
          <Show when={places().home}>
            <button
              class="place"
              classList={{ current: current() === places().home.toLowerCase(), "drop-target": isDropPlace(places().home) }}
              data-place={places().home}
              data-place-name={baseName(places().home) || "Home"}
              title={places().home}
              onClick={[open, places().home]}
              onAuxClick={[open, places().home]}
            >
              {/* Your user folder, named as Explorer names it; Home is Explorer's start page. */}
              <ParityIcon name="person" />
              <span>{baseName(places().home) || "Home"}</span>
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
          <button
            class="place recycle-bin-place"
            classList={{ current: samePlace(current(), RECYCLE_BIN) }}
            title="Recycle Bin"
            onClick={[open, RECYCLE_BIN]}
            onAuxClick={[open, RECYCLE_BIN]}
          >
            <Icon name="bin" />
            <span>Recycle Bin</span>
          </button>
        </section>
        <WslPlaces />
        <ShortcutLists />
      </div>
      <FolderTree />
      <SidebarResizer />
    </nav>
  );
}
