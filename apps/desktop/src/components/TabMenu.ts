/**
 * What every tab strip (the title bar's, and each pane's in Panes.tsx) adds to its tabs for
 * Explorer parity's second round: the tab's context menu (Duplicate tab, Open in new window,
 * Close tab, Close other tabs, Close tabs to the right) and dragging a tab out of the strip into
 * a window of its own or another window's strip (parity2.ts).
 */
import { draggedTab, startTabDrag, tabMenuEntries } from "../parity2";
import { showMenu } from "./ShortcutDialogs";

export { startTabDrag };

/** Shows a tab's context menu where it was asked for. */
export function openTabMenu(tabId: number, event: MouseEvent) {
  event.preventDefault();
  event.stopPropagation();
  showMenu(event.clientX, event.clientY, tabMenuEntries(tabId));
}

/** Classes for a tab being dragged: lifted, and marked once letting go would take it out. */
export function tabDragClasses(tabId: number): Record<string, boolean> {
  const dragged = draggedTab();
  return { "tab-lifted": dragged?.id === tabId, "tab-leaving": dragged?.id === tabId && dragged.out };
}
