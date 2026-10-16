import { Show } from "solid-js";

import { closeDiskUsage, diskOpen } from "../disk";
import { locationOf } from "../lib/locations";
import { closeProjects, projectsOpen } from "../projects";
import { activeTab } from "../state";
import { TRIAGE, newDownloads, openTriage } from "../triage";
import { SearchIcon } from "./SearchIcon";

/**
 * The sidebar's way to the Downloads triage page, with a badge counting what arrived since the
 * owner last looked. Drawn like a place, but it is a page: nothing can be dropped on it.
 */
export function TriageEntry() {
  const current = () => !projectsOpen() && !diskOpen() && locationOf(activeTab()?.path ?? "")?.address === TRIAGE;
  const open = (event: MouseEvent) => {
    closeProjects();
    closeDiskUsage();
    openTriage(event.ctrlKey || event.button === 1);
  };
  const label = () => (newDownloads() === 1 ? "1 new download" : `${newDownloads()} new downloads`);
  return (
    <button
      class="triage-entry"
      classList={{ current: current() }}
      title={newDownloads() > 0 ? `New downloads: ${label()}` : "New downloads: nothing new since you last looked"}
      aria-current={current() ? "page" : undefined}
      onClick={open}
      onAuxClick={open}
    >
      <SearchIcon name="arrived" />
      <span>New downloads</span>
      <Show when={newDownloads() > 0}>
        <span class="triage-badge" aria-label={label()}>
          {newDownloads() > 99 ? "99+" : newDownloads()}
        </span>
      </Show>
    </button>
  );
}
