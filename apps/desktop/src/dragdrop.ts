/**
 * Drag and drop of files, in and out.
 *
 * Out: pressing on selected rows and moving a few pixels starts a native Windows drag of those
 * files (see `dragdrop.rs`), which any application can take. Tauri's drop handling replaces
 * the web view's own HTML5 drag and drop, so the gesture is followed with pointer events.
 *
 * In: Tauri reports where files are dragged over the window and where they are dropped. This
 * module finds the folder under the cursor (a folder row, a sidebar place, a tab, or the open
 * folder itself), highlights it, scrolls the list near its edges, opens a folder hovered for a
 * second, and on drop has the backend copy or move the files there. A drag that starts in
 * Kestrel and ends in it takes the same path as one from Explorer.
 */
import { createSignal } from "solid-js";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { api, type DragRehearsal, type DropEffect, type DropKeys, type RowRange } from "./lib/api";
import { baseName, parentPath } from "./lib/format";
import { activateTab, activeTab, isSearch, navigate, notify, selectInFolder, tabs } from "./state";

export type DropTarget =
  /** A folder row in the list on screen. */
  | { kind: "row"; path: string }
  /** The folder the list on screen shows: the drop lands on empty space or on a file. */
  | { kind: "folder"; path: string; tabId: number }
  /** A place or drive in the sidebar, with the name it shows there. */
  | { kind: "place"; path: string; name?: string }
  /** A tab, standing for the folder it shows. */
  | { kind: "tab"; path: string; tabId: number };

export interface Dropped {
  effect: DropEffect;
  names: string[];
}

/** How far the pointer moves with the button down before a press becomes a drag, as in Windows. */
const DRAG_THRESHOLD = 4;
/** How long a folder is hovered during a drag before it opens. */
const SPRING_MS = 1000;
/** Depth of the bands along the list's top and bottom edges that scroll it during a drag. */
const SCROLL_EDGE = 40;
/** Fastest automatic scroll, in pixels per frame, reached at the very edge. */
const SCROLL_STEP = 20;
/** How often the copy-or-move hint is re-read while hovering, so it follows Ctrl and Shift. */
const HINT_MS = 150;
const NOTHING: Dropped = { effect: "none", names: [] };

const [target, setTarget] = createSignal<DropTarget | null>(null);
const [effect, setEffect] = createSignal<DropEffect>("none");

/** The folder files would be dropped into right now, if any. */
export const dropTarget = target;

/** A drag in progress over the window. */
interface Session {
  paths: string[];
  /** The same paths in comparable form. */
  sources: string[];
  /** The cursor, in CSS pixels relative to the window. */
  point: { x: number; y: number } | null;
  frame: number;
  spring: number | undefined;
  hintAt: number;
}

let session: Session | null = null;
/** Keys a test holds for its synthetic drops; real drags read the keyboard. */
let testKeys: DropKeys | undefined;
/** Dev builds can rehearse drags out without a mouse, for tests. */
let rehearsal: DragRehearsal | undefined;
let rehearsing = false;
let lastDragOut: { effect: DropEffect; items: number } | null = null;

const comparable = (path: string) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
const tabIdOf = (target: DropTarget) => ("tabId" in target ? target.tabId : undefined);
const sameTarget = (a: DropTarget | null, b: DropTarget | null) =>
  a === b ||
  (a !== null && b !== null && a.kind === b.kind && comparable(a.path) === comparable(b.path) && tabIdOf(a) === tabIdOf(b));

/**
 * Whether the dragged items can be dropped into `path`: not into one of themselves or anything
 * inside one, and not into the folder they are all already in, where a drop does nothing.
 */
function accepts(path: string): boolean {
  if (!session) return false;
  const destination = comparable(path);
  const sources = session.sources;
  if (sources.some((source) => destination === source || destination.startsWith(`${source}\\`))) return false;
  return !sources.every((source) => comparable(parentPath(source) ?? "") === destination);
}

/** The drop target at a point in the window. */
function targetAt(x: number, y: number): DropTarget | null {
  const element = document.elementFromPoint(x, y);
  if (!element) return null;

  const tabElement = element.closest<HTMLElement>("[data-tab-id]");
  if (tabElement) {
    const tab = tabs.find((tab) => tab.id === Number(tabElement.dataset.tabId));
    return tab && !isSearch(tab) && accepts(tab.path) ? { kind: "tab", path: tab.path, tabId: tab.id } : null;
  }
  const place = element.closest<HTMLElement>("[data-place]");
  if (place) {
    const path = place.dataset.place ?? "";
    return accepts(path) ? { kind: "place", path, name: place.dataset.placeName } : null;
  }

  if (!element.closest(".tab-panel.active .filelist")) return null;
  // A details row, or an item of the icon, list and tile views.
  const row = element.closest<HTMLElement>(".row[data-path], .view-item[data-path]");
  if (row && row.dataset.folder !== undefined) {
    // A folder that can't take the drop (one being dragged, say) doesn't pass it on to the
    // folder around it, as in Explorer.
    const path = row.dataset.path ?? "";
    return accepts(path) ? { kind: "row", path } : null;
  }
  const tab = activeTab();
  return tab && !isSearch(tab) && accepts(tab.path) ? { kind: "folder", path: tab.path, tabId: tab.id } : null;
}

/** Looks again at what is under the cursor, which changes as the list scrolls or a folder opens. */
function retarget() {
  if (!session) return;
  const point = session.point;
  const next = point ? targetAt(point.x, point.y) : null;
  if (sameTarget(next, target())) return;
  setTarget(next);
  setEffect("none");
  session.hintAt = 0;
  clearTimeout(session.spring);
  // Hovering a folder, place or tab opens it; the open folder itself has nothing to open.
  session.spring = next && next.kind !== "folder" ? window.setTimeout(() => springOpen(next), SPRING_MS) : undefined;
}

function springOpen(opened: DropTarget) {
  if (!session || !sameTarget(target(), opened)) return;
  if (opened.kind === "tab") return activateTab(opened.tabId);
  const tab = activeTab();
  if (tab) navigate(tab.id, opened.path);
}

/**
 * How far to scroll this frame for the cursor at `at`, given the list's `start` and `end` edges
 * along that axis. The band before `start` reaches out to `outer`, so headers count as an edge.
 */
function edgeStep(at: number, start: number, end: number, outer = start) {
  if (at >= outer && at < start + SCROLL_EDGE) return -SCROLL_STEP * Math.min(1, (start + SCROLL_EDGE - at) / SCROLL_EDGE);
  if (at > end - SCROLL_EDGE && at <= end) return SCROLL_STEP * ((at - (end - SCROLL_EDGE)) / SCROLL_EDGE);
  return 0;
}

/** A scroll step in whole pixels, never less than one. */
const whole = (step: number) => Math.sign(step) * Math.max(1, Math.round(Math.abs(step)));

/**
 * Scrolls the list on screen while the cursor is near its top or bottom edge, or near its left
 * or right edge in the list view, whose columns scroll sideways.
 */
function autoScroll() {
  const point = session?.point;
  const viewport = document.querySelector<HTMLElement>(".tab-panel.active .list-viewport");
  if (!point || !viewport) return;
  const box = viewport.getBoundingClientRect();
  if (viewport.closest(".filelist")?.getAttribute("data-view") === "list") {
    if (point.y < box.top || point.y > box.bottom) return;
    const step = edgeStep(point.x, box.left, box.right);
    if (step !== 0) viewport.scrollLeft += whole(step);
    return;
  }
  if (point.x < box.left || point.x > box.right) return;
  // The column headers sit just above the rows and count as part of the top edge.
  const top = viewport.closest(".filelist")?.getBoundingClientRect().top ?? box.top;
  const step = edgeStep(point.y, box.top, box.bottom, top);
  if (step !== 0) viewport.scrollTop += whole(step);
}

/** Asks what a drop on the current target would do, with the keys held now. */
function refreshHint() {
  const current = target();
  if (!session || !current) return;
  session.hintAt = performance.now();
  api
    .dropEffect(session.paths, current.path, testKeys)
    .then((next) => {
      if (sameTarget(target(), current)) setEffect(next);
    })
    .catch(() => {});
}

/** Runs every frame while a drag is over the window. */
function tick() {
  if (!session) return;
  autoScroll();
  retarget();
  if (target() && performance.now() - session.hintAt > HINT_MS) refreshHint();
  session.frame = requestAnimationFrame(tick);
}

function begin(paths: string[]): Session {
  end();
  const started: Session = { paths, sources: paths.map(comparable), point: null, frame: 0, spring: undefined, hintAt: 0 };
  started.frame = requestAnimationFrame(tick);
  session = started;
  return started;
}

function end() {
  if (!session) return;
  cancelAnimationFrame(session.frame);
  clearTimeout(session.spring);
  session = null;
  setTarget(null);
  setEffect("none");
}

function enter(paths: string[], x: number, y: number) {
  begin(paths);
  over(x, y);
}

function over(x: number, y: number) {
  if (!session) return;
  session.point = { x, y };
  retarget();
}

const leave = () => end();

/** Copies or moves dropped files into the folder under the cursor, then selects them there. */
async function drop(paths: string[], x: number, y: number): Promise<Dropped> {
  // The files dropped are the ones that count, should they differ from those that entered.
  const current = session ?? begin(paths);
  current.paths = paths;
  current.sources = paths.map(comparable);
  // Where the cursor is now decides, not where it was last seen.
  const destination = targetAt(x, y);
  end();
  if (!destination) return NOTHING;
  try {
    const result = await api.dropItems(paths, destination.path, testKeys);
    if (result.effect !== "none") selectInFolder(destination.path, result.names);
    return result;
  } catch (error) {
    notify(error);
    return NOTHING;
  }
}

/** Listens for files dragged over the window, from Kestrel or any other application. */
export function listenForDrops() {
  return getCurrentWebview().onDragDropEvent(({ payload }) => {
    // A rehearsed drag out passes over whatever the real cursor rests on, which in a test
    // could be this window; its events are noise there, and could even open a folder.
    if (rehearsing) return;
    if (payload.type === "leave") return leave();
    // Tauri reports physical pixels; the page works in CSS pixels.
    const scale = window.devicePixelRatio || 1;
    const [x, y] = [payload.position.x / scale, payload.position.y / scale];
    if (payload.type === "enter") enter(payload.paths, x, y);
    else if (payload.type === "over") over(x, y);
    else void drop(payload.paths, x, y);
  });
}

/** What a drop would do and where, for the hint shown during a drag. */
export function dropHint(): { verb: string; name: string } | null {
  const current = target();
  const verb = { copy: "Copy", move: "Move", link: null, none: null }[effect()];
  if (!current || !verb) return null;
  const name = (current.kind === "place" && current.name) || baseName(current.path) || current.path;
  return { verb, name };
}

export const isDropRow = (path: string | undefined) => {
  const current = target();
  return current?.kind === "row" && path !== undefined && comparable(current.path) === comparable(path);
};
export const isDropPlace = (path: string) => {
  const current = target();
  return current?.kind === "place" && comparable(current.path) === comparable(path);
};
export const isDropTab = (tabId: number) => {
  const current = target();
  return current?.kind === "tab" && current.tabId === tabId;
};
export const isDropFolder = (tabId: number) => {
  const current = target();
  return current?.kind === "folder" && current.tabId === tabId;
};

// ---------- dragging out ----------

/**
 * Follows a press on a row. Once the pointer travels a few pixels with the button still down,
 * `drag` is called; if the button comes up first, `click` is.
 */
export function followPress(start: MouseEvent, handlers: { drag: () => void; click: () => void }) {
  const stop = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", stop);
    window.removeEventListener("blur", stop);
  };
  const move = (event: PointerEvent) => {
    // The button came up somewhere the release wasn't reported.
    if ((event.buttons & 1) === 0) return stop();
    const far = (a: number, b: number) => Math.abs(a - b) > DRAG_THRESHOLD;
    if (!far(event.clientX, start.clientX) && !far(event.clientY, start.clientY)) return;
    stop();
    handlers.drag();
  };
  const up = () => {
    stop();
    handlers.click();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", stop);
  window.addEventListener("blur", stop);
}

/** Drags the rows in `ranges` out as files, to anywhere Windows accepts them. */
export async function dragOut(set: number, ranges: RowRange[]) {
  if (ranges.length === 0) return;
  rehearsing = rehearsal !== undefined;
  try {
    lastDragOut = await api.startDrag(set, ranges, rehearsal);
  } catch (error) {
    notify(error);
  } finally {
    rehearsing = false;
  }
}

// ---------- test hook ----------

// Development builds let the end-to-end tests drive drops with synthetic events, since a real
// mouse drag can't be scripted. Production builds compile this out.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelDragDrop: {
      /** Holds these keys for the synthetic drops that follow; `undefined` reads the keyboard. */
      keys: (keys?: DropKeys) => void (testKeys = keys),
      enter,
      over,
      leave,
      drop,
      target: () => target(),
      hint: () => dropHint(),
      /**
       * Rehearses the drags out that follow: `prepare` builds the files' data object on the
       * window's thread, `cancel` also runs the shell's drag loop but calls it off at once,
       * and `hold` calls it off after a couple of seconds. Nothing can be dropped anywhere.
       * `undefined` drags for real.
       */
      rehearseDragOut: (mode?: DragRehearsal) => {
        rehearsal = mode;
        lastDragOut = null;
      },
      /** How the last drag out ended, since the last call to `rehearseDragOut`. */
      lastDragOut: () => lastDragOut,
    },
  });
}
