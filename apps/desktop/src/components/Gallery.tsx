import { For, Index, Show, batch, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { api, iconUrl, thumbUrl, type Row, type RowRange } from "../lib/api";
import { baseName, formatCount } from "../lib/format";
import {
  groupOf,
  justify,
  monthGroups,
  nearestInRow,
  rowAt,
  rowCount,
  rowsIn,
  type GalleryLayout,
  type LayoutOptions,
} from "../lib/justify";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { formatLength, mediaApi, type GalleryData, type GalleryFilter, type MediaKind } from "../lib/mediaApi";
import { MEDIA_LIBRARY, followMedia, mediaProgress, progressLine, setMediaPaused } from "../media";
import { revealInFolder } from "../parity";
import { registerPageStepper, setFocusedRow, setQuickLookOpen } from "../previewState";
import { navigate, notify } from "../state";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { MediaIcon } from "./MediaIcon";

const LAYOUT: LayoutOptions = { rowHeight: 168, gap: 4, header: 46, groupGap: 18 };
/** The room left and right of the grid; the timeline lies in the right one (media.css). */
const GUTTERS = 18 + 54;
/** Rows fetched at a time, and how far beyond the view rows are drawn and fetched. */
const CHUNK = 160;
const OVERSCAN = 600;
/** The least time between two reloads while the media pass keeps finding things. */
const RELOAD_EVERY_MS = 3000;
/** Most selected items carried through a reload; a larger selection is let go. */
const MAX_KEPT = 500;
const ONLINE_ONLY = 0x40_0000 | 0x1000;

const monthName = new Intl.DateTimeFormat(undefined, { month: "long", year: "numeric" });
const shortMonth = new Intl.DateTimeFormat(undefined, { month: "short" });
const monthLabel = (year: number, month: number) => monthName.format(new Date(year, month - 1, 1));

/** The filter, kept for as long as the window is open, so coming back finds it as it was. */
const [filter, setFilter] = createSignal<GalleryFilter>({ kind: null, folder: null });

/**
 * Development builds: a Gallery of this many made-up items (no files, no thumbnails), so the
 * end-to-end suite can measure laying out and scrolling tens of thousands of them.
 */
const [synthetic, setSynthetic] = createSignal<number | null>(null);
const SYNTHETIC_SET = -1;

function syntheticData(count: number): GalleryData {
  const shapes = [4 / 3, 3 / 4, 16 / 9, 1, 3 / 2, 2 / 3, 0];
  const now = Date.now();
  // Spread over ten years, newest first, a few items a day on some days.
  const step = (10 * 365 * 86_400_000) / Math.max(1, count);
  return {
    set: SYNTHETIC_SET,
    total: count,
    dates: Array.from({ length: count }, (_, i) => now - Math.floor(i * step)),
    ratios: Array.from({ length: count }, (_, i) => shapes[(i * 7 + (i >> 3)) % shapes.length]),
    videos: Array.from({ length: Math.floor(count / 17) }, (_, i) => [i * 17, (i * 13) % 600] as [number, number]),
    folders: [],
    scopes: ["C:\\Synthetic"],
    thumb: 320,
    kept: [],
  };
}

function syntheticRow(index: number): Row {
  const name = `IMG_${String(index).padStart(6, "0")}.jpg`;
  return { name, path: `C:\\Synthetic\\${name}`, location: "C:\\Synthetic", dir: false, size: 1, mtime: 0, ext: "jpg", hidden: false, alloc: 0, attrs: 0 };
}

/** Inclusive ranges of positions, from a set of them. */
function rangesOf(selected: Set<number>): RowRange[] {
  const sorted = [...selected].sort((a, b) => a - b);
  const ranges: RowRange[] = [];
  for (const at of sorted) {
    const last = ranges[ranges.length - 1];
    if (last && last[1] + 1 === at) last[1] = at;
    else ranges.push([at, at]);
  }
  return ranges;
}

/**
 * The Gallery, as in Explorer: every photo and video in the media library's folders, newest
 * first, in justified rows under a heading per month (by date taken, or when the file last
 * changed), with a timeline at the right to jump through the years. Click to select (Ctrl and
 * Shift as in a list), double-click or Enter to open, Space for Quick Look, the arrow keys to
 * move, right-click for the menu. Only the rows in view are drawn, and their thumbnails come
 * from the thumbnail cache the media pass fills.
 */
export function GalleryPage(props: LocationPageProps) {
  let scroller!: HTMLDivElement;
  let track!: HTMLDivElement;
  const [data, setData] = createSignal<GalleryData | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [width, setWidth] = createSignal(0);
  const [viewport, setViewport] = createSignal(0);
  const [scrollTop, setScrollTop] = createSignal(0);
  const [selected, setSelected] = createSignal<Set<number>>(new Set(), { equals: false });
  const [focus, setFocus] = createSignal<number | null>(null);
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  const [hover, setHover] = createSignal<{ y: number; label: string } | null>(null);
  const [rowsVersion, setRowsVersion] = createSignal(0);
  /** Rows fetched so far for the current set, by position. */
  let rows = new Map<number, Row>();
  const asked = new Set<number>();
  let anchor: number | null = null;
  let loadTicket = 0;
  let dirty = false;
  let lastLoad = 0;
  let reloadTimer: number | undefined;

  followMedia();

  const videos = createMemo(() => new Map(data()?.videos ?? []));
  const groups = createMemo(() => monthGroups(data()?.dates ?? []));
  const layout = createMemo<GalleryLayout | null>(() => {
    const current = data();
    const w = width();
    if (!current || w <= 0) return null;
    return justify(current.ratios, groups(), w, LAYOUT);
  });

  // ---------- loading ----------

  const rowOf = (index: number): Row | undefined => {
    rowsVersion();
    return rows.get(index);
  };

  /** Fetches the chunks of rows covering `from..to` that are not here yet. */
  const fetchRows = (from: number, to: number) => {
    const current = data();
    if (!current) return;
    for (let chunk = Math.floor(from / CHUNK); chunk * CHUNK < Math.min(to, current.total); chunk++) {
      if (asked.has(chunk)) continue;
      asked.add(chunk);
      const set = current.set;
      if (set === SYNTHETIC_SET) {
        for (let i = chunk * CHUNK; i < Math.min(current.total, (chunk + 1) * CHUNK); i++) rows.set(i, syntheticRow(i));
        queueMicrotask(() => setRowsVersion((v) => v + 1));
        continue;
      }
      api
        .rows(set, chunk * CHUNK, CHUNK)
        .then((fetched) => {
          if (data()?.set !== set) return;
          fetched.forEach((row, i) => rows.set(chunk * CHUNK + i, row));
          setRowsVersion((v) => v + 1);
        })
        .catch((error) => {
          asked.delete(chunk);
          // The index was replaced, and the set with it: look again.
          if (String(error).includes("stale")) void load();
        });
    }
  };

  /**
   * Loads the Gallery again: while the pass reads, it does every few seconds. The item at the top
   * of the view stays there, and the focused and selected items stay so, found again by their
   * paths (`kept`); the rows around the view are fetched before the swap, so nothing blanks.
   */
  const load = async () => {
    const ticket = ++loadTicket;
    lastLoad = Date.now();
    dirty = false;
    // The first item in view, and how far its row is below the top of the view; at the very top,
    // the view stays at the top.
    const before = layout();
    const scrolled = scroller?.scrollTop ?? 0;
    const topRow = before && rowCount(before) > 0 && scrolled > 0 ? Math.min(rowCount(before) - 1, rowAt(before, scrolled)) : null;
    const topItem = before && topRow !== null ? before.rowStart[topRow] : null;
    const topOffset = before && topRow !== null ? before.rowTop[topRow] - scrolled : 0;
    const pathAt = (index: number | null) => (index === null ? "" : (rows.get(index)?.path ?? ""));
    const chosen = selected().size <= MAX_KEPT ? [...selected()].map(pathAt).filter((path) => path !== "") : [];
    const keep = [pathAt(topItem), pathAt(focus()), ...chosen];
    const release = (set: number) => set !== SYNTHETIC_SET && void api.releaseSet(set);
    try {
      const fake = synthetic();
      const next = fake !== null ? syntheticData(fake) : await mediaApi.gallery(filter(), keep);
      if (ticket !== loadTicket) return void release(next.set);
      const at = (slot: number) => ((next.kept[slot] ?? -1) >= 0 ? next.kept[slot] : null);
      const fresh = new Map<number, Row>();
      const fetchedChunks: number[] = [];
      if (next.set !== SYNTHETIC_SET && next.total > 0) {
        const first = Math.max(0, Math.floor((at(0) ?? 0) / CHUNK) - 1);
        const fetched = await api.rows(next.set, first * CHUNK, 3 * CHUNK).catch(() => [] as Row[]);
        fetched.forEach((row, i) => fresh.set(first * CHUNK + i, row));
        if (fetched.length > 0) fetchedChunks.push(first, first + 1, first + 2);
      }
      if (ticket !== loadTicket) return void release(next.set);
      const previous = data();
      batch(() => {
        rows = fresh;
        asked.clear();
        fetchedChunks.forEach((chunk) => asked.add(chunk));
        setData(next);
        setFailure(null);
        setLoading(false);
        setRowsVersion((v) => v + 1);
        setSelected(new Set(next.kept.slice(2).filter((index) => index >= 0)));
        setFocus(at(1));
        anchor = at(1);
      });
      if (previous) release(previous.set);
      const shape = layout();
      const top = at(0);
      if (shape && top !== null) scroller.scrollTop = shape.rowTop[shape.rowOf[top]] - topOffset;
    } catch (error) {
      if (ticket !== loadTicket) return;
      setFailure(String(error));
      setLoading(false);
    }
  };

  /** Reloads when the pass has found something, at most every few seconds, and only on screen. */
  const changed = () => {
    if (!props.active) {
      dirty = true;
      return;
    }
    clearTimeout(reloadTimer);
    reloadTimer = window.setTimeout(() => void load(), Math.max(0, lastLoad + RELOAD_EVERY_MS - Date.now()));
  };

  createEffect(on([() => props.revision, filter, synthetic], () => void load()));
  createEffect(on(() => props.active, (active) => active && dirty && void load(), { defer: true }));

  onMount(() => {
    props.focusRef(() => scroller.focus());
    if (props.active) queueMicrotask(() => takeFocusIfFree(scroller));
    const observer = new ResizeObserver(() => {
      setWidth(scroller.clientWidth - GUTTERS);
      setViewport(scroller.clientHeight);
    });
    observer.observe(scroller);
    const stopChanged = mediaApi.onChanged(changed);
    const forgetStepper = registerPageStepper(props.tabId, (delta) => moveBy(delta));
    onCleanup(() => {
      observer.disconnect();
      clearTimeout(reloadTimer);
      void stopChanged.then((stop) => stop());
      forgetStepper();
      setLocationStatus(props.tabId, null);
      const current = data();
      if (current && current.set !== SYNTHETIC_SET) void api.releaseSet(current.set);
    });
  });

  // ---------- what is in view ----------

  const band = createMemo<[number, number] | null>(() => {
    const shape = layout();
    if (!shape) return null;
    return rowsIn(shape, scrollTop() - OVERSCAN, scrollTop() + viewport() + OVERSCAN);
  });

  const visible = createMemo<number[]>(() => {
    const shape = layout();
    const rowsShown = band();
    if (!shape || !rowsShown) return [];
    const from = shape.rowStart[rowsShown[0]];
    const to = shape.rowStart[rowsShown[1]];
    return Array.from({ length: to - from }, (_, i) => from + i);
  });

  createEffect(() => {
    const shown = visible();
    if (shown.length > 0) fetchRows(shown[0], shown[shown.length - 1] + 1);
  });

  // Items on screen that the pass has not read yet are read next, once scrolling rests.
  let hurryTimer: number | undefined;
  const hurried = new Set<string>();
  createEffect(() => {
    const current = data();
    const shown = visible();
    rowsVersion();
    clearTimeout(hurryTimer);
    if (!current || current.set === SYNTHETIC_SET) return;
    hurryTimer = window.setTimeout(() => {
      const paths = shown
        .filter((index) => (current.ratios[index] ?? 0) === 0)
        .map((index) => rows.get(index))
        .filter((row): row is Row => !!row && (row.attrs & ONLINE_ONLY) === 0 && !hurried.has(row.path))
        .map((row) => row.path);
      if (paths.length === 0) return;
      paths.forEach((path) => hurried.add(path));
      void mediaApi.hurry(paths).catch(() => {});
    }, 400);
  });
  onCleanup(() => clearTimeout(hurryTimer));

  const visibleGroups = createMemo(() => {
    const shape = layout();
    if (!shape) return [];
    const top = scrollTop() - OVERSCAN;
    const bottom = scrollTop() + viewport() + OVERSCAN;
    return shape.groups.filter((group, i) => {
      const end = shape.groups[i + 1]?.top ?? shape.height;
      return end >= top && group.top <= bottom;
    });
  });

  /** The month at the top of the view, shown pinned while its heading is scrolled away. */
  const currentMonth = createMemo(() => {
    const shape = layout();
    if (!shape || shape.groups.length === 0 || rowCount(shape) === 0) return null;
    const row = Math.min(rowCount(shape) - 1, rowAt(shape, scrollTop() + 8));
    const group = shape.groups[groupOf(shape, shape.rowStart[Math.max(0, row)])];
    if (group.top + LAYOUT.header / 2 >= scrollTop()) return null;
    return monthLabel(group.year, group.month);
  });

  // ---------- the status bar ----------

  createEffect(() => {
    const current = data();
    if (!current) return setLocationStatus(props.tabId, null);
    const parts = [`${formatCount(current.total)} ${current.total === 1 ? "item" : "items"}`];
    if (selected().size > 0) parts.push(`${formatCount(selected().size)} selected`);
    setLocationStatus(props.tabId, parts);
  });

  // ---------- focus, selection and the preview ----------

  // The preview pane, the details pane and Quick Look follow the focused item. One whose row is
  // still on its way keeps the last until it comes, so Quick Look stays open while stepping.
  createEffect(() => {
    const at = focus();
    if (at === null) return setFocusedRow(props.tabId, null);
    const row = rowOf(at);
    if (row) setFocusedRow(props.tabId, row);
  });

  const reveal = (index: number) => {
    const shape = layout();
    if (!shape) return;
    const row = shape.rowOf[index];
    const top = shape.rowTop[row];
    const bottom = top + shape.rowHeight[row];
    const header = groups()[groupOf(shape, index)];
    // The first row of a month brings its heading into view too.
    const above = header && shape.rowStart[row] === header.start ? top - LAYOUT.header : top;
    if (above < scroller.scrollTop) scroller.scrollTop = above - 6;
    else if (bottom > scroller.scrollTop + scroller.clientHeight) scroller.scrollTop = bottom - scroller.clientHeight + 10;
  };

  const select = (index: number, mode: { extend?: boolean; toggle?: boolean } = {}) => {
    const current = data();
    if (!current || current.total === 0) return;
    const at = Math.max(0, Math.min(current.total - 1, index));
    batch(() => {
      if (mode.extend && anchor !== null) {
        const [from, to] = anchor <= at ? [anchor, at] : [at, anchor];
        const next = mode.toggle ? new Set(selected()) : new Set<number>();
        for (let i = from; i <= to; i++) next.add(i);
        setSelected(next);
      } else if (mode.toggle) {
        const next = new Set(selected());
        if (next.has(at)) next.delete(at);
        else next.add(at);
        setSelected(next);
        anchor = at;
      } else {
        setSelected(new Set([at]));
        anchor = at;
      }
      setFocus(at);
    });
    fetchRows(at, at + 1);
    reveal(at);
  };

  const moveBy = (delta: number) => {
    const current = data();
    if (!current || current.total === 0) return;
    select((focus() ?? (delta > 0 ? -1 : current.total)) + delta);
  };

  /** Up or down a row, to the item nearest the same place across. */
  const moveRows = (rowsDown: number, extend: boolean) => {
    const shape = layout();
    const at = focus();
    if (!shape) return;
    if (at === null) return select(0);
    const row = Math.max(0, Math.min(rowCount(shape) - 1, shape.rowOf[at] + rowsDown));
    select(nearestInRow(shape, row, shape.left[at] + shape.itemWidth[at] / 2), { extend });
  };

  const selection = (): { set: number; ranges: RowRange[] } | null => {
    const current = data();
    if (!current || selected().size === 0) return null;
    return { set: current.set, ranges: rangesOf(selected()) };
  };

  const selectedRows = async (): Promise<Row[]> => {
    const chosen = selection();
    if (!chosen) return [];
    const found: Row[] = [];
    for (const [from, to] of chosen.ranges) {
      for (let at = from; at <= to; at++) {
        const known = rows.get(at);
        if (known) found.push(known);
      }
    }
    if (found.length === selected().size) return found;
    const fetched = await Promise.all(chosen.ranges.map(([from, to]) => api.rows(chosen.set, from, to - from + 1)));
    return fetched.flat();
  };

  // ---------- commands ----------

  const open = async (index?: number) => {
    const at = index ?? focus();
    if (at === null || at === undefined) return;
    const row = rowOf(at) ?? (await selectedRows())[0];
    if (row) void api.openPath(row.path).catch(notify);
  };

  const quickLook = () => {
    const at = focus();
    if (at === null) return;
    const row = rowOf(at);
    if (!row) return;
    setFocusedRow(props.tabId, row);
    setQuickLookOpen(true);
  };

  /** Sends the selection to the Recycle Bin, then shows the Gallery as the index has it then. */
  const recycle = async () => {
    const chosen = selection();
    if (!chosen) return;
    try {
      const done = await api.deleteItems(chosen.set, chosen.ranges, false);
      if (!done) return;
      // The index hears of the deletion a moment later.
      window.setTimeout(() => void load(), 700);
      window.setTimeout(() => void load(), 2500);
    } catch (error) {
      notify(error);
    }
  };

  const copy = async () => {
    const chosen = selection();
    if (chosen) await api.copyItems(chosen.set, chosen.ranges, false).catch(notify);
  };

  const copyPaths = async () => {
    const chosen = await selectedRows();
    if (chosen.length > 0) await navigator.clipboard.writeText(chosen.map((row) => `"${row.path}"`).join("\r\n")).catch(notify);
  };

  const properties = async () => {
    const [row] = await selectedRows();
    if (row) await api.showProperties(row.path).catch(notify);
  };

  const windowsMenu = (x: number, y: number) => {
    const chosen = selection();
    if (!chosen) return;
    const dark = !window.matchMedia("(prefers-color-scheme: light)").matches;
    void api
      .shellMenu(chosen.set, chosen.ranges, x, y, dark, true)
      .then(() => window.setTimeout(() => void load(), 700))
      .catch(notify);
  };

  const showLocation = async () => {
    const [row] = await selectedRows();
    if (row) revealInFolder(row.path);
  };

  const menuEntries = (x: number, y: number): MenuEntry[] => {
    const many = selected().size > 1;
    return [
      { label: "Open", hint: "Enter", disabled: many, run: () => void open() },
      { label: "Quick Look", hint: "Space", disabled: many, run: quickLook },
      { label: "Open file location", disabled: many, run: () => void showLocation() },
      "separator",
      { label: "Copy", hint: "Ctrl+C", run: () => void copy() },
      { label: "Copy as path", hint: "Ctrl+Shift+C", run: () => void copyPaths() },
      "separator",
      { label: "Delete", hint: "Del", run: () => void recycle() },
      "separator",
      { label: "Properties", hint: "Alt+Enter", disabled: many, run: () => void properties() },
      { label: "Show more options", hint: "Shift+F10", run: () => windowsMenu(x, y) },
    ];
  };

  const openMenu = (x: number, y: number) => setMenu({ x, y, entries: menuEntries(x, y) });

  const onKeyDown = (event: KeyboardEvent) => {
    const current = data();
    if (!current || menu()) return;
    const key = event.key;
    const handled = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    const extend = event.shiftKey;
    switch (key) {
      case "ArrowRight":
        return handled(), select((focus() ?? -1) + 1, { extend });
      case "ArrowLeft":
        return handled(), select((focus() ?? 1) - 1, { extend });
      case "ArrowDown":
        return handled(), moveRows(1, extend);
      case "ArrowUp":
        return handled(), moveRows(-1, extend);
      case "PageDown":
        return handled(), moveRows(Math.max(1, Math.floor(scroller.clientHeight / LAYOUT.rowHeight)), extend);
      case "PageUp":
        return handled(), moveRows(-Math.max(1, Math.floor(scroller.clientHeight / LAYOUT.rowHeight)), extend);
      case "Home":
        return handled(), select(0, { extend });
      case "End":
        return handled(), select(current.total - 1, { extend });
      case " ":
        if (event.ctrlKey) return handled(), focus() !== null && select(focus()!, { toggle: true });
        return handled(), quickLook();
      case "Enter":
        if (event.altKey) return handled(), void properties();
        return handled(), void open();
      case "Delete":
        if (event.shiftKey) return;
        return handled(), void recycle();
      case "Escape":
        return handled(), setSelected(new Set<number>());
      case "ContextMenu":
      case "F10": {
        if (key === "F10" && !event.shiftKey) return;
        handled();
        const at = focus();
        if (at === null) return;
        if (!selected().has(at)) select(at);
        const tile = scroller.querySelector<HTMLElement>(`[data-index="${at}"]`)?.getBoundingClientRect();
        if (!tile) return;
        if (key === "F10") return windowsMenu(tile.left + 24, tile.top + 24);
        return openMenu(tile.left + 24, tile.top + 24);
      }
    }
    if (event.ctrlKey && key.toLowerCase() === "a") {
      handled();
      setSelected(new Set(Array.from({ length: current.total }, (_, i) => i)));
      return;
    }
    if (event.ctrlKey && event.shiftKey && key.toLowerCase() === "c") return handled(), void copyPaths();
    if (event.ctrlKey && key.toLowerCase() === "c") return handled(), void copy();
  };

  const onTileDown = (index: number, event: MouseEvent) => {
    if (event.button === 0) select(index, { extend: event.shiftKey, toggle: event.ctrlKey });
    if (event.button === 2 && !selected().has(index)) select(index);
  };

  // ---------- the timeline ----------

  const scrollable = () => Math.max(0, (layout()?.height ?? 0) + 24 - viewport());

  /** The timeline's height, followed as the window resizes. */
  const [trackHeight, setTrackHeight] = createSignal(0);
  const trackRef = (element: HTMLDivElement) => {
    track = element;
    const observer = new ResizeObserver(() => setTrackHeight(element.clientHeight));
    observer.observe(element);
    onCleanup(() => observer.disconnect());
  };

  /** Where on the timeline a point of the layout is. */
  const trackY = (top: number) => {
    const shape = layout();
    if (!shape || shape.height <= 0) return 0;
    return (top / shape.height) * trackHeight();
  };

  /**
   * The timeline's labels: each year where it starts, as room allows, and the months between
   * them only where there is plenty of room (a library of a year or two).
   */
  const years = createMemo(() => {
    const shape = layout();
    if (!shape || trackHeight() <= 0) return [];
    const marks: { label: string; y: number; year: boolean }[] = [];
    let lastYear = -1;
    for (const group of shape.groups) {
      if (group.year === lastYear) continue;
      lastYear = group.year;
      const y = trackY(group.top);
      if (marks.length === 0 || y - marks[marks.length - 1].y >= 20) marks.push({ label: String(group.year), y, year: true });
    }
    const months: { label: string; y: number; year: boolean }[] = [];
    const roomy = (y: number) => [...marks, ...months].every((mark) => Math.abs(mark.y - y) >= 34);
    for (const group of shape.groups) {
      const y = trackY(group.top);
      if (roomy(y)) months.push({ label: shortMonth.format(new Date(group.year, group.month - 1, 1)), y, year: false });
    }
    return [...marks, ...months];
  });

  const monthAtTrack = (y: number) => {
    const shape = layout();
    if (!shape || shape.groups.length === 0) return null;
    const fraction = Math.max(0, Math.min(1, y / track.clientHeight));
    const top = fraction * shape.height;
    let found = shape.groups[0];
    for (const group of shape.groups) {
      if (group.top <= top) found = group;
      else break;
    }
    return { group: found, top };
  };

  const scrub = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const box = track.getBoundingClientRect();
    track.setPointerCapture(event.pointerId);
    const go = (moved: PointerEvent) => {
      const y = Math.max(0, Math.min(box.height, moved.clientY - box.top));
      const at = monthAtTrack(y);
      if (!at) return;
      setHover({ y, label: monthLabel(at.group.year, at.group.month) });
      // Lands on the month's heading, as Explorer's timeline does.
      scroller.scrollTop = Math.min(scrollable(), at.group.top);
    };
    go(event);
    const end = () => {
      track.removeEventListener("pointermove", go);
      track.removeEventListener("pointerup", end);
      setHover(null);
    };
    track.addEventListener("pointermove", go);
    track.addEventListener("pointerup", end);
  };

  // ---------- the header ----------

  const kinds: { kind: MediaKind | null; label: string }[] = [
    { kind: null, label: "All" },
    { kind: "photo", label: "Photos" },
    { kind: "video", label: "Videos" },
  ];

  const folderMenu = (event: MouseEvent) => {
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const current = data();
    const chosen = filter().folder;
    const entries: MenuEntry[] = [
      { label: "All folders", checked: chosen === null, run: () => setFilter((f) => ({ ...f, folder: null })) },
    ];
    const scopes = current?.scopes ?? [];
    if (scopes.length > 1) {
      entries.push("separator");
      for (const scope of scopes) entries.push({ label: scope, checked: chosen === scope, run: () => setFilter((f) => ({ ...f, folder: scope })) });
    }
    const folders = (current?.folders ?? []).filter((folder) => !scopes.includes(folder.path));
    if (folders.length > 0) {
      entries.push("separator");
      for (const folder of folders) {
        entries.push({
          label: `${baseName(folder.path) || folder.path} (${formatCount(folder.count)})`,
          checked: chosen === folder.path,
          run: () => setFilter((f) => ({ ...f, folder: folder.path })),
        });
      }
    }
    setMenu({ x: box.left, y: box.bottom + 4, entries });
  };

  const progress = () => mediaProgress();
  const running = () => progress()?.state === "running" || progress()?.state === "waiting";

  const emptyMessage = () => {
    const current = data();
    if (!current) return "";
    if (current.scopes.length === 0) return "No folders are in the media library yet. Choose some in Library settings.";
    if (filter().folder || filter().kind) return "Nothing here matches the filter.";
    if (progress()?.state === "starting") return "Looking for photos and videos in a moment.";
    return "No photos or videos in the media library's folders.";
  };

  return (
    <div class="gallery" aria-label="Gallery">
      <header class="gallery-bar">
        <h1 class="gallery-title">Gallery</h1>
        <div class="gallery-kinds" role="radiogroup" aria-label="Show">
          <For each={kinds}>
            {(choice) => (
              <button
                class="gallery-kind"
                role="radio"
                aria-checked={filter().kind === choice.kind}
                classList={{ chosen: filter().kind === choice.kind }}
                data-kind={choice.kind ?? "all"}
                onClick={() => setFilter((f) => ({ ...f, kind: choice.kind }))}
              >
                {choice.label}
              </button>
            )}
          </For>
        </div>
        <button class="gallery-folder" aria-haspopup="menu" title={filter().folder ?? "All folders"} onClick={folderMenu}>
          <MediaIcon name="folder" />
          <span>{filter().folder ? baseName(filter().folder!) || filter().folder : "All folders"}</span>
          <MediaIcon name="chevron" size={12} />
        </button>
        <span class="gallery-bar-spacer" />
        <Show when={progress()}>
          {(p) => (
            <div class="media-status" data-state={p().state}>
              <span class="media-status-dot" aria-hidden="true" />
              <span class="media-status-text">{progressLine(p())}</span>
              <Show when={running() || p().state === "paused"}>
                <button
                  class="media-status-action"
                  data-action={p().state === "paused" ? "resume" : "pause"}
                  onClick={() => void setMediaPaused(p().state !== "paused")}
                >
                  <MediaIcon name={p().state === "paused" ? "play" : "pause"} size={14} />
                  <span>{p().state === "paused" ? "Resume" : "Pause"}</span>
                </button>
              </Show>
            </div>
          )}
        </Show>
        <button class="gallery-settings" title="Choose the folders, see what was read, pause or resume" onClick={() => navigate(props.tabId, MEDIA_LIBRARY)}>
          <MediaIcon name="settings" />
          <span>Library settings</span>
        </button>
      </header>
      <div class="gallery-body">
        <div
          ref={scroller}
          class="gallery-scroller"
          tabindex="0"
          role="listbox"
          aria-multiselectable="true"
          aria-label="Photos and videos"
          onScroll={() => setScrollTop(scroller.scrollTop)}
          onKeyDown={onKeyDown}
          onMouseDown={(event) => {
            if (event.target === event.currentTarget || (event.target as HTMLElement).classList.contains("gallery-canvas")) {
              if (!event.ctrlKey && !event.shiftKey) setSelected(new Set<number>());
            }
          }}
          onContextMenu={(event) => {
            event.preventDefault();
            const tile = (event.target as HTMLElement).closest<HTMLElement>("[data-index]");
            if (tile && selected().size > 0) openMenu(event.clientX, event.clientY);
          }}
        >
          <Show when={layout()}>
            {(shape) => (
              <div class="gallery-canvas" style={{ height: `${shape().height + 24}px` }}>
                <For each={visibleGroups()}>
                  {(group) => (
                    <div class="gallery-month" style={{ top: `${group.top}px`, height: `${LAYOUT.header}px` }} data-month={`${group.year}-${String(group.month).padStart(2, "0")}`}>
                      <h2>{monthLabel(group.year, group.month)}</h2>
                      <span class="gallery-month-count">{formatCount(group.count)}</span>
                    </div>
                  )}
                </For>
                <Index each={visible()}>
                  {(index) => {
                    const row = () => rowOf(index());
                    const box = () => {
                      const s = shape();
                      const r = s.rowOf[index()];
                      return { left: s.left[index()], top: s.rowTop[r], width: s.itemWidth[index()], height: s.rowHeight[r] };
                    };
                    const cloud = () => ((row()?.attrs ?? 0) & ONLINE_ONLY) !== 0;
                    const length = () => videos().get(index());
                    const ratioKnown = () => (data()?.ratios[index()] ?? 0) > 0;
                    return (
                      <div
                        class="gallery-tile"
                        role="option"
                        aria-selected={selected().has(index())}
                        classList={{ selected: selected().has(index()), focused: focus() === index(), plain: !ratioKnown() || cloud() }}
                        data-index={index()}
                        data-path={row()?.path}
                        title={row()?.name}
                        style={{ left: `${box().left}px`, top: `${box().top}px`, width: `${box().width}px`, height: `${box().height}px` }}
                        onMouseDown={(event) => onTileDown(index(), event)}
                        onDblClick={() => void open(index())}
                      >
                        <Show when={data()?.set === SYNTHETIC_SET}>
                          <span class="gallery-swatch" style={{ "--hue": String((index() * 37) % 360) }} />
                        </Show>
                        <Show when={data()?.set !== SYNTHETIC_SET && row()}>
                          {(loaded) => (
                            <img
                              // Once the pass has read an item, its thumbnail is in the cache: the
                              // address changes so a stand-in shown before is asked for again.
                              src={cloud() ? iconUrl(loaded(), 48) : `${thumbUrl(loaded(), data()?.thumb ?? 320)}${ratioKnown() ? "&v=1" : ""}`}
                              alt=""
                              loading="lazy"
                              decoding="async"
                              draggable={false}
                              onError={(event) => (event.currentTarget.src = iconUrl(loaded(), 48))}
                            />
                          )}
                        </Show>
                        <Show when={length() !== undefined}>
                          <span class="gallery-video-badge">
                            <MediaIcon name="play" size={10} filled />
                            <Show when={(length() ?? 0) > 0}>{formatLength(length()!)}</Show>
                          </span>
                        </Show>
                        <Show when={cloud()}>
                          <span class="gallery-cloud-badge" title="Online only">
                            <MediaIcon name="cloud" size={12} />
                          </span>
                        </Show>
                      </div>
                    );
                  }}
                </Index>
              </div>
            )}
          </Show>
          <Show when={!loading() && data()?.total === 0}>
            <div class="gallery-empty">
              <MediaIcon name="gallery" size={36} />
              <p>{emptyMessage()}</p>
              <Show when={data()?.scopes.length === 0}>
                <button class="media-button" onClick={() => navigate(props.tabId, MEDIA_LIBRARY)}>
                  Library settings
                </button>
              </Show>
            </div>
          </Show>
          <Show when={failure()}>
            <p class="gallery-empty gallery-failure">{failure()}</p>
          </Show>
        </div>
        <Show when={currentMonth()}>
          {(label) => (
            <div class="gallery-current-month" aria-hidden="true">
              {label()}
            </div>
          )}
        </Show>
        <Show when={layout() && scrollable() > 0}>
          <div
            ref={trackRef}
            class="gallery-timeline"
            aria-label="Timeline"
            onPointerDown={scrub}
            onPointerMove={(event) => {
              if (event.buttons !== 0) return;
              const y = event.clientY - track.getBoundingClientRect().top;
              const at = monthAtTrack(y);
              setHover(at ? { y, label: monthLabel(at.group.year, at.group.month) } : null);
            }}
            onPointerLeave={(event) => event.buttons === 0 && setHover(null)}
          >
            <div
              class="gallery-timeline-thumb"
              style={{ top: `${trackY(scrollTop())}px`, height: `${Math.max(14, Math.min(trackHeight(), trackY(viewport())))}px` }}
            />
            <For each={years()}>
              {(mark) => (
                <span class="gallery-timeline-mark" classList={{ year: mark.year }} style={{ top: `${mark.y}px` }}>
                  {mark.label}
                </span>
              )}
            </For>
            <Show when={hover()}>
              {(at) => (
                <span class="gallery-timeline-bubble" style={{ top: `${at().y}px` }}>
                  {at().label}
                </span>
              )}
            </Show>
          </div>
        </Show>
      </div>
      <Show when={menu()}>
        {(at) => (
          <ContextMenu
            x={at().x}
            y={at().y}
            entries={at().entries}
            onClose={() => {
              setMenu(null);
              scroller.focus();
            }}
          />
        )}
      </Show>
    </div>
  );
}

// Development builds: made-up Galleries for the end-to-end suite (see `synthetic`).
if (import.meta.env.DEV) {
  Object.assign(window, { __kestrelGallery: { synthetic: (count: number | null) => setSynthetic(count) } });
}
