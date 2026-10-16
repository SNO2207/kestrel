/**
 * The preview's own webview, seen from the page. Images, video, audio and PDF documents are
 * shown in a second webview with no access to any command (`previewview.rs`), laid over the
 * part of the pane or of Quick Look where the content belongs.
 *
 * Each preview that shows such content claims the webview for an element of its own; the
 * claim with the highest priority (Quick Look over the pane) gets it. Every frame while a
 * claim stands, the element's place on the page is measured and, when it or the address has
 * changed, sent to the backend, so the webview follows resizing, scrolling and animation. The
 * webview is drawn above the page, so it steps aside while a menu, a dialog or a notice of the
 * app's own lies over it.
 */
import { createSignal } from "solid-js";

import { api, type SurfacePlacement } from "./api";
import { previewUrl } from "./preview";

/**
 * The app's own things that can lie over the preview area: menus, dialogs, notices, Quick
 * Look over the pane when what it shows has no use for the webview, the command palette, Go to
 * folder, the projects and disk usage hubs, which cover the pane while they are open, the
 * address bar's suggestions, and the panel of running archive jobs (the archive dialogs are
 * dialogs).
 */
const OVERLAYS =
  ".menu, .dialog-backdrop, .notice, .quicklook-backdrop, .palette-backdrop, .goto-backdrop, .projects-hub, .disk-hub, .address-suggest, .archive-jobs";

export type SurfaceKind = "image" | "video" | "audio" | "pdf";

interface Claim {
  element: HTMLElement;
  url: string;
  priority: number;
  kind: SurfaceKind;
}

const claims = new Map<number, Claim>();
let nextClaim = 1;
let frame = 0;
/** The placement last sent, as JSON; `undefined` before anything was sent. */
let sent: string | undefined;
let sending = false;

const light = window.matchMedia("(prefers-color-scheme: light)");
const [scheme, setScheme] = createSignal<"light" | "dark">(light.matches ? "light" : "dark");
light.addEventListener("change", () => setScheme(light.matches ? "light" : "dark"));

/**
 * The address the preview webview shows a file at: a PDF as itself, in the browser's viewer;
 * an image, a video or a sound in the protocol's viewer page, on the window's colours, so it
 * reads `scheme` and follows the theme.
 */
export function surfaceUrl(kind: SurfaceKind, path: string, version: number, download: boolean): string {
  if (kind === "pdf") return previewUrl(path, version, download);
  const theme = scheme();
  const style = getComputedStyle(document.documentElement);
  const colour = (name: string) => encodeURIComponent(style.getPropertyValue(name).trim());
  const parts = [
    `kind=${kind}`,
    `p=${encodeURIComponent(path)}`,
    `v=${version}`,
    `scheme=${theme}`,
    `bg=${colour("--surface")}`,
    `raised=${colour("--raised")}`,
    `line=${colour("--line")}`,
    `chrome=${colour("--chrome")}`,
  ];
  if (download) parts.push("download=1");
  return `http://preview.localhost/view?${parts.join("&")}`;
}

function owner(): Claim | null {
  let best: [number, Claim] | null = null;
  for (const entry of claims) {
    if (!best || entry[1].priority > best[1].priority || (entry[1].priority === best[1].priority && entry[0] > best[0])) best = entry;
  }
  return best?.[1] ?? null;
}

const overlaps = (a: DOMRect, b: DOMRect) => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

/** Where the webview should be now, or `null` when nothing claims it. */
function placement(): SurfacePlacement | null {
  const claim = owner();
  if (!claim) return null;
  const box = claim.element.getBoundingClientRect();
  // An overlay the surface itself is part of, such as Quick Look's own, doesn't count.
  const covered = [...document.querySelectorAll(OVERLAYS)].some(
    (overlay) => !overlay.contains(claim.element) && overlaps(overlay.getBoundingClientRect(), box),
  );
  const shown = claim.element.isConnected && box.width >= 1 && box.height >= 1 && !covered;
  return { url: claim.url, x: box.left, y: box.top, width: box.width, height: box.height, visible: shown };
}

/** Sends the current placement if it changed, one request at a time. */
function sync() {
  if (sending) return;
  const next = placement();
  const text = JSON.stringify(next);
  if (text === sent) return;
  sent = text;
  sending = true;
  api
    .previewSurface(next)
    .catch((error) => console.error("preview view:", error))
    .finally(() => {
      sending = false;
      // Anything that changed meanwhile goes out now.
      sync();
    });
}

function tick() {
  sync();
  frame = claims.size > 0 ? requestAnimationFrame(tick) : 0;
}

function wake() {
  if (frame === 0) frame = requestAnimationFrame(tick);
}

/**
 * Claims the preview webview for `element`, showing `url`; a higher `priority` wins. Returns
 * functions to change the address and to give the claim up.
 */
export function claimSurface(element: HTMLElement, url: string, priority: number, kind: SurfaceKind) {
  const id = nextClaim++;
  claims.set(id, { element, url, priority, kind });
  wake();
  return {
    setUrl(next: string) {
      const claim = claims.get(id);
      if (claim) claim.url = next;
      wake();
    },
    release() {
      claims.delete(id);
      // One last pass hands the webview to the next claim, or empties it.
      wake();
    },
  };
}

// A click into the preview webview takes the keyboard there. An image has nothing to use it
// for, so the page asks for it back, and the list's keys and Quick Look's go on working. Video,
// sound and PDF documents keep it: their controls need it.
window.addEventListener("blur", () => {
  if (owner()?.kind !== "image") return;
  // The window may be losing the foreground altogether, which the backend checks once it has.
  setTimeout(() => {
    if (owner()?.kind === "image" && !document.hasFocus()) void api.previewSurfaceReturnFocus().catch(() => {});
  }, 50);
});

// A reload of the page leaves the webview showing whatever it showed before; with nothing
// claimed yet, this empties it.
sync();
