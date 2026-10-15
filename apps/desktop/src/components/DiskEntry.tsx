import { closeDiskUsage, diskOpen, driveOf, openDiskUsage } from "../disk";
import { activeTab, places } from "../state";
import { DiskIcon } from "./DiskIcon";

/**
 * The sidebar's way into the disk usage hub: the whole drive the folder on screen is on. (The
 * shortcut and the folder's menu open it on the folder itself.)
 */
export function DiskEntry() {
  const drive = () => {
    const path = activeTab()?.path ?? places().drives[0]?.path;
    return path ? driveOf(path) : undefined;
  };
  return (
    <button
      class="disk-entry"
      classList={{ current: diskOpen() }}
      title="Disk usage (Ctrl+Shift+U for this folder)"
      aria-pressed={diskOpen()}
      onClick={() => (diskOpen() ? closeDiskUsage() : openDiskUsage(drive()))}
    >
      <DiskIcon name="usage" />
      <span>Disk usage</span>
    </button>
  );
}
