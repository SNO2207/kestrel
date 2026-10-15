import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { api, iconUrl, type Listing, type Marks, type Row, type SortKey, type SortSpec } from "../lib/api";
import { formatDate, formatSize } from "../lib/format";
import { Selection } from "../lib/selection";
import { ContextMenu, type MenuEntry } from "./ContextMenu";

const ROW_HEIGHT = 28;
const PAGE_SIZE = 128;
const OVERSCAN = 8;
/** Pages kept in memory per list; the rest are refetched on demand in about a millisecond. */
const MAX_PAGES = 48;
/**
 * Browsers cap an element's height at a few tens of millions of pixels, which a list of a
 * million rows exceeds. Beyond this height the scrollbar is scaled: it covers the whole list,
 * while the wheel and keyboard still move by exact rows.
 */
const MAX_SCROLL_HEIGHT = 8_000_000;
const TYPE_AHEAD_RESET_MS = 700;

/** Actions the list asks its owner to carry out on the current selection or folder. */
export type ListCommand =
  | "cut"
  | "copy"
  | "paste"
  | "copy-path"
  | "delete"
  | "erase"
  | "new-folder"
  | "properties"
  | "folder-properties"
  | "refresh";

interface Props {
  listing: Listing | null;
  revision: number;
  sort: SortSpec;
  query: string;
  error: string | null;
  /** Selection to show when the current rows first appear, after a refresh or re-sort. */
  restore: Marks | null;
  /** Names to select as soon as they appear, because an operation just created them. */
  pending: { names: string[]; rename: boolean } | null;
  onSort: (key: SortKey) => void;
  onOpen: (row: Row, inNewTab: boolean) => void;
  onStale: () => void;
  onSelectionChange: (selection: Selection, focus: number) => void;
  onPendingDone: () => void;
  onRenaming: (active: boolean) => void;
  onRename: (row: Row, name: string) => void;
  onCommand: (command: ListCommand) => void;
  focusRef: (focus: () => void) => void;
  /** Reports the focused row once its data has loaded, or null when no row has focus. */
  onFocusRow?: (row: Row | null) => void;
  /** Hands the owner a way to move the focus by some rows, for Quick Look. */
  stepRef?: (step: (delta: number) => void) => void;
}

interface Column {
  id: string;
  label: string;
  sort?: SortKey;
  numeric?: boolean;
}

const FOLDER_COLUMNS: Column[] = [
  { id: "name", label: "Name", sort: "name" },
  { id: "modified", label: "Date modified", sort: "modified" },
  { id: "type", label: "Type", sort: "type" },
  { id: "size", label: "Size", sort: "size", numeric: true },
];

const SEARCH_COLUMNS: Column[] = [
  { id: "name", label: "Name", sort: "name" },
  { id: "location", label: "Folder" },
  { id: "modified", label: "Date modified", sort: "modified" },
  { id: "size", label: "Size", sort: "size", numeric: true },
];

export function FileList(props: Props) {
  let viewport!: HTMLDivElement;
  const [height, setHeight] = createSignal(0);
  const [scrollTop, setScrollTop] = createSignal(0);
  /** Scroll offset in list pixels, which differs from `scrollTop` when the scrollbar is scaled. */
  const [offset, setOffset] = createSignal(0);
  const [loadedTick, setLoadedTick] = createSignal(0);
  const [selection, setSelection] = createSignal(Selection.empty);
  const [focused, setFocused] = createSignal(-1);
  /** Row whose name is being edited, or -1. */
  const [renaming, setRenaming] = createSignal(-1);
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  let anchor = -1;

  const loadedPages = new Map<number, Row[]>();
  const requested = new Set<number>();
  let cachedFor = "";
  let reportedStale = false;
  /** Identifies the folder or query on screen, to tell a refresh from a change of view. */
  let viewId = "";
  let sortId = "";

  const total = () => props.listing?.total ?? 0;
  const isSearch = () => props.listing?.path === "";
  const columns = () => (isSearch() ? SEARCH_COLUMNS : FOLDER_COLUMNS);
  const listHeight = () => total() * ROW_HEIGHT;
  const scrollHeight = () => Math.min(listHeight(), MAX_SCROLL_HEIGHT);
  const scale = () => {
    const scrollable = scrollHeight() - height();
    return listHeight() > scrollHeight() && scrollable > 0 ? (listHeight() - height()) / scrollable : 1;
  };
  const maxOffset = () => Math.max(0, listHeight() - height());
  /** Identifies the rows currently behind the list; responses for anything else are dropped. */
  const contentKey = () => `${props.listing?.set}:${props.revision}`;
  /**
   * The loaded pages for the current content. The cache empties itself the first time it is
   * read after the content changes, so no reader can see rows from a previous listing no
   * matter which effect happens to run first.
   */
  const pages = () => {
    const key = contentKey();
    if (key !== cachedFor) {
      cachedFor = key;
      loadedPages.clear();
      requested.clear();
      reportedStale = false;
    }
    return loadedPages;
  };

  /** Characters typed in quick succession to jump to a name, and where that jump began. */
  const typeAhead = { prefix: "", at: 0, from: 0, request: 0 };
  const resetTypeAhead = () => void (typeAhead.prefix = "");

  onMount(() => {
    const observer = new ResizeObserver(() => setHeight(viewport.clientHeight));
    observer.observe(viewport);
    onCleanup(() => observer.disconnect());
    props.focusRef(() => viewport.focus());
  });

  const scrollTo = (target: number) => {
    const next = Math.min(Math.max(0, target), maxOffset());
    setOffset(next);
    viewport.scrollTop = next / scale();
    setScrollTop(viewport.scrollTop);
  };

  const reveal = (row: number) => {
    const top = row * ROW_HEIGHT;
    if (top < offset()) scrollTo(top);
    else if (top + ROW_HEIGHT > offset() + height()) scrollTo(top + ROW_HEIGHT - height());
  };

  /** Selects the items an operation just created, once the listing that contains them is in. */
  const selectPending = async () => {
    const pending = props.pending;
    const listing = props.listing;
    if (!pending || !listing) return;
    const key = contentKey();
    const ranges = await api.locateNames(listing.set, pending.names);
    // Not there yet: a later refresh will carry them, and `pending` stays set until then.
    if (contentKey() !== key || ranges.length === 0) return;
    const first = ranges[0][0];
    setSelection(Selection.of(ranges));
    setFocused(first);
    anchor = first;
    reveal(first);
    props.onPendingDone();
    if (pending.rename && ranges.length === 1 && ranges[0][1] === first) setRenaming(first);
  };

  createEffect(
    on(contentKey, () => {
      resetTypeAhead();
      setRenaming(-1);
      const restore = props.restore;
      setSelection(restore ? Selection.of(restore.ranges) : Selection.empty);
      setFocused(restore?.focus ?? -1);
      anchor = restore?.focus ?? -1;

      // A refresh of the same folder or query keeps its place. A re-sort follows the focused
      // item, since its old position is meaningless. Anything else starts at the top.
      const view = isSearch() ? `query:${props.query}` : `folder:${props.listing?.path}`;
      const order = `${props.sort.key}:${props.sort.desc}`;
      const refreshed = view === viewId;
      // An open menu acts on the selection, which a refresh preserves, so it only needs to
      // close when the view itself changes.
      if (!refreshed) setMenu(null);
      scrollTo(refreshed && (order === sortId || focused() >= 0) ? offset() : 0);
      if (refreshed && order !== sortId && focused() >= 0) reveal(focused());
      viewId = view;
      sortId = order;
      void selectPending();
    }),
  );

  createEffect(() => props.onSelectionChange(selection(), focused()));
  createEffect(() => props.onRenaming(renaming() >= 0));

  const firstRow = createMemo(() => Math.max(0, Math.floor(offset() / ROW_HEIGHT) - OVERSCAN));
  const lastRow = createMemo(() =>
    Math.min(total() - 1, Math.ceil((offset() + height()) / ROW_HEIGHT) + OVERSCAN),
  );
  const visibleRows = createMemo(() => {
    const rows: number[] = [];
    for (let row = firstRow(); row <= lastRow(); row++) rows.push(row);
    return rows;
  });
  /** Where the first rendered row sits inside the scrolled content. */
  const translate = () =>
    scale() === 1 ? firstRow() * ROW_HEIGHT : scrollTop() + firstRow() * ROW_HEIGHT - offset();

  const rowAt = (row: number): Row | undefined => {
    loadedTick();
    return pages().get(Math.floor(row / PAGE_SIZE))?.[row % PAGE_SIZE];
  };

  createEffect(() => {
    const listing = props.listing;
    if (!listing || total() === 0) return;
    const key = contentKey();
    const loaded = pages();
    const from = Math.floor(firstRow() / PAGE_SIZE);
    const to = Math.floor(lastRow() / PAGE_SIZE);
    for (let page = from; page <= to; page++) {
      if (loaded.has(page) || requested.has(page)) continue;
      requested.add(page);
      api
        .rows(listing.set, page * PAGE_SIZE, PAGE_SIZE)
        .then((rows) => {
          if (contentKey() !== key) return;
          requested.delete(page);
          loaded.set(page, rows);
          // Drop the pages furthest from the one just loaded once the cache is full.
          if (loaded.size > MAX_PAGES) {
            const furthest = [...loaded.keys()].sort((a, b) => Math.abs(b - page) - Math.abs(a - page));
            furthest.slice(0, loaded.size - MAX_PAGES).forEach((old) => loaded.delete(old));
          }
          setLoadedTick((tick) => tick + 1);
        })
        .catch(() => {
          // The result set no longer exists, typically because the index was replaced.
          if (contentKey() === key && !reportedStale) {
            reportedStale = true;
            props.onStale();
          }
        });
    }
  });

  const onScroll = () => {
    const top = viewport.scrollTop;
    setScrollTop(top);
    const factor = scale();
    if (factor === 1) {
      setOffset(top);
      return;
    }
    // A scaled scrollbar cannot represent every row, so ignore scroll events that merely echo
    // an offset set by the wheel or keyboard, and follow real scrollbar drags.
    if (Math.abs(top * factor - offset()) > factor * 2) setOffset(Math.min(top * factor, maxOffset()));
  };

  const onWheel = (event: WheelEvent) => {
    if (scale() === 1 || event.ctrlKey) return;
    event.preventDefault();
    scrollTo(offset() + event.deltaY);
  };

  const select = (row: number, mode: { extend?: boolean; toggle?: boolean } = {}) => {
    if (row < 0 || row >= total()) return;
    if (mode.extend && anchor >= 0) {
      setSelection(mode.toggle ? selection().add(anchor, row) : Selection.range(anchor, row));
    } else if (mode.toggle) {
      setSelection(selection().toggle(row));
      anchor = row;
    } else {
      setSelection(Selection.range(row, row));
      anchor = row;
    }
    setFocused(row);
    reveal(row);
  };

  // The preview follows the focused row. While a row's page is still loading, the previous
  // report stands, so stepping through the list never flashes an empty preview.
  createEffect(() => {
    const index = focused();
    const row = index >= 0 ? rowAt(index) : undefined;
    if (index < 0) props.onFocusRow?.(null);
    else if (row) props.onFocusRow?.(row);
  });
  props.stepRef?.((delta) => {
    if (total() > 0) select(Math.min(Math.max(0, focused() + delta), total() - 1));
  });

  const open = (row: number, inNewTab = false) => {
    const data = rowAt(row);
    if (data) props.onOpen(data, inNewTab);
  };

  /** The single row an action such as Rename applies to, or -1 if the selection is not one row. */
  const soleRow = () => (selection().count === 1 ? selection().first : -1);

  const startRename = () => {
    const row = soleRow();
    if (row >= 0 && rowAt(row)) {
      reveal(row);
      setRenaming(row);
    }
  };

  const finishRename = (row: number, name: string | null) => {
    if (renaming() !== row) return;
    setRenaming(-1);
    viewport.focus();
    const data = rowAt(row);
    const trimmed = name?.trim();
    if (data && trimmed && trimmed !== data.name) props.onRename(data, trimmed);
  };

  const jumpToTyped = async (char: string) => {
    const listing = props.listing;
    if (!listing || total() === 0) return;
    const now = performance.now();
    const letter = char.toLowerCase();
    // Pressing the same letter again steps to the next name starting with it.
    const repeating = typeAhead.prefix === letter;
    if (repeating || typeAhead.prefix === "" || now - typeAhead.at > TYPE_AHEAD_RESET_MS) {
      typeAhead.prefix = letter;
      typeAhead.from = (Math.max(-1, focused()) + 1) % total();
    } else {
      typeAhead.prefix += letter;
    }
    typeAhead.at = now;

    // Every keystroke searches from where the burst began, so the outcome depends only on the
    // prefix typed so far and replies arriving out of order cannot move focus backwards.
    const request = ++typeAhead.request;
    const key = contentKey();
    const found = await api.findPrefix(listing.set, typeAhead.prefix, typeAhead.from);
    if (found !== null && request === typeAhead.request && contentKey() === key) select(found);
  };

  const openMenu = async (x: number, y: number, onItems: boolean) => {
    const count = selection().count;
    const sole = soleRow() >= 0 ? rowAt(soleRow()) : undefined;
    const browsing = !isSearch();
    const canPaste = browsing && (await api.canPaste().catch(() => false));
    const command = (name: ListCommand) => () => props.onCommand(name);

    const forItems: MenuEntry[] = [
      { label: "Open", hint: "Enter", disabled: !sole, run: () => open(soleRow()) },
      ...(sole?.dir ? [{ label: "Open in new tab", run: () => open(soleRow(), true) }] : []),
      "separator",
      { label: "Cut", hint: "Ctrl+X", run: command("cut") },
      { label: "Copy", hint: "Ctrl+C", run: command("copy") },
      { label: count > 1 ? "Copy as paths" : "Copy as path", hint: "Ctrl+Shift+C", run: command("copy-path") },
      "separator",
      { label: "Rename", hint: "F2", disabled: !sole, run: startRename },
      { label: "Delete", hint: "Del", danger: true, run: command("delete") },
      "separator",
      { label: "Properties", hint: "Alt+Enter", disabled: !sole, run: command("properties") },
    ];
    const forFolder: MenuEntry[] = [
      { label: "New folder", hint: "Ctrl+Shift+N", disabled: !browsing, run: command("new-folder") },
      { label: "Paste", hint: "Ctrl+V", disabled: !canPaste, run: command("paste") },
      "separator",
      { label: "Refresh", hint: "F5", run: command("refresh") },
      "separator",
      { label: "Properties", disabled: !browsing, run: command("folder-properties") },
    ];
    setMenu({ x, y, entries: onItems && count > 0 ? forItems : forFolder });
  };

  const closeMenu = () => {
    setMenu(null);
    viewport.focus();
  };

  /** Handles a shortcut for a file action. Returns whether the key was one. */
  const onShortcut = (event: KeyboardEvent): boolean => {
    const key = event.key.toLowerCase();
    const has = selection().count > 0;
    const run = (command: ListCommand) => (props.onCommand(command), true);
    if (event.ctrlKey && event.shiftKey && key === "n") return run("new-folder");
    if (event.ctrlKey && event.shiftKey && key === "c") return has && run("copy-path");
    if (event.ctrlKey && key === "c") return has && run("copy");
    if (event.ctrlKey && key === "x") return has && run("cut");
    if (event.ctrlKey && key === "v") return run("paste");
    if (event.altKey && key === "enter") return has && run("properties");
    if (key === "delete") return has && run(event.shiftKey ? "erase" : "delete");
    if (key === "f2") return startRename(), true;
    if (key === "contextmenu" || (event.shiftKey && key === "f10")) {
      const box = viewport.getBoundingClientRect();
      const row = Math.max(0, focused());
      void openMenu(box.left + 40, box.top + (row + 1) * ROW_HEIGHT - offset(), true);
      return true;
    }
    return false;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // Keys typed into the rename box belong to it.
    if (event.target instanceof HTMLInputElement) return;
    if (onShortcut(event)) {
      event.preventDefault();
      return;
    }
    const pageRows = Math.max(1, Math.floor(height() / ROW_HEIGHT) - 1);
    const current = focused();
    const mode = { extend: event.shiftKey };
    const move = (row: number) => {
      event.preventDefault();
      select(Math.min(Math.max(0, row), total() - 1), mode);
    };
    if (event.ctrlKey && event.key.toLowerCase() === "a") {
      event.preventDefault();
      if (total() > 0) setSelection(Selection.range(0, total() - 1));
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    const isCharacter = event.key.length === 1 && event.key !== " ";
    if (!isCharacter) resetTypeAhead();
    switch (event.key) {
      case "ArrowDown":
        return move(current + 1);
      case "ArrowUp":
        return move(current < 0 ? 0 : current - 1);
      case "PageDown":
        return move(current + pageRows);
      case "PageUp":
        return move(current - pageRows);
      case "Home":
        return move(0);
      case "End":
        return move(total() - 1);
      case "Enter":
        event.preventDefault();
        return open(current >= 0 ? current : selection().first);
      case "Escape":
        return setSelection(Selection.empty);
      default:
        if (isCharacter) {
          event.preventDefault();
          void jumpToTyped(event.key);
        }
    }
  };

  const sizeBar = (row: Row) => {
    const max = props.listing?.max_size ?? 0;
    if (max <= 0) return 0;
    // Strictly proportional: an item too small to register gets no bar rather than a tick.
    const share = (row.size / max) * 100;
    return share < 0.5 ? 0 : share;
  };

  const typeLabel = (row: Row) => (row.dir ? "Folder" : row.ext ? `${row.ext.toUpperCase()} file` : "File");

  const isBackground = (target: EventTarget | null) =>
    target === viewport || (target as HTMLElement).classList.contains("list-spacer");

  return (
    <div class="filelist" classList={{ results: isSearch() }}>
      <div class="list-header" role="row">
        <For each={columns()}>
          {(column) => (
            <Show
              when={column.sort}
              fallback={<div class="column-header" classList={{ [column.id]: true }}>{column.label}</div>}
            >
              {(key) => (
                <button
                  class="column-header"
                  classList={{ [column.id]: true, numeric: column.numeric, sorted: props.sort.key === key() }}
                  aria-sort={props.sort.key === key() ? (props.sort.desc ? "descending" : "ascending") : "none"}
                  onClick={() => props.onSort(key())}
                >
                  <span>{column.label}</span>
                  <Show when={props.sort.key === key()}>
                    <svg class="sort-arrow" classList={{ desc: props.sort.desc }} width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
                      <path d="M1 5.5 4 2.5l3 3" fill="none" stroke="currentColor" stroke-width="1.3" />
                    </svg>
                  </Show>
                </button>
              )}
            </Show>
          )}
        </For>
      </div>

      <Show when={props.error}>
        <div class="list-error" role="alert">{props.error}</div>
      </Show>

      <div
        ref={viewport}
        class="list-viewport"
        tabindex="0"
        role="grid"
        aria-rowcount={total()}
        aria-multiselectable="true"
        onScroll={onScroll}
        onWheel={onWheel}
        onKeyDown={onKeyDown}
        onMouseDown={(event) => {
          if (event.button === 0 && isBackground(event.target)) {
            setSelection(Selection.empty);
            setFocused(-1);
          }
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          if (!isBackground(event.target)) return;
          setSelection(Selection.empty);
          void openMenu(event.clientX, event.clientY, false);
        }}
      >
        <div class="list-spacer" style={{ height: `${scrollHeight()}px` }}>
          <div class="list-rows" style={{ transform: `translateY(${translate()}px)` }}>
            <For each={visibleRows()}>
              {(index) => {
                const row = createMemo(() => rowAt(index));
                return (
                  <div
                    class="row"
                    classList={{
                      selected: selection().has(index),
                      focused: focused() === index,
                      dimmed: row()?.hidden,
                    }}
                    role="row"
                    aria-rowindex={index + 1}
                    aria-selected={selection().has(index)}
                    onMouseDown={(event) => {
                      if (event.button !== 0 || event.target instanceof HTMLInputElement) return;
                      resetTypeAhead();
                      select(index, { extend: event.shiftKey, toggle: event.ctrlKey });
                    }}
                    onDblClick={(event) => {
                      if (!(event.target instanceof HTMLInputElement)) open(index);
                    }}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      event.stopPropagation();
                      // Right-clicking outside the selection acts on that one item instead.
                      if (!selection().has(index)) select(index);
                      void openMenu(event.clientX, event.clientY, true);
                    }}
                  >
                    <Show when={row()}>
                      {(data) => (
                        <>
                          <div class="cell name">
                            <img src={iconUrl(data())} width="16" height="16" alt="" draggable={false} />
                            <Show when={renaming() === index} fallback={<span class="label">{data().name}</span>}>
                              <input
                                class="rename"
                                aria-label="New name"
                                spellcheck={false}
                                value={data().name}
                                ref={(input) =>
                                  queueMicrotask(() => {
                                    input.focus();
                                    // Pre-select the name without its extension, as Explorer does.
                                    const dot = data().dir ? -1 : data().name.lastIndexOf(".");
                                    input.setSelectionRange(0, dot > 0 ? dot : data().name.length);
                                  })
                                }
                                onBlur={(event) => finishRename(index, event.currentTarget.value)}
                                onKeyDown={(event) => {
                                  if (event.key === "Enter") finishRename(index, event.currentTarget.value);
                                  if (event.key === "Escape") finishRename(index, null);
                                }}
                              />
                            </Show>
                          </div>
                          <Show when={isSearch()}>
                            <div class="cell location" title={data().location ?? ""}>{data().location}</div>
                          </Show>
                          <div class="cell modified">{formatDate(data().mtime)}</div>
                          <Show when={!isSearch()}>
                            <div class="cell type">{typeLabel(data())}</div>
                          </Show>
                          <div class="cell size">
                            <span class="weight-track">
                              <span class="weight" classList={{ folder: data().dir }} style={{ width: `${sizeBar(data())}%` }} />
                            </span>
                            <span class="amount">{data().dir && data().size === 0 ? "" : formatSize(data().size)}</span>
                          </div>
                        </>
                      )}
                    </Show>
                  </div>
                );
              }}
            </For>
          </div>
        </div>

        <Show when={props.listing && total() === 0 && !props.error}>
          <div class="list-empty">
            <Show when={isSearch()} fallback={<p>This folder is empty.</p>}>
              <p>Nothing matches “{props.query}”.</p>
              <p class="hint">
                Try fewer words, or filters such as <code>ext:pdf</code>, <code>size:&gt;100mb</code> or{" "}
                <code>dm:thisweek</code>.
              </p>
            </Show>
          </div>
        </Show>
      </div>

      <Show when={menu()}>
        {(open) => <ContextMenu x={open().x} y={open().y} entries={open().entries} onClose={closeMenu} />}
      </Show>
    </div>
  );
}
