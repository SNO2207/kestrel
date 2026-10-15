import { createSignal } from "solid-js";

import { closeProjects } from "./projects";
import { activeTab, navigate, openTab } from "./state";

/**
 * The cleanup page: cleanup recommendations and the duplicate finder, each a panel made for a
 * tab of the disk usage hub. Until the hub has them, this page of their own shows them, opened
 * from the command palette. It covers the file list while open; going anywhere else (another
 * folder or tab) closes it, as the projects hub does.
 */

export type CleanupTab = "recommendations" | "duplicates";

const [open, setOpen] = createSignal(false);
const [tab, setTab] = createSignal<CleanupTab>("recommendations");
/** The folder the panels look in, or `null` for every drive. */
const [scope, setScope] = createSignal<string | null>(null);

export { open as cleanupOpen, tab as cleanupTab, setTab as setCleanupTab, scope as cleanupScope, setScope as setCleanupScope };

export function openCleanup(which: CleanupTab) {
  closeProjects();
  setTab(which);
  setOpen(true);
}

export const closeCleanup = () => setOpen(false);

/** What opening the Recycle Bin navigates to; the places feature shows it as a page. */
export const RECYCLE_BIN = "shell:RecycleBinFolder";

/** Shows a folder (or the Recycle Bin) in the current tab and closes the page. */
export function showFolder(path: string) {
  const current = activeTab();
  closeCleanup();
  if (current) navigate(current.id, path);
  else openTab(path);
}

/** The palette's actions for the page, listed with the app's own (`lib/paletteActions.ts`). */
export function cleanupActions() {
  return [
    {
      id: "cleanup-recommendations",
      title: "Cleanup recommendations",
      keywords: "clean up free space disk storage junk temporary cache recycle",
      run: () => openCleanup("recommendations"),
    },
    {
      id: "find-duplicates",
      title: "Find duplicate files",
      keywords: "duplicates copies same identical free space disk",
      run: () => openCleanup("duplicates"),
    },
  ];
}
