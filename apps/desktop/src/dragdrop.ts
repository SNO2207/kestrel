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
import { inArchive } from "./archives";
import { baseName, parentPath } from "./lib/format";
import { locationOf } from "./lib/locations";
import { activateTab, activeTab, isSearch, navigate, notify, selectInFolder, tabs } from "./state";
import { pinPaths } from "./shortcuts";

export type DropTarget =
  /** A folder row in the list on screen; `tabId` is the tab whose list it is in. */
  | { kind: "row"; path: string; tabId?: number }
  /** The folder the list on screen shows: the drop lands on empty space or on a file. */
  | { kind: "folder"; path: string; tabId: number }
  /** A place or drive in the sidebar, with the name it shows there. */
  | { kind: "place"; path: string; name?: string }
  /** A tab, standing for the folder it shows. */
  | { kind: "tab"; path: string; tabId: number }
  /** The sidebar's pinned places, or one of their groups: a drop there pins the items. */
  | { kind: "pin"; path: ""; group: number | null; name: string };

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
const groupOf = (target: DropTarget) => (target.kind === "pin" ? target.group : undefined);
const sameTarget = (a: DropTarget | null, b: DropTarget | null) =>
  a === b ||
  (a !== null &&
    b !== null &&
    a.kind === b.kind &&
    comparable(a.path) === comparable(b.path) &&
    tabIdOf(a) === tabIdOf(b) &&
    groupOf(a) === groupOf(b));

/**
 * Whether the dragged items can be dropped into `path`: not into one of themselves or anything
 * inside one, and not into the folder they are all already in, where a drop does nothing.
 */
function accepts(path: string): boolean {
  // Special locations (This PC, the Recycle Bin, Home) take no files.
  if (!session || locationOf(path)) return false;
  const destination = comparable(path);
  const sources = session.sources;
  if (sources.some((source) => destination === source || destination.startsWith(`${source}\\`))) return false;
  return !sources.every((source) => comparable(parentPath(source) ?? "") === destination);
}

/**
 * Stands for a row or item under the cursor whose data has not arrived yet. A refreshed list
 * fetches its rows again, and in the few milliseconds that takes they carry no path. Such a row
 * is not empty space: taking it for the open folder once sent files dropped on a folder row
 * into the folder around it instead.
 */
const LOADING = Symbol("loading");
/** How long a drop waits for the row under it to arrive before giving up. */
const LOADING_WAIT_MS = 2000;

/** The drop target at a point in the window. */
function targetAt(x: number, y: number): DropTarget | null | typeof LOADING {
  const element = document.elementFromPoint(x, y);
  if (!element) return null;

  const tabElement = element.closest<HTMLElement>("[data-tab-id]");
  if (tabElement) {
    const tab = tabs.find((tab) => tab.id === Number(tabElement.dataset.tabId));
    // The inside of an archive is read-only (archives.ts).
    return tab && !isSearch(tab) && !inArchive(tab.listing) && accepts(tab.path) ? { kind: "tab", path: tab.path, tabId: tab.id } : null;
  }
  const place = element.closest<HTMLElement>("[data-place]");
  if (place) {
    const path = place.dataset.place ?? "";
    return accepts(path) ? { kind: "place", path, name: place.dataset.placeName } : null;
  }
  // Pinned places take anything, to pin it, except over a pinned folder, which takes the
  // files into it like any other place.
  const pins = element.closest<HTMLElement>("[data-pin-zone]");
  if (pins) {
    const group = pins.dataset.pinGroup ? Number(pins.dataset.pinGroup) : null;
    return { kind: "pin", path: "", group, name: pins.dataset.pinZone || "Pinned" };
  }

  // The list on screen: the active tab's, or with two panes either pane's (panes.ts).
  const panel = element.closest<HTMLElement>(".tab-panel.active, .tab-panel.current");
  if (!panel || !element.closest(".filelist")) return null;
  const tab = tabs.find((each) => each.id === Number(panel.dataset.panelTab)) ?? activeTab();
  if (inArchive(tab?.listing)) return null;
  // A details row, or an item of the icon, list and tile views.
  const row = element.closest<HTMLElement>(".row[data-index], .view-item[data-index]");
  if (row && row.dataset.path === undefined) return LOADING;
  if (row && row.dataset.folder !== undefined) {
    // A folder that can't take the drop (one being dragged, say) doesn't pass it on to the
    // folder around it, as in Explorer.
    const path = row.dataset.path ?? "";
    return accepts(path) ? { kind: "row", path, tabId: tab?.id } : null;
  }
  return tab && !isSearch(tab) && accepts(tab.path) ? { kind: "folder", path: tab.path, tabId: tab.id } : null;
}

/**
 * The drop target at a point once the row there, if any, has loaded, or `null` if it doesn't
 * in time: a drop is better refused than sent somewhere the user didn't point at.
 */
async function settledTargetAt(x: number, y: number): Promise<DropTarget | null> {
  const giveUp = performance.now() + LOADING_WAIT_MS;
  for (;;) {
    const found = targetAt(x, y);
    if (found !== LOADING) return found;
    if (performance.now() > giveUp) return null;
    await new Promise((resolve) => setTimeout(resolve, 16));
  }
}

/** Looks again at what is under the cursor, which changes as the list scrolls or a folder opens. */
function retarget() {
  if (!session) return;
  const point = session.point;
  const next = point ? targetAt(point.x, point.y) : null;
  // A row still loading keeps whatever was under the cursor; the next frame looks again.
  if (next === LOADING || sameTarget(next, target())) return;
  setTarget(next);
  setEffect("none");
  session.hintAt = 0;
  clearTimeout(session.spring);
  // Hovering a folder, place or tab opens it; the open folder itself and the pinned places
  // as a whole have nothing to open.
  const opens = next && next.kind !== "folder" && next.kind !== "pin";
  session.spring = opens ? window.setTimeout(() => springOpen(next), SPRING_MS) : undefined;
}

function springOpen(opened: DropTarget) {
  if (!session || !sameTarget(target(), opened)) return;
  if (opened.kind === "tab") return activateTab(opened.tabId);
  // A folder row opens in the pane it is in.
  const tab = (opened.kind === "row" && tabs.find((each) => each.id === opened.tabId)) || activeTab();
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
  // The list under the cursor, which with two panes may be either pane's; else the active one.
  const under = point ? document.elementFromPoint(point.x, point.y)?.closest(".tab-panel.current, .tab-panel.active") : null;
  const viewport = (under ?? document.querySelector(".tab-panel.active"))?.querySelector<HTMLElement>(".list-viewport");
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
  if (!session || !current || current.kind === "pin") return;
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
  // Where the cursor is now decides, not where it was last seen. Nothing opens or scrolls
  // while the row there finishes loading.
  cancelAnimationFrame(current.frame);
  clearTimeout(current.spring);
  const destination = await settledTargetAt(x, y);
  if (session === current) end();
  if (!destination) return NOTHING;
  if (destination.kind === "pin") {
    await pinPaths(paths, destination.group);
    return { effect: "link", names: [] };
  }
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
export function dropHint(): { verb: string; name: string; to?: string } | null {
  const current = target();
  if (current?.kind === "pin") return { verb: "Pin", name: current.name };
  // Alt (or Ctrl and Shift) makes shortcuts, as in Explorer.
  const verb = { copy: "Copy", move: "Move", link: "Create shortcuts", none: null }[effect()];
  if (!current || !verb) return null;
  const name = (current.kind === "place" && current.name) || baseName(current.path) || current.path;
  return { verb, name, to: effect() === "link" ? "in" : undefined };
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
/** Whether dropped files would be pinned to `group` (`null`: above every group). */
export const isDropPin = (group: number | null) => {
  const current = target();
  return current?.kind === "pin" && current.group === group;
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
