import { api } from "./lib/api";
import { isSearch, notify, refresh, selectWhenListed, tabs } from "./state";

/** One undo or redo at a time; the backend refuses overlaps too, but quietly is kinder. */
let running = false;

const samePath = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/**
 * Undoes or redoes the latest file operation, says what happened, and selects whatever it put
 * back when that is in the folder the tab shows.
 */
async function reverse(tabId: number, redo: boolean) {
  if (running) return;
  running = true;
  try {
    const report = await (redo ? api.redo() : api.undo());
    notify(report.message, "info");
    const tab = tabs.find((candidate) => candidate.id === tabId);
    if (tab && report.folder && !isSearch(tab) && samePath(tab.path, report.folder)) {
      selectWhenListed(tabId, report.names);
    }
  } catch (error) {
    notify(error);
  } finally {
    running = false;
    refresh(tabId);
  }
}

export const undoLast = (tabId: number) => void reverse(tabId, false);
export const redoLast = (tabId: number) => void reverse(tabId, true);
