import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";

import {
  canZoomBack,
  closeDiskUsage,
  diskError,
  diskMeasure,
  diskOpen,
  diskOverview,
  diskRoot,
  diskStatus,
  diskTab,
  dismissDiskStatus,
  driveOf,
  openDiskUsage,
  recycleItems,
  revealItem,
  samePath,
  setDiskMeasure,
  setDiskTab,
  showFolder,
  toggleDiskUsage,
  undoInDisk,
  zoomBack,
  zoomTo,
} from "../disk";
import { api } from "../lib/api";
import { CATEGORIES } from "../lib/diskApi";
import { breadcrumbs, formatCount, formatSize } from "../lib/format";
import { activeId, activeTab, notify, places } from "../state";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { DiskIcon } from "./DiskIcon";
import { DiskOverviewPanel, type ListedItem } from "./DiskOverview";
import { DiskTreemap } from "./DiskTreemap";
import { Icon } from "./Icon";

/** A tab of the hub. Its panel is given the folder (or drive) being analysed as `root`. */
export interface DiskTabSpec {
  id: string;
  label: string;
  panel: Component<{ root: string }>;
}

/**
 * The hub's tabs, in order. Other features add theirs here: the cleanup and duplicates panels,
 * whose props are `{ root: string }`, go in as
 *
 *   { id: "cleanup", label: "Cleanup", panel: CleanupPanel },
 *   { id: "duplicates", label: "Duplicates", panel: DuplicatesPanel },
 */
const TABS: DiskTabSpec[] = [
  { id: "overview", label: "Overview", panel: OverviewTab },
  { id: "map", label: "Map", panel: MapTab },
];

// ---------- the menu and the confirmation, shared by every tab ----------

const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
const [confirming, setConfirming] = createSignal<ListedItem | null>(null);

/** Whether `path` is a drive's root, which is never offered for the Recycle Bin. */
const isDrive = (path: string) => /^[A-Za-z]:\\?$/.test(path);

function openMenuFor(item: ListedItem, x: number, y: number) {
  const recycle: MenuEntry[] = isDrive(item.path)
    ? []
    : ["separator", { label: "Move to the Recycle Bin…", danger: true, run: () => setConfirming(item) }];
  const entries: MenuEntry[] = item.folder
    ? [
        { label: "Look inside", run: () => zoomTo(item.path) },
        { label: "Open", run: () => showFolder(item.path) },
        { label: "Show in folder", disabled: isDrive(item.path), run: () => void revealItem(item.path) },
        ...recycle,
      ]
    : [
        { label: "Open", run: () => void api.openPath(item.path).catch(notify) },
        { label: "Show in folder", run: () => void revealItem(item.path) },
        ...recycle,
      ];
  setMenu({ x, y, entries });
}

function RecycleDialog(props: { item: ListedItem; onClose: () => void }) {
  let confirm: HTMLButtonElement | undefined;
  onMount(() => confirm?.focus());
  const go = () => {
    const item = props.item;
    props.onClose();
    void recycleItems([item.path], item.name);
  };
  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div
        class="dialog disk-confirm"
        role="dialog"
        aria-modal="true"
        aria-label="Move to the Recycle Bin"
        onKeyDown={(event) => {
          // The window's own shortcuts must not act on the list behind the dialog.
          event.stopPropagation();
          if (event.key === "Escape") {
            event.preventDefault();
            props.onClose();
          }
        }}
      >
        <h2 class="dialog-title">Move to the Recycle Bin?</h2>
        <div class="disk-confirm-body">
          <p>
            <strong>{props.item.name}</strong>
            {props.item.folder ? " and everything in it" : ""}, {formatSize(props.item.bytes)} on disk.
          </p>
          <p class="disk-confirm-path">{props.item.path}</p>
          <p class="disk-hint">Ctrl+Z puts it back, as does restoring it from the Recycle Bin.</p>
        </div>
        <div class="dialog-footer">
          <span class="disk-spacer" />
          <button class="dialog-button" onClick={props.onClose}>
            Cancel
          </button>
          <button ref={confirm} class="dialog-button primary disk-danger" onClick={go}>
            Move to the Recycle Bin
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------- the tabs ----------

/** The overview for the root being analysed, once it has arrived. */
const current = () => {
  const overview = diskOverview();
  return overview && samePath(overview.path, diskRoot()) ? overview : null;
};

function Waiting() {
  return (
    <Show when={diskError()} fallback={<div class="disk-message">Adding up…</div>}>
      <div class="disk-message error" role="alert">
        {diskError()}
      </div>
    </Show>
  );
}

function OverviewTab() {
  return (
    <div class="disk-scroll">
      <Show when={current()} fallback={<Waiting />}>
        {(overview) => <DiskOverviewPanel overview={overview()} onMenu={openMenuFor} />}
      </Show>
    </div>
  );
}

function MapTab() {
  const present = createMemo(() => {
    const overview = current();
    if (!overview) return CATEGORIES.map((category) => category.id);
    return overview.categories.filter((row) => row.bytes > 0).map((row) => row.category);
  });
  return (
    <div class="disk-map">
      <div class="disk-map-bar">
        <ul class="disk-map-legend" aria-label="Colours">
          <For each={CATEGORIES.filter((category) => present().includes(category.id))}>
            {(category) => (
              <li>
                <span class="disk-swatch" data-category={category.id} />
                {category.label}
              </li>
            )}
          </For>
          <li>
            <span class="disk-swatch" data-category="folder" />
            Folder too small to open
          </li>
          <li>
            <span class="disk-swatch" data-category="rest" />
            Smaller items
          </li>
        </ul>
        <div class="segmented small" role="group" aria-label="Size by">
          <button aria-pressed={diskMeasure() === "allocated"} onClick={() => setDiskMeasure("allocated")}>
            Size on disk
          </button>
          <button aria-pressed={diskMeasure() === "logical"} onClick={() => setDiskMeasure("logical")}>
            Size
          </button>
        </div>
      </div>
      <Show when={!diskError()} fallback={<Waiting />}>
        <DiskTreemap
          onMenu={(tile, x, y) => openMenuFor({ path: tile.path, name: tile.name, folder: tile.kind === "folder", bytes: tile.bytes }, x, y)}
        />
      </Show>
      <p class="disk-map-hint">Click a folder to look inside it, right-click for more. Back or Alt+Left steps out again.</p>
    </div>
  );
}

// ---------- the hub ----------

/**
 * The disk usage hub: what uses a drive or folder, worked out from the index. A breadcrumb and
 * Back move between folders, the drive list switches drives, and tabs show the overview and the
 * treemap (and other features' panels). It covers the file list while open; going anywhere else
 * closes it.
 */
export function DiskHub() {
  let root: HTMLElement | undefined;
  let crumbInput: HTMLInputElement | undefined;
  const [editing, setEditing] = createSignal(false);

  onMount(() => {
    // Ctrl+Shift+U opens the hub on the folder on screen, and closes it.
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey && event.shiftKey && !event.altKey && event.key.toLowerCase() === "u") {
        event.preventDefault();
        toggleDiskUsage();
      }
    };
    window.addEventListener("keydown", onKey);
    // After Solid's own delegated listener, so the hub's handlers have run first.
    document.addEventListener("keydown", guardKeys);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("keydown", guardKeys);
    });
  });

  // Leaving for a folder or another tab closes the hub.
  const where = createMemo(() => `${activeId()}|${activeTab()?.path ?? ""}|${activeTab()?.query ?? ""}`);
  createEffect(on(where, () => diskOpen() && closeDiskUsage(), { defer: true }));

  // The keyboard goes to the hub when it opens, so keys meant for it never reach the files.
  createEffect(
    on(diskOpen, (open) => {
      if (open) queueMicrotask(() => root?.focus());
      else setEditing(false);
    }),
  );
  createEffect(() => {
    if (editing()) queueMicrotask(() => (crumbInput?.focus(), crumbInput?.select()));
  });

  /**
   * Keys pressed in the hub are the hub's: the window's file shortcuts (Backspace, Delete,
   * Ctrl+Z on the folder underneath...) must not act on the files hidden beneath it. Tab
   * switching, Go to folder, the pins' keys and the hub's own shortcut get through.
   */
  const guardKeys = (event: KeyboardEvent) => {
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) return;
    const key = event.key.toLowerCase();
    const typing = event.target instanceof HTMLInputElement;
    const stop = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    if (key === "escape") {
      stop();
      if (editing()) setEditing(false);
      else closeDiskUsage();
      return;
    }
    if (!typing && ((event.altKey && key === "arrowleft") || key === "backspace")) return stop(), zoomBack();
    if (!typing && event.ctrlKey && !event.shiftKey && key === "z") return stop(), undoInDisk();
    if (event.ctrlKey && event.shiftKey && key === "u") return;
    if (event.ctrlKey && (key === "t" || key === "w" || key === "tab")) return;
    if (event.ctrlKey && !event.shiftKey && !event.altKey && (key === "g" || key === "k" || /^Digit[1-9]$/.test(event.code))) return;
    event.stopPropagation();
  };

  /** The folder analysed, as the index writes it once it has answered. */
  const shownPath = () => current()?.path ?? diskRoot();
  const crumbs = () =>
    breadcrumbs(shownPath()).map((crumb, i) => {
      if (i > 0) return crumb;
      const drive = places().drives.find((each) => samePath(each.path, crumb.path));
      return { ...crumb, label: drive ? `${drive.label || "Local Disk"} (${crumb.label})` : crumb.label };
    });
  const tab = () => TABS.find((spec) => spec.id === diskTab()) ?? TABS[0];
  const fixedDrives = () => places().drives.filter((drive) => drive.kind === "fixed");

  const goTyped = (value: string) => {
    setEditing(false);
    if (value.trim()) zoomTo(value.trim().replace(/\//g, "\\"));
  };

  return (
    <>
      <Show when={diskOpen()}>
        <section ref={root} class="disk-hub" aria-label="Disk usage" tabindex="-1">
          <header class="disk-head">
            <div class="disk-title-row">
              <DiskIcon name="usage" size={18} />
              <h1>Disk usage</h1>
              <button class="disk-icon-button" disabled={!canZoomBack()} title="Back (Alt+Left)" aria-label="Back" onClick={zoomBack}>
                <DiskIcon name="back" size={14} />
              </button>
              <div
                class="disk-crumbs"
                classList={{ editing: editing() }}
                title="Click to type a folder"
                onClick={(event) => {
                  if (event.target === event.currentTarget) setEditing(true);
                }}
              >
                <Show
                  when={editing()}
                  fallback={
                    <For each={crumbs()}>
                      {(crumb, i) => (
                        <>
                          <Show when={i() > 0}>
                            <Icon name="chevron" size={12} />
                          </Show>
                          <button
                            class="disk-crumb"
                            aria-current={i() === crumbs().length - 1 ? "location" : undefined}
                            onClick={() => zoomTo(crumb.path)}
                          >
                            {crumb.label}
                          </button>
                        </>
                      )}
                    </For>
                  }
                >
                  <input
                    ref={crumbInput}
                    class="disk-crumb-input"
                    aria-label="Folder to analyse"
                    spellcheck={false}
                    value={shownPath()}
                    onBlur={() => setEditing(false)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") goTyped(event.currentTarget.value);
                    }}
                  />
                </Show>
              </div>
              <label class="disk-drive-pick">
                <span>Drive</span>
                <select
                  aria-label="Drive"
                  value={driveOf(shownPath())}
                  onChange={(event) => openDiskUsage(event.currentTarget.value)}
                >
                  <For each={fixedDrives()}>
                    {(drive) => (
                      <option value={drive.path}>
                        {drive.label || "Local Disk"} ({drive.path.slice(0, 2)})
                      </option>
                    )}
                  </For>
                </select>
              </label>
              <button class="disk-icon-button" aria-label="Close disk usage" title="Close (Esc)" onClick={closeDiskUsage}>
                <DiskIcon name="close" size={14} />
              </button>
            </div>
            <div class="disk-tab-row">
              <div class="disk-tabs" role="tablist" aria-label="Views">
                <For each={TABS}>
                  {(spec) => (
                    <button
                      class="disk-tab"
                      role="tab"
                      data-tab={spec.id}
                      aria-selected={spec.id === tab().id}
                      onClick={() => setDiskTab(spec.id)}
                    >
                      {spec.label}
                    </button>
                  )}
                </For>
              </div>
              <Show when={current()}>
                {(overview) => (
                  <span class="disk-summary">
                    {formatSize(overview().totals.bytes)} on disk · {formatCount(overview().totals.files)} files ·{" "}
                    {formatCount(overview().totals.folders)} folders
                  </span>
                )}
              </Show>
            </div>
          </header>
          <div class="disk-body" role="tabpanel" aria-label={tab().label}>
            <Dynamic component={tab().panel} root={diskRoot()} />
          </div>
          <Show when={diskStatus()}>
            {(status) => (
              <div class="disk-status" role="status">
                <span>{status().text}</span>
                <Show when={status().undo}>
                  <button class="disk-status-action" onClick={undoInDisk}>
                    Undo
                  </button>
                </Show>
                <button class="disk-status-dismiss" aria-label="Dismiss" onClick={dismissDiskStatus}>
                  <DiskIcon name="close" size={12} />
                </button>
              </div>
            )}
          </Show>
        </section>
      </Show>
      <Show when={menu()}>
        {(open) => (
          <ContextMenu
            x={open().x}
            y={open().y}
            entries={open().entries}
            onClose={() => {
              setMenu(null);
              root?.focus();
            }}
          />
        )}
      </Show>
      <Show when={confirming()}>
        {(item) => (
          <RecycleDialog
            item={item()}
            onClose={() => {
              setConfirming(null);
              root?.focus();
            }}
          />
        )}
      </Show>
    </>
  );
}
