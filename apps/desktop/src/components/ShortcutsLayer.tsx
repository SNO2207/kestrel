import { onCleanup, onMount } from "solid-js";

import { openPinKey, pinFolder, pinSelection, startShortcuts, MAX_TAGGED_AT_ONCE } from "../shortcuts";
import { activeTab, notify, selectedRows, tabs } from "../state";
import { GoToFolder, openGoto } from "./GoToFolder";
import { ShortcutDialogs, openTagPicker } from "./ShortcutDialogs";

/** The list commands the shortcuts add to an item's menu and the folder's menu. */
export type ShortcutCommand = "pin" | "pin-folder" | "tags";

/** Carries out a shortcut command from a tab's list. */
export function runShortcutCommand(tabId: number, command: ShortcutCommand) {
  switch (command) {
    case "pin":
      return void pinSelection(tabId);
    case "pin-folder":
      return void pinFolder(tabId);
    case "tags":
      return void tagSelection(tabId);
  }
}

async function tagSelection(tabId: number) {
  const count = tabs.find((tab) => tab.id === tabId)?.selected ?? 0;
  if (count > MAX_TAGGED_AT_ONCE) {
    return notify(`Tag at most ${MAX_TAGGED_AT_ONCE.toLocaleString()} items at a time; ${count.toLocaleString()} are selected.`);
  }
  openTagPicker((await selectedRows(tabId, MAX_TAGGED_AT_ONCE)).map((row) => row.path));
}

/**
 * The keys of the shortcuts: Ctrl+D pins the selection (or the folder), Ctrl+G opens Go to
 * folder, Ctrl+1 to Ctrl+9 open the pins with those numbers. Returns whether it was one.
 */
function onShortcutKey(event: KeyboardEvent): boolean {
  if (!event.ctrlKey || event.altKey || event.metaKey || event.shiftKey) return false;
  const typing = event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement;
  const key = event.key.toLowerCase();
  if (key === "g") {
    openGoto();
    return true;
  }
  if (key === "d") {
    const tab = activeTab();
    if (tab && !typing) void pinSelection(tab.id);
    return true;
  }
  // The code rather than the key, so the digit row works whatever the keyboard layout.
  const digit = /^Digit([1-9])$/.exec(event.code)?.[1];
  if (digit) {
    openPinKey(Number(digit));
    return true;
  }
  return false;
}

/** Starts the shortcuts, listens for their keys, and holds their menus and dialogs. */
export function ShortcutsLayer(props: { focusList: () => void }) {
  startShortcuts();
  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (onShortcutKey(event)) event.preventDefault();
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });
  return (
    <>
      <ShortcutDialogs onClose={() => props.focusList()} />
      <GoToFolder onClose={() => props.focusList()} />
    </>
  );
}
