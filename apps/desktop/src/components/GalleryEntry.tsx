import { locationOf } from "../lib/locations";
import { closeDiskUsage, diskOpen } from "../disk";
import { GALLERY, MEDIA_LIBRARY } from "../media";
import { closeProjects, projectsOpen } from "../projects";
import { activeTab, navigate, openTab } from "../state";
import { MediaIcon } from "./MediaIcon";

/**
 * The Gallery under Home at the top of the sidebar, as in Explorer's navigation pane. Drawn as
 * a place but not one: it is no folder, and nothing can be dropped on it.
 */
export function GalleryEntry() {
  const current = () => {
    const tab = activeTab();
    const address = tab && !tab.listing ? locationOf(tab.path)?.address : undefined;
    return !projectsOpen() && !diskOpen() && (address === GALLERY || address === MEDIA_LIBRARY);
  };
  const open = (event: MouseEvent) => {
    closeProjects();
    closeDiskUsage();
    const tab = activeTab();
    if (event.ctrlKey || event.button === 1 || !tab) openTab(GALLERY);
    else navigate(tab.id, GALLERY);
  };
  return (
    <button
      class="gallery-entry"
      classList={{ current: current() }}
      title="Gallery: every photo and video in your media folders"
      aria-current={current() ? "page" : undefined}
      onClick={open}
      onAuxClick={open}
    >
      <MediaIcon name="gallery" />
      <span>Gallery</span>
    </button>
  );
}
