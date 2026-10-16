/**
 * Downloads triage (`src-tauri/src/triage.rs`, `TriagePage.tsx`): a page, at the special
 * location `kestrel:new-downloads`, of what arrived in the Downloads folder since the owner last
 * looked, grouped by kind, each with the actions that suit it. The sidebar's entry carries a
 * badge with how many downloads are new; the backend watches the folder and says when that
 * changes (`triage-changed`).
 */
import { createSignal } from "solid-js";

import { searchApi } from "./lib/searchApi";
import { activeTab, navigate, openTab } from "./state";

/** The page's address; never a file system path. */
export const TRIAGE = "kestrel:new-downloads";
export const TRIAGE_ALIASES = ["New downloads", "Downloads triage"];

const [newDownloads, setNewDownloads] = createSignal(0);
/** How many downloads arrived since the owner last looked. */
export { newDownloads };

/** Opens the page in the active tab, or a new one. */
export function openTriage(inNewTab = false) {
  const tab = activeTab();
  if (inNewTab || !tab) openTab(TRIAGE);
  else navigate(tab.id, TRIAGE);
}

let started = false;

/** Follows the count of new downloads. Call once, in a window. */
export function startTriage() {
  if (started) return;
  started = true;
  void searchApi.triageCount().then(setNewDownloads).catch(() => {});
  void searchApi.onTriageChanged(setNewDownloads);
  // Development builds let the end-to-end tests read the count.
  if (import.meta.env.DEV) Object.assign(window, { __kestrelTriage: { count: () => newDownloads() } });
}
