/**
 * Cleanup recommendations and the duplicate finder are tabs of the disk usage hub
 * (`DiskHub.tsx`, panels `CleanupPanel.tsx` and `DuplicatesPanel.tsx`), working on the drive
 * or folder the hub analyses. These are the command palette's ways to them.
 */
import { analysableFolder, driveOf, openDiskUsage, setDiskTab } from "./disk";
import { activeTab, places } from "./state";

/** The hub's tabs for the two panels. */
export type CleanupTab = "cleanup" | "duplicates";

/**
 * Opens the hub on a tab: recommendations for the drive on screen, duplicates in the user
 * folder, where copies collect (and where the finder looked by default before it was a tab).
 */
export function openCleanup(which: CleanupTab) {
  const here = analysableFolder(activeTab());
  const home = places().home;
  const root = which === "cleanup" ? driveOf(here ?? (home || "C:\\")) : home || here || "C:\\";
  setDiskTab(which);
  openDiskUsage(root);
}

/** The palette's actions for the two tabs, listed with the app's own (`lib/paletteActions.ts`). */
export function cleanupActions() {
  return [
    {
      id: "cleanup-recommendations",
      title: "Cleanup recommendations",
      keywords: "clean up free space disk storage junk temporary cache recycle",
      run: () => openCleanup("cleanup"),
    },
    {
      id: "find-duplicates",
      title: "Find duplicate files",
      keywords: "duplicates copies same identical free space disk",
      run: () => openCleanup("duplicates"),
    },
  ];
}
