import { onCleanup, onMount } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { recheckAccess } from "../lib/access";
import { folderOptions, setFolderOptions } from "../lib/folderOptions";
import { parityApi } from "../lib/parityApi";
import {
  invertSelection,
  newMenuKinds,
  openNewWindow,
  selectAll,
  selectNone,
  setOptionsOpen,
  toggleDetails,
  toggleFullScreen,
} from "../parity";
import { activeTab, goBack, goForward, refresh } from "../state";
import { FolderOptionsDialog } from "./FolderOptions";
import { NewShortcutDialog } from "./NewShortcutDialog";
import { focusTargets } from "./Toolbar";

/** What F6 moves through, in order: the address bar, the search box, the sidebar, the files. */
const REGIONS = [".address", ".searchbox", ".sidebar", ".content"] as const;

/** Which of the regions F6 moves through holds the keyboard now. */
function currentRegion(): number {
  const active = document.activeElement;
  const at = REGIONS.findIndex((selector) => active?.closest(selector));
  return at < 0 ? REGIONS.length - 1 : at;
}

/**
 * Explorer's shortcuts the rest of the app did not have: Ctrl+N (a new window), F11 (full
 * screen), Alt+Shift+P (the details pane), F4 (the address bar), F6 and Shift+F6 (move between
 * the address bar, the search box, the sidebar and the files), Ctrl+R (refresh), and the
 * mouse's back and forward buttons. Ctrl+Shift+E (show the folder in the navigation tree) is
 * passed on as a `kestrel:reveal-in-tree` event for the folder tree to act on. Also holds the
 * Folder Options and New shortcut dialogs.
 */
export function ParityLayer(props: { focusList: () => void }) {
  const cycle = (step: 1 | -1) => {
    const next = (currentRegion() + step + REGIONS.length) % REGIONS.length;
    const region = REGIONS[next];
    if (region === ".address") return focusTargets.address?.();
    if (region === ".searchbox") return focusTargets.search?.();
    if (region === ".sidebar") {
      const place = document.querySelector<HTMLElement>(".sidebar .place.current, .sidebar button");
      return place?.focus();
    }
    props.focusList();
  };

  onMount(() => {
    document.documentElement.dataset.window = getCurrentWindow().label;
    const onKeyDown = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      const plain = !event.ctrlKey && !event.altKey && !event.metaKey;
      const handled = () => event.preventDefault();
      if (event.ctrlKey && !event.shiftKey && !event.altKey && key === "n") return handled(), openNewWindow();
      if (plain && !event.shiftKey && key === "f11") return handled(), void toggleFullScreen();
      if (event.altKey && event.shiftKey && !event.ctrlKey && key === "p") return handled(), toggleDetails();
      if (plain && !event.shiftKey && key === "f4") return handled(), focusTargets.address?.();
      if (plain && key === "f6") return handled(), cycle(event.shiftKey ? -1 : 1);
      if (event.ctrlKey && !event.shiftKey && !event.altKey && key === "r") {
        const tab = activeTab();
        handled();
        recheckAccess();
        if (tab) refresh(tab.id);
        return;
      }
      if (event.ctrlKey && event.shiftKey && !event.altKey && key === "e") {
        handled();
        const tab = activeTab();
        window.dispatchEvent(new CustomEvent("kestrel:reveal-in-tree", { detail: { path: tab?.path ?? null } }));
      }
    };
    // The mouse's back and forward buttons, as in Explorer.
    const onMouseDown = (event: MouseEvent) => {
      if (event.button === 3 || event.button === 4) event.preventDefault();
    };
    const onMouseUp = (event: MouseEvent) => {
      const tab = activeTab();
      if (!tab || (event.button !== 3 && event.button !== 4)) return;
      event.preventDefault();
      if (event.button === 3) goBack(tab.id);
      else goForward(tab.id);
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("mouseup", onMouseUp);
    onCleanup(() => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("mouseup", onMouseUp);
    });
  });

  return (
    <>
      <FolderOptionsDialog onClose={() => props.focusList()} />
      <NewShortcutDialog onClose={() => props.focusList()} />
    </>
  );
}

// Development builds let the end-to-end tests reach what has no key or button of its own.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelParity: {
      label: () => getCurrentWindow().label,
      newMenu: () => newMenuKinds(),
      options: () => folderOptions(),
      setOptions: setFolderOptions,
      openOptions: () => setOptionsOpen(true),
      selectAll: () => (activeTab() ? selectAll(activeTab()!.id) : undefined),
      selectNone: () => (activeTab() ? selectNone(activeTab()!.id) : undefined),
      invert: () => (activeTab() ? invertSelection(activeTab()!.id) : undefined),
      api: parityApi,
    },
  });
}
