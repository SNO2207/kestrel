import { createEffect, onCleanup, onMount } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { HOME_PATH } from "../lib/home";
import { parity2Api } from "../lib/parity2Api";
import {
  addToZip,
  clearHistory,
  closeOtherTabs,
  closeTabsToRight,
  dropTab,
  duplicateTab,
  loadLibraries,
  parity2Options,
  receiveTab,
  setParity2Options,
  tabMenuEntries,
} from "../parity2";
import { refreshFrequent } from "../shortcuts";
import { activeId, navigate, refresh, tabs } from "../state";

/**
 * Explorer parity's second round, where the window as a whole is concerned: taking in a tab
 * another window lets go over this one's strip, following a clearing of the history in every
 * window, and reading the libraries when the navigation pane shows them. Its development hook,
 * `__kestrelParity2`, reaches what has no key of its own for the end-to-end tests.
 */
export function Parity2Layer(_props: { focusList: () => void }) {
  onMount(() => {
    const arrivals = parity2Api.onTabArrival((arrival) => void receiveTab(arrival));
    // Home and the frequent folders forget what was cleared, in every window.
    const cleared = parity2Api.onHistoryCleared(() => {
      void refreshFrequent();
      tabs.forEach((tab) => tab.path === HOME_PATH && refresh(tab.id));
    });
    onCleanup(() => {
      void arrivals.then((stop) => stop());
      void cleared.then((stop) => stop());
    });
  });

  // "Show libraries" puts them in the navigation pane, which needs them read.
  createEffect(() => {
    if (parity2Options().showLibraries) void loadLibraries();
  });

  return null;
}

// Development builds let the end-to-end tests reach what has no key or button of its own.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelParity2: {
      label: () => getCurrentWindow().label,
      options: () => parity2Options(),
      setOptions: setParity2Options,
      tabs: () => tabs.map((tab) => ({ id: tab.id, path: tab.path, pane: tab.pane, history: [...tab.history], cursor: tab.cursor })),
      activeTab: () => activeId(),
      tabMenu: (tabId: number) => tabMenuEntries(tabId).map((entry) => (entry === "separator" ? "-" : entry.label)),
      duplicate: duplicateTab,
      closeOthers: closeOtherTabs,
      closeRight: closeTabsToRight,
      /** Lets a tab go as a drag out of its strip would, over `at` (a window's page) or nowhere. */
      dropTab: (tabId: number, at: { label: string | null; x: number; y: number } | null) => dropTab(tabId, at),
      clearHistory,
      addToZip,
      /** Opens a path in the active tab at once, for timing a navigation from its start. */
      navigate: (path: string) => navigate(activeId(), path),
      closeWindow: () => getCurrentWindow().close(),
      api: parity2Api,
    },
  });
}
