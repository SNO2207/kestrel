/**
 * The Recycle Bin: everything in the bin of every drive, with where each item was deleted
 * from, when, its size and type. Columns sort; items select as in the file list (click,
 * Ctrl+click, Shift+click, Ctrl+A, the arrow keys); Restore puts the selection back where it
 * was deleted from (an undoable operation, with the shell's dialog for name conflicts), Restore
 * all puts back everything after asking, Delete permanently asks, naming how many, and Empty
 * Recycle Bin asks too. The list is virtualised, so a bin of tens of thousands of items scrolls
 * as freely as a few.
 *
 * Every action names its items by identity (`recyclebin.rs`); only Empty means everything.
 * The page follows the bin: it asks the backend for a stamp every couple of seconds while it is
 * on screen, and reads the bin again when that changes, when the window comes back to the
 * front, and on Refresh.
 */
import { For, Show, batch, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { formatDate, formatSize } from "../lib/format";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { placesApi, type BinItem } from "../lib/placesApi";
import { showExtensions } from "../lib/views";
import { openLocation } from "../places";
import { notify } from "../state";
import { confirmAction } from "./ConfirmDialog";
import type { MenuEntry } from "./ContextMenu";
import { Icon } from "./Icon";
import { showMenu } from "./ShortcutDialogs";

const ROW = 28;
const OVERSCAN = 10;
/** How often the bin is checked for changes while the page is on screen. */
const WATCH_MS = 2000;
const SORT_KEY = "kestrel.recycle-bin-sort";

type BinSortKey = "name" | "folder" | "deleted" | "size" | "type";

interface BinSort {
  key: BinSortKey;
  desc: boolean;
}

const COLUMNS: { key: BinSortKey; label: string; numeric?: boolean }[] = [
  { key: "name", label: "Name" },
  { key: "folder", label: "Original location" },
  { key: "deleted", label: "Date deleted" },
  { key: "size", label: "Size", numeric: true },
  { key: "type", label: "Type" },
];

function storedSort(): BinSort {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(SORT_KEY) ?? "null");
    if (stored && typeof stored === "object") {
      const { key, desc } = stored as BinSort;
      if (COLUMNS.some((column) => column.key === key) && typeof desc === "boolean") return { key, desc };
    }
  } catch {
    // Unreadable or unavailable: the default stands.
  }
  return { key: "deleted", desc: true };
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function compare(a: BinItem, b: BinItem, key: BinSortKey): number {
  switch (key) {
    case "name":
      return collator.compare(a.name, b.name);
    case "folder":
      return collator.compare(a.folder, b.folder) || collator.compare(a.name, b.name);
    case "deleted":
      return a.deleted - b.deleted || collator.compare(a.name, b.name);
    case "size":
      return a.size - b.size || collator.compare(a.name, b.name);
    case "type":
      return collator.compare(a.type_name, b.type_name) || collator.compare(a.name, b.name);
  }
}

/** A bin item's icon, by its type alone: the item's own file is never read. */
const iconOf = (item: BinItem) =>
  item.dir ? "http://icon.localhost/folder?s=16" : `http://icon.localhost/ext/${encodeURIComponent(item.ext || "_")}?s=16`;

const displayName = (item: BinItem) => {
  if (showExtensions() || item.dir) return item.name;
  const dot = item.name.lastIndexOf(".");
  return dot > 0 ? item.name.slice(0, dot) : item.name;
};

const plural = (count: number, noun: string) => `${count.toLocaleString()} ${noun}${count === 1 ? "" : "s"}`;

export function RecycleBinPage(props: LocationPageProps) {
  let page!: HTMLDivElement;
  let viewport!: HTMLDivElement;
  const [items, setItems] = createSignal<BinItem[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [sort, setSortSignal] = createSignal<BinSort>(storedSort());
  const [selected, setSelected] = createSignal<ReadonlySet<string>>(new Set());
  const [focused, setFocused] = createSignal<string | null>(null);
  const [scrollTop, setScrollTop] = createSignal(0);
  const [height, setHeight] = createSignal(400);
  const [busy, setBusy] = createSignal(false);
  let anchor: string | null = null;
  let stamp = "";

  const sorted = createMemo(() => {
    const { key, desc } = sort();
    const list = [...items()];
    list.sort((a, b) => (desc ? -compare(a, b, key) : compare(a, b, key)));
    return list;
  });
  const positionOf = createMemo(() => new Map(sorted().map((item, index) => [item.id, index])));
  const totalSize = createMemo(() => items().reduce((sum, item) => sum + item.size, 0));
  const chosen = createMemo(() => sorted().filter((item) => selected().has(item.id)));

  const first = () => Math.max(0, Math.floor(scrollTop() / ROW) - OVERSCAN);
  const last = () => Math.min(sorted().length - 1, Math.ceil((scrollTop() + height()) / ROW) + OVERSCAN);
  const shown = createMemo(() => sorted().slice(first(), last() + 1));

  /** Reads the bin, keeping the selection on the items still there. */
  const read = async () => {
    try {
      const [next, nextStamp] = await Promise.all([placesApi.binItems(), placesApi.binStamp()]);
      stamp = nextStamp;
      const ids = new Set(next.map((item) => item.id));
      batch(() => {
        setItems(next);
        setLoaded(true);
        setFailure(null);
        setSelected((current) => new Set([...current].filter((id) => ids.has(id))));
        if (focused() && !ids.has(focused()!)) setFocused(null);
      });
    } catch (error) {
      setFailure(String(error));
      setLoaded(true);
    }
  };

  createEffect(on(() => props.revision, () => void read()));

  onMount(() => {
    props.focusRef(() => viewport.focus());
    if (props.active) queueMicrotask(() => takeFocusIfFree(viewport));
    const observer = new ResizeObserver(() => setHeight(viewport.clientHeight));
    observer.observe(viewport);
    // Checked only while on screen, one check at a time; coming back to the window checks at once.
    let checking = false;
    const check = async () => {
      if (checking || !props.active || document.visibilityState === "hidden") return;
      checking = true;
      const now = await placesApi.binStamp().catch(() => stamp);
      checking = false;
      if (now !== stamp) void read();
    };
    const timer = window.setInterval(() => void check(), WATCH_MS);
    const onFocus = () => void check();
    window.addEventListener("focus", onFocus);
    onCleanup(() => {
      observer.disconnect();
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
      setLocationStatus(props.tabId, null);
    });
  });

  createEffect(() => {
    const parts = [plural(items().length, "item")];
    if (selected().size > 0) parts.push(`${selected().size.toLocaleString()} selected`);
    if (items().length > 0) parts.push(formatSize(totalSize()));
    setLocationStatus(props.tabId, parts);
  });

  const setSort = (key: BinSortKey) => {
    const current = sort();
    const next = { key, desc: current.key === key ? !current.desc : key === "deleted" || key === "size" };
    setSortSignal(next);
    try {
      localStorage.setItem(SORT_KEY, JSON.stringify(next));
    } catch {
      // Remembered for this session only.
    }
    const focus = focused();
    if (focus) queueMicrotask(() => reveal(positionOf().get(focus) ?? 0));
  };

  const reveal = (index: number) => {
    const top = index * ROW;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + ROW > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = top + ROW - viewport.clientHeight;
  };

  /** Selects the item at a position: alone, or extending from the anchor, or toggled. */
  const selectAt = (index: number, mode: { extend?: boolean; toggle?: boolean } = {}) => {
    const list = sorted();
    const item = list[Math.max(0, Math.min(list.length - 1, index))];
    if (!item) return;
    const anchorAt = anchor !== null ? (positionOf().get(anchor) ?? -1) : -1;
    if (mode.extend && anchorAt >= 0) {
      const [from, to] = anchorAt <= index ? [anchorAt, index] : [index, anchorAt];
      const range = list.slice(from, to + 1).map((each) => each.id);
      setSelected(new Set(mode.toggle ? [...selected(), ...range] : range));
    } else if (mode.toggle) {
      const next = new Set(selected());
      if (next.has(item.id)) next.delete(item.id);
      else next.add(item.id);
      setSelected(next);
      anchor = item.id;
    } else {
      setSelected(new Set([item.id]));
      anchor = item.id;
    }
    setFocused(item.id);
    reveal(positionOf().get(item.id) ?? 0);
  };

  // ---------- actions ----------

  const run = async (work: () => Promise<void>) => {
    if (busy()) return;
    setBusy(true);
    try {
      await work();
    } catch (error) {
      notify(error);
    } finally {
      setBusy(false);
      await read();
      viewport.focus();
    }
  };

  const describe = (list: BinItem[]) => (list.length === 1 ? list[0].name : plural(list.length, "item"));

  const restore = (list: BinItem[]) =>
    run(async () => {
      if (list.length === 0) return;
      const result = await placesApi.binRestore(list.map((item) => item.id));
      const restored = result.paths.length;
      if (result.missing > 0) {
        notify(`${plural(result.missing, "item")} ${result.missing === 1 ? "was" : "were"} no longer in the Recycle Bin.`);
      } else if (restored > 0) {
        notify(`Restored ${restored === list.length ? describe(list) : plural(restored, "item")}`, "info");
      }
    });

  const restoreAll = async () => {
    const list = sorted();
    if (list.length === 0) return;
    const yes = await confirmAction({
      title: `Restore all ${plural(list.length, "item")}?`,
      message: "Each goes back to the folder it was deleted from.",
      action: "Restore all",
    });
    if (yes) await restore(list);
  };

  const erase = async (list: BinItem[]) => {
    if (list.length === 0) return;
    const yes = await confirmAction({
      title: `Delete ${plural(list.length, "item")} permanently?`,
      message: list.length === 1 ? `${list[0].name} will be gone for good. This can't be undone.` : "They will be gone for good. This can't be undone.",
      action: "Delete",
      danger: true,
    });
    if (!yes) return viewport.focus();
    await run(async () => {
      const result = await placesApi.binDelete(list.map((item) => item.id));
      if (result.left > 0) notify(`${plural(result.left, "item")} couldn't be deleted.`);
      else notify(`Deleted ${describe(list)} permanently`, "info");
    });
  };

  const empty = async () => {
    const count = items().length;
    if (count === 0) return;
    const yes = await confirmAction({
      title: "Empty the Recycle Bin?",
      message: `All ${plural(count, "item")} (${formatSize(totalSize())}) will be deleted permanently. This can't be undone.`,
      action: "Empty Recycle Bin",
      danger: true,
    });
    if (!yes) return viewport.focus();
    await run(async () => {
      await placesApi.binEmpty();
      setSelected(new Set<string>());
    });
  };

  const openMenu = (x: number, y: number) => {
    const list = chosen();
    if (list.length === 0) return;
    const entries: MenuEntry[] = [
      { label: "Restore", run: () => void restore(list) },
      "separator",
      { label: "Delete permanently", hint: "Del", danger: true, run: () => void erase(list) },
    ];
    if (list.length === 1) {
      entries.push("separator", { label: "Open original location", run: () => openLocation(list[0].folder) });
    }
    showMenu(x, y, entries);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const list = sorted();
    const at = focused() ? (positionOf().get(focused()!) ?? -1) : -1;
    const page = Math.max(1, Math.floor(viewport.clientHeight / ROW) - 1);
    const move = (index: number) => {
      event.preventDefault();
      selectAt(index, { extend: event.shiftKey, toggle: event.ctrlKey && event.shiftKey });
    };
    if (event.ctrlKey && event.key.toLowerCase() === "a") {
      event.preventDefault();
      setSelected(new Set(list.map((item) => item.id)));
      return;
    }
    switch (event.key) {
      case "ArrowDown":
        return move(at + 1);
      case "ArrowUp":
        return move(Math.max(0, at - 1));
      case "Home":
        return move(0);
      case "End":
        return move(list.length - 1);
      case "PageDown":
        return move(at + page);
      case "PageUp":
        return move(Math.max(0, at - page));
      case "Escape":
        return setSelected(new Set<string>());
      case "Delete":
        event.preventDefault();
        return void erase(chosen());
      case "ContextMenu": {
        event.preventDefault();
        const row = viewport.querySelector(`[data-index="${Math.max(0, at)}"]`)?.getBoundingClientRect();
        return openMenu((row?.left ?? 0) + 40, (row?.bottom ?? 0) - 4);
      }
    }
  };

  return (
    <div ref={page} class="bin-page" aria-label="Recycle Bin">
      <div class="bin-bar" role="toolbar" aria-label="Recycle Bin">
        <button class="bin-action" disabled={busy() || selected().size === 0} onClick={() => void restore(chosen())}>
          <Icon name="restore-item" />
          <span>Restore</span>
        </button>
        <button class="bin-action" disabled={busy() || items().length === 0} onClick={() => void restoreAll()}>
          <span>Restore all</span>
        </button>
        <button class="bin-action danger" disabled={busy() || selected().size === 0} onClick={() => void erase(chosen())}>
          <span>Delete permanently</span>
        </button>
        <span class="bin-bar-spacer" />
        <button class="bin-action danger" disabled={busy() || items().length === 0} onClick={() => void empty()}>
          <Icon name="bin" />
          <span>Empty Recycle Bin</span>
        </button>
      </div>
      <div class="bin-header" role="row">
        <For each={COLUMNS}>
          {(column) => (
            <button
              class="bin-column"
              classList={{ numeric: column.numeric, sorted: sort().key === column.key, [column.key]: true }}
              aria-sort={sort().key === column.key ? (sort().desc ? "descending" : "ascending") : "none"}
              onClick={() => setSort(column.key)}
            >
              <span>{column.label}</span>
              <Show when={sort().key === column.key}>
                <svg class="sort-arrow" classList={{ desc: sort().desc }} width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
                  <path d="M1 5.5 4 2.5l3 3" fill="none" stroke="currentColor" stroke-width="1.3" />
                </svg>
              </Show>
            </button>
          )}
        </For>
      </div>
      <div
        ref={viewport}
        class="bin-viewport"
        tabindex="0"
        role="grid"
        aria-rowcount={sorted().length}
        aria-multiselectable="true"
        onScroll={() => setScrollTop(viewport.scrollTop)}
        onKeyDown={onKeyDown}
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) setSelected(new Set<string>());
        }}
        onContextMenu={(event) => event.preventDefault()}
      >
        <div class="bin-spacer" style={{ height: `${sorted().length * ROW}px` }}>
          <For each={shown()}>
            {(item) => {
              const index = () => positionOf().get(item.id) ?? 0;
              return (
                <div
                  class="bin-row"
                  classList={{ selected: selected().has(item.id), focused: focused() === item.id }}
                  role="row"
                  aria-rowindex={index() + 1}
                  aria-selected={selected().has(item.id)}
                  data-index={index()}
                  data-id={item.id}
                  style={{ transform: `translateY(${index() * ROW}px)` }}
                  onMouseDown={(event) => {
                    if (event.button === 0) selectAt(index(), { extend: event.shiftKey, toggle: event.ctrlKey });
                    else if (event.button === 2 && !selected().has(item.id)) selectAt(index());
                  }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    openMenu(event.clientX, event.clientY);
                  }}
                >
                  <div class="bin-cell name" title={item.name}>
                    <img src={iconOf(item)} width="16" height="16" alt="" draggable={false} />
                    <span class="bin-label">{displayName(item)}</span>
                  </div>
                  <div class="bin-cell folder" title={item.folder}>
                    {item.folder}
                  </div>
                  <div class="bin-cell deleted">{formatDate(item.deleted)}</div>
                  <div class="bin-cell size">{formatSize(item.size)}</div>
                  <div class="bin-cell type" title={item.type_name}>
                    {item.type_name}
                  </div>
                </div>
              );
            }}
          </For>
        </div>
        <Show when={loaded() && items().length === 0}>
          <div class="list-empty">
            <Show when={failure()} fallback={<p>The Recycle Bin is empty.</p>}>
              <p>{failure()}</p>
            </Show>
          </div>
        </Show>
      </div>
    </div>
  );
}
