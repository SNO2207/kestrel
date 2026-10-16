import { For, Show, batch, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { api, iconUrl, type Listing, type Marks, type Row, type RowRange, type SortKey, type SortSpec } from "../lib/api";
import { LOCKED_MESSAGE, checkFolders, foldersOf, isLocked } from "../lib/access";
import { checkGitBadges, gitBadge } from "../lib/gitBadges";
import { dragOut, followPress, isDropRow } from "../dragdrop";
import { formatDate, formatSize } from "../lib/format";
import { Selection } from "../lib/selection";
import { GRID_PADDING, geometry, keyTarget, sameGeometry } from "../lib/viewLayout";
import { ListLayout, groupedKeyTarget } from "../lib/listLayout";
import { layoutApi, type GroupHeader as GroupInfo } from "../lib/layoutApi";
import {
  attributeLetters,
  columnSet,
  columnTemplate,
  columnsWidth,
  folderOf,
  type ColumnId,
  type ColumnKind,
} from "../lib/columns";
import { VIEW_MODES, displayName, listingLocation, setViewMode, showExtensions, viewModeAt } from "../lib/views";
import { ColumnHeaders } from "./ColumnHeaders";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { FilterBar } from "./FilterBar";
import { GitGlyph } from "./GitGlyph";
import { GroupHeader } from "./GroupHeader";
import { LockGlyph } from "./LockGlyph";
import type { ShortcutCommand } from "./ShortcutsLayer";
import { TagDots } from "./TagDots";
import { ViewItem } from "./ViewItem";
import { folderOptions } from "../lib/folderOptions";
import { inverted, type SelectControl } from "../lib/selectionOps";
import { activeId } from "../state";
import { newEntries, openInTerminal, openNewWindow, revealInFolder } from "../parity";
import { SyncGlyph } from "./SyncGlyph";

const PAGE_SIZE = 128;
/** Pixels rendered beyond each edge of the view, so short scrolls never show a gap. */
const OVERSCAN_PX = 224;
/** Pages kept in memory per list; the rest are refetched on demand in about a millisecond. */
const MAX_PAGES = 48;
/**
 * Browsers cap an element's height at a few tens of millions of pixels, which a list of a
 * million rows exceeds. Beyond this height the scrollbar is scaled: it covers the whole list,
 * while the wheel and keyboard still move by exact rows.
 */
const MAX_SCROLL_HEIGHT = 8_000_000;
const TYPE_AHEAD_RESET_MS = 700;
/** How long scrolling must pause before thumbnails are asked for. */
const THUMB_DELAY_MS = 150;
/** Wheel travel, with Ctrl held, that steps to the next view size. */
const ZOOM_STEP = 100;
/** A group's header in the views that scroll down, and the space before each group after the first. */
const GROUP_HEADER = 36;
const GROUP_GAP = 4;
/** The band above the list view's columns that holds their groups' headers. */
const GROUP_BAND = 28;
/** Where the list view's first item sits below the top of the view. */
const LIST_TOP = 4;
/** Most group names asked for at once, as many as the backend answers in one call. */
const HEADER_BATCH = 4096;
/** How far the pointer moves before a press on empty space draws a selection rectangle. */
const MARQUEE_THRESHOLD = 4;
/** The band along the view's edges where a selection rectangle scrolls the list, and its fastest step. */
const MARQUEE_EDGE = 28;
const MARQUEE_STEP = 24;

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
  | "refresh"
  | "undo"
  | "redo"
  | "batch-rename"
  | "open-with"
  | ShortcutCommand;

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
  /** Where each group starts, while the rows are grouped (see `setGrouping` in state.ts). */
  groups?: number[] | null;
  /** The filter box's text, or null while it is closed. */
  filter?: string | null;
  /** How many rows the listing had before the filter narrowed it. */
  unfilteredTotal?: number | null;
  onFilter?: (text: string | null) => void;
  /** More entries for the item and folder menus, such as "Copy to other pane" and "Group by". */
  menuExtras?: { items?: () => MenuEntry[]; folder?: () => MenuEntry[] };
  onSort: (key: SortKey) => void;
  onOpen: (row: Row, inNewTab: boolean) => void;
  onStale: () => void;
  onSelectionChange: (selection: Selection, focus: number) => void;
  onPendingDone: () => void;
  onRenaming: (active: boolean) => void;
  onRename: (row: Row, name: string) => void;
  onCommand: (command: ListCommand) => void;
  /**
   * Shows the Windows context menu at a point in the window: for the selection, or for the
   * folder when `onItems` is false. `extended` adds the commands Shift reveals in Explorer.
   */
  onShellMenu: (x: number, y: number, onItems: boolean, extended: boolean) => void;
  focusRef: (focus: () => void) => void;
  /** Reports the focused row once its data has loaded, or null when no row has focus. */
  onFocusRow?: (row: Row | null) => void;
  /** Hands the owner a way to move the focus by some rows, for Quick Look. */
  stepRef?: (step: (delta: number) => void) => void;
  /** Hands the owner Select all, Select none, Invert selection and Rename (`lib/selectionOps.ts`). */
  selectRef?: (control: SelectControl) => void;
}

const sameRows = (a: readonly number[], b: readonly number[]) => a.length === b.length && a.every((value, i) => value === b[i]);

export function FileList(props: Props) {
  let viewport!: HTMLDivElement;
  const [height, setHeight] = createSignal(0);
  const [width, setWidth] = createSignal(0);
  /** The viewport's scroll position along the view's scroll axis: down, or across for the list view. */
  const [scrollPos, setScrollPos] = createSignal(0);
  /** Scroll offset in list pixels, which differs from `scrollPos` when the scrollbar is scaled. */
  const [offset, setOffset] = createSignal(0);
  /** How far the details view's columns are scrolled sideways, which the column headers follow. */
  const [shift, setShift] = createSignal(0);
  const [loadedTick, setLoadedTick] = createSignal(0);
  const [selection, setSelection] = createSignal(Selection.empty);
  const [focused, setFocused] = createSignal(-1);
  /** Row whose name is being edited, or -1. */
  const [renaming, setRenaming] = createSignal(-1);
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  /** The view is being scrolled, so thumbnails wait until it pauses. */
  const [scrolling, setScrolling] = createSignal(false);
  /** The selection rectangle being drawn, in the viewport's own pixels. */
  const [band, setBand] = createSignal<{ left: number; top: number; width: number; height: number } | null>(null);
  /** Names of the groups collapsed to their headers. */
  const [collapsed, setCollapsed] = createSignal<ReadonlySet<string>>(new Set());
  let anchor = -1;
  let scrollPause: number | undefined;
  /** Ctrl+wheel travel not yet turned into a change of view size. */
  let zoomTravel = 0;

  const loadedPages = new Map<number, Row[]>();
  const requested = new Set<number>();
  let cachedFor = "";
  let reportedStale = false;
  /** Identifies the folder or query on screen, to tell a refresh from a change of view. */
  let viewId = "";
  let sortId = "";

  const total = () => props.listing?.total ?? 0;
  const isSearch = () => props.listing?.path === "";
  const columnKind = (): ColumnKind => (isSearch() ? "search" : "folder");
  const columnOrder = () => columnSet(columnKind()).order;
  /** The view mode remembered for the folder, or the search, on screen. */
  const mode = () => viewModeAt(listingLocation(props.listing));
  const grouped = () => (props.groups?.length ?? 0) > 0;
  const layout = createMemo(
    () => geometry(mode(), width(), grouped() && mode() === "list" ? height() - GROUP_BAND : height()),
    undefined,
    { equals: sameGeometry },
  );
  const vertical = () => layout().axis === "y";
  /** The viewport's extent along the scroll axis. */
  const extent = () => (vertical() ? height() : width());

  // ---------- groups ----------

  /** Group names and sizes fetched for the rows on screen, by group. */
  const headers = new Map<number, GroupInfo>();
  const headersAsked = new Set<number>();
  let headersFor = "";
  const [headerTick, setHeaderTick] = createSignal(0);
  const headerOf = (index: number): GroupInfo | undefined => {
    headerTick();
    groupCache();
    return headers.get(index);
  };
  /** Empties the group names when the rows change, as `pages` does. */
  const groupCache = () => {
    const key = contentKey();
    if (key !== headersFor) {
      headersFor = key;
      headers.clear();
      headersAsked.clear();
    }
    return headers;
  };
  const isCollapsed = (index: number) => {
    const label = headerOf(index)?.label;
    return label !== undefined && collapsed().has(label);
  };

  const list = createMemo(
    () =>
      new ListLayout(layout(), total(), grouped() ? props.groups! : null, isCollapsed, {
        header: vertical() ? GROUP_HEADER : 0,
        gap: vertical() ? GROUP_GAP : 3 * GROUP_GAP,
      }),
  );
  const listExtent = () => list().extent;
  const scrollExtent = () => Math.min(listExtent(), MAX_SCROLL_HEIGHT);
  const scale = () => {
    const scrollable = scrollExtent() - extent();
    return listExtent() > scrollExtent() && scrollable > 0 ? (listExtent() - extent()) / scrollable : 1;
  };
  const maxOffset = () => Math.max(0, listExtent() - extent());
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
    const observer = new ResizeObserver(() =>
      batch(() => {
        setHeight(viewport.clientHeight);
        setWidth(viewport.clientWidth);
      }),
    );
    observer.observe(viewport);
    onCleanup(() => {
      observer.disconnect();
      clearTimeout(scrollPause);
    });
    props.focusRef(() => viewport.focus());
  });

  /** The viewport's own scroll position along the scroll axis. */
  const readScroll = () => (vertical() ? viewport.scrollTop : viewport.scrollLeft);
  const writeScroll = (position: number) => {
    if (vertical()) viewport.scrollTop = position;
    else viewport.scrollLeft = position;
  };

  /** The scroll position last set by `scrollTo`, as the viewport reported it back. */
  let ownScroll = -1;

  const scrollTo = (target: number) => {
    const next = Math.min(Math.max(0, target), maxOffset());
    setOffset(next);
    writeScroll(next / scale());
    ownScroll = readScroll();
    setScrollPos(ownScroll);
  };

  /** Scrolls just far enough to show the whole line holding `row`, and its group's header atop the first line. */
  const reveal = (row: number) => {
    const { lineSize } = layout();
    let start = list().lineStart(row);
    const slot = list().slotOfRow(row);
    // The first line of a group comes into view with its header, as in Explorer.
    if (grouped() && vertical() && start === slot.items) start = slot.start;
    const end = list().lineStart(row) + (slot.collapsed ? GROUP_HEADER : lineSize);
    if (start < offset()) scrollTo(start);
    else if (end > offset() + extent()) scrollTo(end - extent());
  };

  /** The first row in view, to keep in view when the layout changes. */
  const firstInView = (layoutBefore: ListLayout) => {
    const slot = layoutBefore.slotAt(offset());
    const line = Math.max(0, Math.floor((offset() - slot.items) / layoutBefore.g.lineSize));
    return Math.min(slot.first + line * layoutBefore.g.perLine, Math.max(0, total() - 1));
  };

  // A new view mode, or a resize that changes how many items fit in a line, keeps the same
  // items in view: the focused one if there is one, otherwise the first one showing.
  createEffect(
    on(layout, (_next, previous) => {
      if (!previous) return;
      const before = new ListLayout(previous, total(), grouped() ? props.groups! : null, isCollapsed, {
        header: previous.axis === "y" ? GROUP_HEADER : 0,
        gap: previous.axis === "y" ? GROUP_GAP : 3 * GROUP_GAP,
      });
      const atTop = offset() <= 0;
      const first = firstInView(before);
      // A position along the other axis belongs to the previous view.
      viewport.scrollTop = 0;
      viewport.scrollLeft = 0;
      setShift(0);
      // A group's first line comes with its header.
      const slot = list().slotOfRow(first);
      const line = list().lineStart(first);
      scrollTo(atTop || total() === 0 ? 0 : line === slot.items ? slot.start : line);
      if (focused() >= 0) reveal(focused());
    }),
  );

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
      if (!refreshed) {
        setMenu(null);
        setCollapsed(new Set<string>());
      }
      scrollTo(refreshed && (order === sortId || focused() >= 0) ? offset() : 0);
      if (refreshed && order !== sortId && focused() >= 0) reveal(focused());
      viewId = view;
      sortId = order;
      void selectPending();
    }),
  );

  createEffect(() => props.onSelectionChange(selection(), focused()));
  createEffect(() => props.onRenaming(renaming() >= 0));

  /** The groups that reach into the view, with the lines of each in it. */
  const blocks = createMemo(() => list().blocks(offset() - OVERSCAN_PX, offset() + extent() + OVERSCAN_PX));
  const shownGroups = createMemo(() => blocks().map((block) => block.slot.index), [], { equals: sameRows });
  const rowsOfGroup = (index: number) => {
    const block = blocks().find((each) => each.slot.index === index);
    return block ? list().rowsOf(block.slot, block.firstLine, block.lastLine) : [];
  };
  const visibleRows = createMemo(() => blocks().flatMap((block) => list().rowsOf(block.slot, block.firstLine, block.lastLine)), [], {
    equals: sameRows,
  });
  /** Where a position in the list is drawn inside the scrolled content. */
  const place = (position: number) => (scale() === 1 ? position : scrollPos() + position - offset());
  /** Places a group's rendered lines: rows for details and content, a grid of cells, or the list view's columns. */
  const blockStyle = (index: number) => {
    const block = blocks().find((each) => each.slot.index === index);
    const { perLine, lineSize, itemSize } = layout();
    const start = block ? block.slot.items + block.firstLine * lineSize : 0;
    if (mode() === "details" || mode() === "content") return { transform: `translateY(${place(start)}px)` };
    if (vertical()) {
      // Cells share out the width left over, so a row fills the view.
      return {
        transform: `translateY(${place(start)}px)`,
        "grid-template-columns": `repeat(${perLine}, minmax(0, 1fr))`,
        "grid-auto-rows": `${lineSize}px`,
      };
    }
    return {
      transform: `translateX(${place(start)}px)`,
      top: `${LIST_TOP + (grouped() ? GROUP_BAND : 0)}px`,
      "grid-template-rows": `repeat(${perLine}, ${itemSize}px)`,
      "grid-auto-columns": `${lineSize}px`,
    };
  };
  /** Places a group's header: a line of its own, or the band above the list view's columns. */
  const headerStyle = (index: number) => {
    const slot = list().slots[index];
    if (!slot) return {};
    if (vertical()) return { transform: `translateY(${place(slot.start)}px)`, height: `${GROUP_HEADER}px` };
    return { transform: `translateX(${place(slot.start)}px)`, width: `${slot.end - slot.start}px`, height: `${GROUP_BAND}px` };
  };

  const rowAt = (row: number): Row | undefined => {
    loadedTick();
    return pages().get(Math.floor(row / PAGE_SIZE))?.[row % PAGE_SIZE];
  };

  // Group names are asked for the groups in view only, so a search of a million rows grouped
  // by type sends a few dozen names, not thousands. With any group collapsed, every name is
  // needed to place the groups, and all are asked for.
  createEffect(() => {
    const listing = props.listing;
    if (!listing || !grouped()) return;
    const key = contentKey();
    const cache = groupCache();
    const count = props.groups!.length;
    const wanted = collapsed().size > 0 ? Array.from({ length: Math.min(count, HEADER_BATCH) }, (_, i) => i) : shownGroups();
    const missing = wanted.filter((index) => !cache.has(index) && !headersAsked.has(index));
    if (missing.length === 0) return;
    const from = Math.min(...missing);
    const to = Math.max(...missing);
    for (let index = from; index <= to; index++) headersAsked.add(index);
    void (async () => {
      for (let start = from; start <= to; start += HEADER_BATCH) {
        const found = await layoutApi.groupHeaders(listing.set, start, Math.min(HEADER_BATCH, to - start + 1)).catch(() => []);
        if (contentKey() !== key) return;
        found.forEach((header, i) => cache.set(start + i, header));
      }
      setHeaderTick((tick) => tick + 1);
    })();
  });

  // Whether the rows on screen sit in folders this account can't open is asked lazily, for
  // those rows only, so even a search with millions of results stays instant.
  createEffect(() => {
    const folders: string[] = [];
    for (const index of visibleRows()) {
      const data = rowAt(index);
      if (data) folders.push(...foldersOf(data));
    }
    checkFolders(folders);
  });

  // Git badges, in a folder inside a repository, are likewise asked for the rows on screen only.
  createEffect(() => {
    const listing = props.listing;
    if (!listing || isSearch()) return;
    const shown: Row[] = [];
    for (const index of visibleRows()) {
      const data = rowAt(index);
      if (data) shown.push(data);
    }
    if (shown.length > 0) checkGitBadges(listing.path, listing.set, shown);
  });
  /** The row's git badge, in folder views only. */
  const badgeOf = (row: Row | undefined) => (isSearch() ? undefined : gitBadge(row));

  createEffect(() => {
    const listing = props.listing;
    if (!listing || total() === 0) return;
    const key = contentKey();
    const loaded = pages();
    const pagesWanted = new Set(visibleRows().map((row) => Math.floor(row / PAGE_SIZE)));
    for (const page of pagesWanted) {
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
    if (mode() === "details") setShift(viewport.scrollLeft);
    const top = readScroll();
    setScrollPos(top);
    holdThumbnails();
    const factor = scale();
    if (factor === 1) {
      setOffset(top);
      return;
    }
    // A scaled scrollbar cannot represent every row, so ignore scroll events that merely echo
    // an offset set by the wheel or keyboard, and follow real scrollbar drags. An echo is
    // recognised by the position itself too, since the browser may have rounded or clamped it.
    const echo = Math.abs(top - ownScroll) < 1;
    if (!echo && Math.abs(top * factor - offset()) > factor * 2) setOffset(Math.min(top * factor, maxOffset()));
  };

  /** Thumbnails wait until scrolling pauses, so a fling does not ask for thousands. */
  const holdThumbnails = () => {
    setScrolling(true);
    clearTimeout(scrollPause);
    scrollPause = window.setTimeout(() => setScrolling(false), THUMB_DELAY_MS);
  };

  const onWheel = (event: WheelEvent) => {
    if (event.ctrlKey) return zoom(event);
    if (vertical()) {
      if (scale() === 1) return;
      event.preventDefault();
      scrollTo(offset() + event.deltaY);
      return;
    }
    // The list view scrolls sideways, and the wheel moves it as it would move a page down.
    event.preventDefault();
    scrollTo(offset() + (Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY));
  };

  /** Ctrl+wheel steps through the view sizes, as in Explorer: up for larger, down for smaller. */
  const zoom = (event: WheelEvent) => {
    event.preventDefault();
    const location = listingLocation(props.listing);
    zoomTravel += event.deltaY;
    if (location === null || Math.abs(zoomTravel) < ZOOM_STEP) return;
    const at = VIEW_MODES.findIndex((info) => info.mode === mode());
    const next = Math.min(VIEW_MODES.length - 1, Math.max(0, at + Math.sign(zoomTravel)));
    zoomTravel = 0;
    setViewMode(location, VIEW_MODES[next].mode);
  };

  const select = (row: number, mode: { extend?: boolean; toggle?: boolean } = {}) => {
    if (row < 0 || row >= total()) return;
    // What the user picks wins over a selection still waiting for an operation's results.
    if (props.pending) props.onPendingDone();
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
  props.selectRef?.({
    all: () => total() > 0 && setSelection(Selection.range(0, total() - 1)),
    none: () => setSelection(Selection.empty),
    invert: () => setSelection(inverted(selection(), total())),
    rename: () => (selection().count > 1 ? props.onCommand("batch-rename") : startRename()),
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
    const [canPaste, history] = await Promise.all([
      browsing && api.canPaste().catch(() => false),
      api.undoState().catch(() => null),
    ]);
    const command = (name: ListCommand) => () => props.onCommand(name);
    const undo: MenuEntry = history?.erased
      ? { label: "Can't undo a permanent delete", disabled: true, run: () => {} }
      : {
          label: history?.undo ? `Undo ${history.undo}` : "Undo",
          hint: "Ctrl+Z",
          disabled: !history?.undo,
          run: command("undo"),
        };
    const redo: MenuEntry[] = history?.redo
      ? [{ label: `Redo ${history.redo}`, hint: "Ctrl+Y", run: command("redo") }]
      : [];

    const moreOptions = (onItems: boolean): MenuEntry[] => [
      "separator",
      {
        label: "Show more options",
        hint: "Shift+F10",
        disabled: !onItems && !browsing,
        run: () => props.onShellMenu(x, y, onItems, false),
      },
    ];
    const extraItems = props.menuExtras?.items?.() ?? [];
    const extraFolder = props.menuExtras?.folder?.() ?? [];
    const forItems: MenuEntry[] = [
      { label: "Open", hint: "Enter", disabled: !sole, run: () => open(soleRow()) },
      ...(sole?.dir ? [{ label: "Open in new tab", run: () => open(soleRow(), true) }] : []),
      // Explorer parity (parity.ts): another window, the terminal, and where a result lives.
      ...(sole?.dir
        ? [
            { label: "Open in new window", run: () => openNewWindow(sole.path) },
            { label: "Open in Terminal", run: () => openInTerminal(sole.path) },
          ]
        : []),
      ...(sole && !browsing ? [{ label: "Open file location", run: () => revealInFolder(sole.path) }] : []),
      ...(sole && !sole.dir ? [{ label: "Open with…", run: command("open-with") }] : []),
      { label: "Pin to sidebar", hint: "Ctrl+D", run: command("pin") },
      { label: "Tags…", run: command("tags") },
      "separator",
      { label: "Cut", hint: "Ctrl+X", run: command("cut") },
      { label: "Copy", hint: "Ctrl+C", run: command("copy") },
      { label: count > 1 ? "Copy as paths" : "Copy as path", hint: "Ctrl+Shift+C", run: command("copy-path") },
      ...(extraItems.length > 0 ? (["separator", ...extraItems] as MenuEntry[]) : []),
      "separator",
      count > 1
        ? { label: `Rename ${count} items`, hint: "F2", run: command("batch-rename") }
        : { label: "Rename", hint: "F2", disabled: !sole, run: startRename },
      { label: "Delete", hint: "Del", danger: true, run: command("delete") },
      "separator",
      { label: "Properties", hint: "Alt+Enter", disabled: !sole, run: command("properties") },
      ...moreOptions(true),
    ];
    const forFolder: MenuEntry[] = [
      ...extraFolder,
      ...(extraFolder.length > 0 ? (["separator"] as MenuEntry[]) : []),
      { label: "New folder", hint: "Ctrl+Shift+N", disabled: !browsing, run: command("new-folder") },
      // Every other type Windows offers, in a menu of its own, as Explorer's New submenu.
      {
        label: "New",
        disabled: !browsing,
        run: () => setMenu({ x, y, entries: newEntries(activeId(), command("new-folder")) }),
      },
      { label: "Paste", hint: "Ctrl+V", disabled: !canPaste, run: command("paste") },
      { label: "Pin this folder", hint: "Ctrl+D", disabled: !browsing, run: command("pin-folder") },
      "separator",
      undo,
      ...redo,
      "separator",
      { label: "Refresh", hint: "F5", run: command("refresh") },
      { label: "Open in Terminal", disabled: !browsing, run: () => props.listing && openInTerminal(props.listing.path) },
      "separator",
      { label: "Properties", hint: "Alt+Enter", disabled: !browsing, run: command("folder-properties") },
      ...moreOptions(false),
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
    // With nothing selected, Alt+Enter shows the folder's own properties, as in Explorer.
    if (event.altKey && key === "enter") return has ? run("properties") : !isSearch() && run("folder-properties");
    if (key === "delete") return has && run(event.shiftKey ? "erase" : "delete");
    if (key === "f2") return selection().count > 1 ? run("batch-rename") : (startRename(), true);
    if (key === "contextmenu" || (event.shiftKey && key === "f10")) {
      const row = Math.max(0, focused());
      reveal(row);
      const list = viewport.getBoundingClientRect();
      // The menu opens over the item, below its icon and the start of its name, kept inside the
      // list.
      const item = viewport.querySelector(`[data-index="${row}"]`)?.getBoundingClientRect() ?? list;
      const x = item.left + Math.min(40, item.width / 2);
      const y = Math.min(Math.max(item.top + Math.min(item.height, layout().lineSize), list.top + 8), list.bottom - 8);
      // Shift+F10 goes straight to the Windows menu, as in Explorer; the Menu key opens Kestrel's.
      if (event.shiftKey && (has || !isSearch())) props.onShellMenu(x, has ? y : list.top + 8, has, true);
      else void openMenu(x, y, true);
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
    const current = focused();
    if (event.ctrlKey && event.key.toLowerCase() === "a") {
      event.preventDefault();
      // Every item, in every group, collapsed or not.
      if (total() > 0) setSelection(Selection.range(0, total() - 1));
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    const isCharacter = event.key.length === 1 && event.key !== " ";
    if (!isCharacter) resetTypeAhead();
    // A page is a viewful of lines, less one so that the line at the edge stays in sight.
    const { perLine, lineSize } = layout();
    const pageLines = Math.max(1, Math.floor(extent() / lineSize) - 1);
    const target = grouped()
      ? groupedKeyTarget(event.key, current, list(), pageLines)
      : keyTarget(event.key, current, total(), layout(), pageLines * perLine);
    if (target !== null) {
      event.preventDefault();
      if (target >= 0) select(target, { extend: event.shiftKey });
      return;
    }
    switch (event.key) {
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
    target === viewport ||
    (target as HTMLElement).classList.contains("list-spacer") ||
    (target as HTMLElement).classList.contains("list-rows");

  // Mouse handling for an item in any view: a details row, or a cell of the other views.
  const onItemMouseDown = (index: number, event: MouseEvent) => {
    // Middle-click opens a folder in a new tab, as in Explorer.
    if (event.button === 1 && rowAt(index)?.dir) {
      event.preventDefault();
      return open(index, true);
    }
    if (event.button !== 0 || event.target instanceof HTMLInputElement) return;
    resetTypeAhead();
    const mode = { extend: event.shiftKey, toggle: event.ctrlKey };
    // A press on a selected item may start a drag of the whole selection, so narrowing the
    // selection to that item, or toggling it off, waits until the button comes up without a drag.
    const deferred = selection().has(index) && !event.shiftKey;
    if (!deferred) select(index, mode);
    // Folder Options' "Single-click to open an item": a plain click opens it.
    const opens = folderOptions().singleClick && !event.shiftKey && !event.ctrlKey;
    followPress(event, {
      drag: () => {
        const listing = props.listing;
        if (listing && selection().count > 0) void dragOut(listing.set, selection().ranges as RowRange[]);
      },
      click: () => {
        if (deferred) select(index, mode);
        if (opens) open(index);
      },
    });
  };
  const onItemDblClick = (index: number, event: MouseEvent) => {
    if (!(event.target instanceof HTMLInputElement)) open(index);
  };
  const onItemContextMenu = (index: number, event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    // Right-clicking outside the selection acts on that one item instead.
    if (!selection().has(index)) select(index);
    // Shift+right-click goes straight to the Windows menu, as in Explorer.
    if (event.shiftKey) props.onShellMenu(event.clientX, event.clientY, true, true);
    else void openMenu(event.clientX, event.clientY, true);
  };

  // ---------- groups' headers ----------

  const toggleGroup = (index: number) => {
    const label = headerOf(index)?.label;
    if (label === undefined) return;
    const next = new Set(collapsed());
    if (next.has(label)) next.delete(label);
    else next.add(label);
    setCollapsed(next);
  };

  /** Clicking a group's header selects its items, as in Explorer; Ctrl adds them. */
  const selectGroup = (index: number, event: MouseEvent) => {
    const slot = list().slots[index];
    if (!slot || slot.count === 0) return;
    const range = [slot.first, slot.first + slot.count - 1] as const;
    setSelection(event.ctrlKey ? selection().add(range[0], range[1]) : Selection.range(range[0], range[1]));
    setFocused(slot.first);
    anchor = slot.first;
    viewport.focus();
  };

  const groupMenu = (index: number, event: MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    const shut = isCollapsed(index);
    const setAll = async (close: boolean) => {
      const listing = props.listing;
      if (!listing || !close) return setCollapsed(new Set<string>());
      const all = await layoutApi.groupHeaders(listing.set, 0, HEADER_BATCH).catch(() => []);
      setCollapsed(new Set(all.map((header) => header.label)));
    };
    setMenu({
      x: event.clientX,
      y: event.clientY,
      entries: [
        { label: shut ? "Expand group" : "Collapse group", run: () => toggleGroup(index) },
        "separator",
        { label: "Expand all groups", run: () => void setAll(false) },
        { label: "Collapse all groups", run: () => void setAll(true) },
      ],
    });
  };

  // ---------- the selection rectangle ----------

  /** Where cells sit across the scroll axis, for finding the items a rectangle meets. */
  const crossCells = () => {
    const { perLine, itemSize } = layout();
    if (vertical()) return { start: GRID_PADDING, size: Math.max(1, (width() - 2 * GRID_PADDING) / perLine) };
    return { start: LIST_TOP + (grouped() ? GROUP_BAND : 0), size: itemSize };
  };

  /**
   * Draws a selection rectangle from a press on empty space. It selects the items it meets,
   * found by position, so items scrolled out of the DOM are included; Ctrl adds them to the
   * selection, and the list scrolls when the pointer nears or passes its edges.
   */
  const startMarquee = (start: MouseEvent) => {
    const base = start.ctrlKey ? selection() : Selection.empty;
    const box = () => viewport.getBoundingClientRect();
    /** A point of the window in list pixels: along the scroll axis, and across it. */
    const toList = (x: number, y: number) => {
      const rect = box();
      return vertical()
        ? { along: offset() + y - rect.top, across: x - rect.left + viewport.scrollLeft }
        : { along: offset() + x - rect.left, across: y - rect.top };
    };
    const origin = toList(start.clientX, start.clientY);
    let pointer = { x: start.clientX, y: start.clientY };
    let drawing = false;
    let frame = 0;

    const update = () => {
      const now = toList(pointer.x, pointer.y);
      const along: [number, number] = [Math.min(origin.along, now.along), Math.max(origin.along, now.along)];
      const across: [number, number] = [Math.min(origin.across, now.across), Math.max(origin.across, now.across)];
      // The ranges come sorted and apart, so without Ctrl they are the selection as they are.
      const ranges = list().rowsInRect(along, across, crossCells());
      setSelection(base.count === 0 ? Selection.of(ranges) : ranges.reduce((next, [from, to]) => next.add(from, to), base));
      // Back to the viewport's pixels for drawing, clipped to the view.
      const startAlong = Math.max(along[0] - offset(), -2);
      const endAlong = Math.min(along[1] - offset(), extent() + 2);
      const crossShift = vertical() ? viewport.scrollLeft : 0;
      const rect = vertical()
        ? { left: across[0] - crossShift, top: startAlong, width: across[1] - across[0], height: Math.max(0, endAlong - startAlong) }
        : { left: startAlong, top: across[0], width: Math.max(0, endAlong - startAlong), height: across[1] - across[0] };
      setBand(rect);
    };

    const tick = () => {
      const rect = box();
      const [at, low, high] = vertical() ? [pointer.y, rect.top, rect.bottom] : [pointer.x, rect.left, rect.right];
      let step = 0;
      if (at < low + MARQUEE_EDGE) step = -Math.min(1, (low + MARQUEE_EDGE - at) / MARQUEE_EDGE) * MARQUEE_STEP;
      else if (at > high - MARQUEE_EDGE) step = Math.min(1, (at - (high - MARQUEE_EDGE)) / MARQUEE_EDGE) * MARQUEE_STEP;
      if (step !== 0) {
        const before = offset();
        scrollTo(offset() + Math.sign(step) * Math.max(1, Math.round(Math.abs(step))));
        if (offset() !== before) update();
      }
      frame = requestAnimationFrame(tick);
    };

    const move = (event: PointerEvent) => {
      if ((event.buttons & 1) === 0) return finish();
      pointer = { x: event.clientX, y: event.clientY };
      if (!drawing) {
        const far = Math.max(Math.abs(pointer.x - start.clientX), Math.abs(pointer.y - start.clientY)) > MARQUEE_THRESHOLD;
        if (!far) return;
        drawing = true;
        frame = requestAnimationFrame(tick);
      }
      update();
    };
    const finish = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      window.removeEventListener("blur", finish);
      cancelAnimationFrame(frame);
      setBand(null);
      if (drawing && selection().count > 0) {
        // The keyboard carries on from the first item the rectangle took.
        const first = selection().ranges.find(([from, to]) => !base.has(from) || !base.has(to))?.[0] ?? selection().first;
        setFocused(first);
        anchor = first;
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
    window.addEventListener("blur", finish);
  };

  // ---------- columns ----------

  /** The width a column's content needs, from the rows rendered and its header. */
  const measure = (id: ColumnId): number => {
    const header = viewport.closest(".filelist")?.querySelector<HTMLElement>(`.column-header.${id} .column-label`);
    let widest = (header?.scrollWidth ?? 0) + 34;
    if (id === "size") widest = Math.max(widest, 140);
    for (const cell of viewport.querySelectorAll<HTMLElement>(`.row .cell.${id}`)) {
      let need = cell.scrollWidth;
      if (id === "name") {
        const parts = [...cell.children] as HTMLElement[];
        need = 16 + parts.reduce((sum, part) => sum + Math.max(part.scrollWidth, part.getBoundingClientRect().width), 0) + 9 * Math.max(0, parts.length - 1);
      }
      if (id !== "size") widest = Math.max(widest, need + 4);
    }
    return Math.ceil(widest);
  };

  const cell = (id: ColumnId, data: () => Row, index: number) => {
    switch (id) {
      case "name":
        return (
          <div class="cell name">
            <img src={iconUrl(data())} width="16" height="16" alt="" draggable={false} />
            <Show when={renaming() === index} fallback={<span class="label">{displayName(data(), showExtensions())}</span>}>
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
            <Show when={isLocked(data())}>
              <LockGlyph />
            </Show>
            <Show when={badgeOf(data())?.change}>{(change) => <GitGlyph change={change()} dir={data().dir} />}</Show>
            <SyncGlyph row={data()} />
            <TagDots path={data().path} />
          </div>
        );
      case "location":
        return (
          <div class="cell location" title={folderOf(data())}>
            {folderOf(data())}
          </div>
        );
      case "modified":
        return <div class="cell modified">{formatDate(data().mtime)}</div>;
      case "type":
        return <div class="cell type">{typeLabel(data())}</div>;
      case "size":
        return (
          <div class="cell size">
            <span class="weight-track">
              <span class="weight" classList={{ folder: data().dir }} style={{ width: `${sizeBar(data())}%` }} />
            </span>
            <span class="amount">{data().dir && data().size === 0 ? "" : formatSize(data().size)}</span>
          </div>
        );
      case "alloc":
        return <div class="cell alloc">{data().dir && data().alloc === 0 ? "" : formatSize(data().alloc)}</div>;
      case "attributes":
        return <div class="cell attributes">{attributeLetters(data().attrs)}</div>;
      case "extension":
        return <div class="cell extension">{data().dir ? "" : data().ext}</div>;
    }
  };

  const details = () => mode() === "details";
  /** The details view's columns scroll sideways once they need more room than the view has. */
  const columnsOverflow = () => details() && columnsWidth(columnKind()) > width();

  return (
    <div
      class="filelist"
      classList={{ results: isSearch(), grouped: grouped(), "single-click": folderOptions().singleClick }}
      data-view={mode()}
      style={{ "--columns": columnTemplate(columnKind()) }}
    >
      <Show when={props.filter !== null && props.filter !== undefined}>
        <FilterBar
          text={props.filter ?? ""}
          shown={total()}
          of={props.unfilteredTotal ?? null}
          onChange={(text) => props.onFilter?.(text)}
          onClose={() => {
            props.onFilter?.(null);
            viewport.focus();
          }}
          onDone={() => viewport.focus()}
        />
      </Show>
      <ColumnHeaders kind={columnKind()} sort={props.sort} shift={shift()} onSort={props.onSort} measure={measure} />

      <Show when={props.error}>
        <div class="list-error" role="alert">{props.error}</div>
      </Show>

      <div
        ref={viewport}
        class="list-viewport"
        tabindex="0"
        role={details() ? "grid" : "listbox"}
        aria-rowcount={details() ? total() : undefined}
        aria-multiselectable="true"
        style={{ "overflow-x": columnsOverflow() ? "auto" : undefined }}
        onScroll={onScroll}
        onWheel={onWheel}
        onKeyDown={onKeyDown}
        onMouseDown={(event) => {
          if (event.button === 0 && isBackground(event.target)) {
            if (!event.ctrlKey) {
              setSelection(Selection.empty);
              setFocused(-1);
            }
            startMarquee(event);
          }
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          if (!isBackground(event.target)) return;
          setSelection(Selection.empty);
          // Shift+right-click goes straight to the Windows menu, as in Explorer. Search results
          // have no folder of their own, so they keep Kestrel's.
          if (event.shiftKey && !isSearch()) props.onShellMenu(event.clientX, event.clientY, false, true);
          else void openMenu(event.clientX, event.clientY, false);
        }}
      >
        <div
          class="list-spacer"
          style={
            vertical()
              ? { height: `${scrollExtent()}px`, "min-width": columnsOverflow() ? `${columnsWidth(columnKind())}px` : undefined }
              : { width: `${scrollExtent()}px`, height: "100%" }
          }
        >
          <For each={shownGroups()}>
            {(group) => {
              const rows = createMemo(() => rowsOfGroup(group), [], { equals: sameRows });
              return (
                <>
                  <Show when={grouped()}>
                    <GroupHeader
                      index={group}
                      header={headerOf(group)}
                      count={list().slots[group]?.count ?? 0}
                      collapsed={isCollapsed(group)}
                      across={!vertical()}
                      style={headerStyle(group)}
                      onToggle={() => toggleGroup(group)}
                      onSelect={(event) => selectGroup(group, event)}
                      onMenu={(event) => groupMenu(group, event)}
                    />
                  </Show>
                  <Show when={rows().length > 0}>
                    <div
                      class="list-rows"
                      classList={{ "view-cells": !details() && mode() !== "content" && vertical(), "view-columns": !vertical() }}
                      style={blockStyle(group)}
                    >
                      <For each={details() ? rows() : []}>
                        {(index) => {
                          const row = createMemo(() => rowAt(index));
                          const locked = () => {
                            const data = row();
                            return data !== undefined && isLocked(data);
                          };
                          return (
                            <div
                              class="row"
                              classList={{
                                selected: selection().has(index),
                                focused: focused() === index,
                                dimmed: row()?.hidden || locked() || badgeOf(row())?.ignored,
                                locked: locked(),
                                "drop-target": isDropRow(row()?.path),
                              }}
                              title={locked() ? LOCKED_MESSAGE : undefined}
                              role="row"
                              aria-rowindex={index + 1}
                              aria-selected={selection().has(index)}
                              data-index={index}
                              data-path={row()?.path}
                              data-folder={row()?.dir ? "" : undefined}
                              onMouseDown={(event) => onItemMouseDown(index, event)}
                              onDblClick={(event) => onItemDblClick(index, event)}
                              onContextMenu={(event) => onItemContextMenu(index, event)}
                            >
                              <Show when={row()}>{(data) => <For each={columnOrder()}>{(id) => cell(id, data, index)}</For>}</Show>
                            </div>
                          );
                        }}
                      </For>
                      <For each={details() ? [] : rows()}>
                        {(index) => (
                          <ViewItem
                            index={index}
                            row={rowAt(index)}
                            mode={mode()}
                            visual={layout().visual}
                            selected={selection().has(index)}
                            focused={focused() === index}
                            renaming={renaming() === index}
                            showExtension={showExtensions()}
                            locked={(() => {
                              const data = rowAt(index);
                              return data !== undefined && isLocked(data);
                            })()}
                            dropTarget={isDropRow(rowAt(index)?.path)}
                            git={badgeOf(rowAt(index))}
                            loadThumbs={!scrolling()}
                            onMouseDown={(event) => onItemMouseDown(index, event)}
                            onDblClick={(event) => onItemDblClick(index, event)}
                            onContextMenu={(event) => onItemContextMenu(index, event)}
                            onRenameDone={(name) => finishRename(index, name)}
                          />
                        )}
                      </For>
                    </div>
                  </Show>
                </>
              );
            }}
          </For>
        </div>

        <Show when={band()}>
          {(rect) => (
            <div
              class="marquee"
              style={{
                left: `${rect().left + (vertical() ? viewport.scrollLeft : viewport.scrollLeft)}px`,
                top: `${rect().top + (vertical() ? viewport.scrollTop : 0)}px`,
                width: `${rect().width}px`,
                height: `${rect().height}px`,
              }}
            />
          )}
        </Show>

        <Show when={props.listing && total() === 0 && !props.error}>
          <div class="list-empty">
            <Show
              when={(props.filter ?? "").trim() !== ""}
              fallback={
                <Show when={isSearch()} fallback={<p>This folder is empty.</p>}>
                  <p>Nothing matches “{props.query}”.</p>
                  <p class="hint">
                    Try fewer words, or filters such as <code>ext:pdf</code>, <code>size:&gt;100mb</code> or{" "}
                    <code>dm:thisweek</code>.
                  </p>
                </Show>
              }
            >
              <p>No names here contain “{props.filter}”.</p>
              <p class="hint">
                <code>*</code> and <code>?</code> match any text and any one character, over the whole name.
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
