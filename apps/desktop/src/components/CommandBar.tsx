import { Show, createEffect, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";

import { inArchive, isArchiveFile } from "../archives";
import { api, type SortKey } from "../lib/api";
import {
  detailsOpen,
  inFolder,
  invertSelection,
  newEntries,
  newMenuKinds,
  openInTerminal,
  openNewWindow,
  renameSelection,
  selectAll,
  selectNone,
  setOptionsOpen,
  toggleDetails,
  toggleFullScreen,
} from "../parity";
import { isZipListing, networkEntries, showsNetworkCommands } from "../parity2";
import { focusedItem } from "../previewState";
import { activeId, activeTab, setSortSpec, startsDescending } from "../state";
import { redoLast, undoLast } from "../undo";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import type { ListCommand } from "./FileList";
import { ParityIcon, type ParityIconName } from "./ParityIcon";

const SORT_KEYS: { key: SortKey; label: string }[] = [
  { key: "name", label: "Name" },
  { key: "modified", label: "Date modified" },
  { key: "type", label: "Type" },
  { key: "size", label: "Size" },
];

/**
 * Explorer's command bar, under the address bar: New, the clipboard and file commands, Sort,
 * the "..." menu (undo, selection, Open in Terminal, a new window, full screen, Folder
 * Options) and, at its right, the details pane's switch.
 */
export function CommandBar(props: { runCommand: (command: ListCommand) => void; focusList: () => void }) {
  const [menu, setMenu] = createSignal<{ name: string; x: number; y: number; entries: MenuEntry[] } | null>(null);
  const [canPaste, setCanPaste] = createSignal(false);
  /** The menu that the latest press on its own button closed, so the click must not reopen it. */
  let closedBy: string | null = null;

  const tab = activeTab;
  const selected = () => tab()?.selected ?? 0;
  const folder = () => inFolder(tab());
  /** Inside an archive, or with one archive selected, "Extract all…" has something to extract. */
  const extractable = () => {
    if (inArchive(tab()?.listing)) return true;
    const item = focusedItem();
    return selected() === 1 && item !== null && isArchiveFile(item);
  };
  /** Inside an archive nothing can be cut, renamed or deleted (archives.ts). */
  const changeable = () => selected() > 0 && !inArchive(tab()?.listing);

  const checkPaste = () => void api.canPaste().then(setCanPaste).catch(() => setCanPaste(false));
  onMount(() => {
    checkPaste();
    // Read now, so the New menu has its types by the time it is first opened.
    newMenuKinds();
    // The clipboard changes elsewhere too; looking again whenever the window comes back is
    // what Explorer's button does.
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey && ["c", "x"].includes(event.key.toLowerCase())) window.setTimeout(checkPaste, 300);
    };
    const onPress = (event: MouseEvent) => {
      const open = menu();
      const button = (event.target as Element | null)?.closest?.("[data-command-menu]");
      closedBy = open && button?.getAttribute("data-command-menu") === open.name ? open.name : null;
    };
    window.addEventListener("focus", checkPaste);
    window.addEventListener("keyup", onKey);
    window.addEventListener("mousedown", onPress, true);
    onCleanup(() => {
      window.removeEventListener("focus", checkPaste);
      window.removeEventListener("keyup", onKey);
      window.removeEventListener("mousedown", onPress, true);
    });
  });
  createEffect(on(activeId, checkPaste));

  const run = (command: ListCommand) => () => {
    props.runCommand(command);
    if (command === "cut" || command === "copy") window.setTimeout(checkPaste, 300);
  };

  const open = (name: string, entries: () => MenuEntry[]) => (event: MouseEvent) => {
    if (closedBy === name) {
      closedBy = null;
      return;
    }
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    setMenu({ name, x: box.left, y: box.bottom + 4, entries: entries() });
  };

  const sortEntries = (): MenuEntry[] => {
    const current = tab();
    if (!current) return [];
    const sort = current.sort;
    return [
      ...SORT_KEYS.map(({ key, label }) => ({
        label,
        checked: sort.key === key,
        run: () => setSortSpec(current.id, { key, desc: sort.key === key ? sort.desc : startsDescending(key) }),
      })),
      "separator",
      { label: "Ascending", checked: !sort.desc, run: () => setSortSpec(current.id, { key: sort.key, desc: false }) },
      { label: "Descending", checked: sort.desc, run: () => setSortSpec(current.id, { key: sort.key, desc: true }) },
    ];
  };

  const moreEntries = (): MenuEntry[] => {
    const current = tab();
    if (!current) return [];
    const browsing = folder();
    const listed = current.listing !== null;
    // This PC and the Network offer the network's commands first, as Explorer's "..." does (parity2.ts).
    const network: MenuEntry[] = showsNetworkCommands(current.path) ? [...networkEntries(), "separator"] : [];
    return [
      ...network,
      { label: "Undo", hint: "Ctrl+Z", run: () => undoLast(current.id) },
      { label: "Redo", hint: "Ctrl+Y", run: () => redoLast(current.id) },
      "separator",
      { label: selected() > 0 ? "Pin to sidebar" : "Pin this folder", hint: "Ctrl+D", disabled: !browsing && selected() === 0, run: () => props.runCommand(selected() > 0 ? "pin" : "pin-folder") },
      "separator",
      { label: "Select all", hint: "Ctrl+A", disabled: !listed, run: () => selectAll(current.id) },
      { label: "Select none", disabled: !listed || selected() === 0, run: () => selectNone(current.id) },
      { label: "Invert selection", disabled: !listed, run: () => invertSelection(current.id) },
      "separator",
      // Archives (archives.ts) and the disk usage hub (disk.ts), as their menus have them.
      { label: "Compress to ZIP file", disabled: !browsing || selected() === 0, run: () => props.runCommand("compress-zip") },
      { label: "Extract all…", disabled: !extractable(), run: () => props.runCommand("extract-all") },
      { label: "Show disk usage", hint: "Ctrl+Shift+U", disabled: !browsing && !inArchive(current.listing), run: () => props.runCommand("disk-usage") },
      "separator",
      { label: "Open in Terminal", disabled: !browsing, run: () => openInTerminal(current.path) },
      { label: "New window", hint: "Ctrl+N", run: () => openNewWindow() },
      { label: "Full screen", hint: "F11", run: () => void toggleFullScreen() },
      "separator",
      {
        label: "Properties",
        hint: "Alt+Enter",
        disabled: selected() === 0 && !browsing,
        run: () => props.runCommand(selected() > 0 ? "properties" : "folder-properties"),
      },
      { label: "Options", run: () => setOptionsOpen(true) },
    ];
  };

  const close = () => {
    setMenu(null);
    props.focusList();
  };

  const Button = (button: { icon: ParityIconName; label: string; title: string; disabled?: boolean; onClick: (event: MouseEvent) => void; text?: boolean; menu?: string; children?: JSX.Element }) => (
    <button
      class="command"
      classList={{ "with-text": button.text, open: button.menu !== undefined && menu()?.name === button.menu }}
      title={button.title}
      aria-label={button.label}
      disabled={button.disabled}
      data-command-menu={button.menu}
      aria-haspopup={button.menu ? "menu" : undefined}
      aria-expanded={button.menu ? menu()?.name === button.menu : undefined}
      onClick={(event) => button.onClick(event)}
    >
      <ParityIcon name={button.icon} />
      <Show when={button.text}>
        <span>{button.label}</span>
      </Show>
      {button.children}
    </button>
  );

  return (
    <div class="commandbar" role="toolbar" aria-label="Commands">
      <Button
        icon="new"
        label="New"
        title="Create a folder, a shortcut or a new file"
        text
        menu="new"
        disabled={!folder()}
        onClick={open("new", () => newEntries(activeId(), () => props.runCommand("new-folder")))}
      >
        <ParityIcon name="chevron" size={12} />
      </Button>
      <span class="command-gap" />
      <Button icon="cut" label="Cut" title="Cut (Ctrl+X)" disabled={!changeable()} onClick={run("cut")} />
      <Button icon="copy" label="Copy" title="Copy (Ctrl+C)" disabled={selected() === 0} onClick={run("copy")} />
      <Button icon="paste" label="Paste" title="Paste (Ctrl+V)" disabled={!(folder() || isZipListing(tab()?.listing)) || !canPaste()} onClick={run("paste")} />
      <Button icon="rename" label="Rename" title="Rename (F2)" disabled={!changeable()} onClick={() => renameSelection(activeId())} />
      <Button icon="delete" label="Delete" title="Delete (Del); Shift+Del deletes permanently" disabled={!changeable()} onClick={run("delete")} />
      <span class="command-gap" />
      <Button
        icon="sort"
        label="Sort"
        title="Sort the items"
        text
        menu="sort"
        disabled={!tab()?.listing}
        onClick={open("sort", sortEntries)}
      >
        <ParityIcon name="chevron" size={12} />
      </Button>
      <Button icon="more" label="More" title="See more" menu="more" disabled={!tab()} onClick={open("more", moreEntries)} />
      <span class="command-spacer" />
      <Show when={tab()}>
        <button
          class="command with-text details-toggle"
          classList={{ on: detailsOpen() }}
          title="Details pane (Alt+Shift+P)"
          aria-label="Details"
          aria-pressed={detailsOpen()}
          onClick={toggleDetails}
        >
          <ParityIcon name="details" />
          <span>Details</span>
        </button>
      </Show>
      <Show when={menu()}>{(at) => <ContextMenu x={at().x} y={at().y} entries={at().entries} onClose={close} />}</Show>
    </div>
  );
}
