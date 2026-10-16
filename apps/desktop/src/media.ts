/**
 * The media library in the front end: where the background pass stands (followed from
 * `media-progress`), pausing and resuming it, and the addresses of its two pages, the Gallery and
 * the library's settings, which `mediaSetup.ts` registers as special locations.
 */
import { createSignal } from "solid-js";

import { formatCount } from "./lib/format";
import { mediaApi, type MediaProgress } from "./lib/mediaApi";
import { notify } from "./state";

/** The Gallery's address, as Explorer's Gallery is called. */
export const GALLERY = "Gallery";
/** The library's settings page. */
export const MEDIA_LIBRARY = "Media library";
/** Explorer's own name for its Gallery. */
export const GALLERY_CLSID = "{e88865ea-0e1c-4e20-9aa6-edcd0212c87c}";

const [progress, setProgress] = createSignal<MediaProgress | null>(null);
export { progress as mediaProgress };

let following = false;

/** Starts following the pass, once; every window does for itself. */
export function followMedia() {
  if (following) return;
  following = true;
  void mediaApi.status().then((status) => setProgress(status.progress)).catch(() => {});
  void mediaApi.onProgress(setProgress);
}

export async function setMediaPaused(paused: boolean) {
  try {
    setProgress(await mediaApi.setPaused(paused));
  } catch (error) {
    notify(error);
  }
}

/** Items read or passed over so far. */
export const handled = (p: MediaProgress) => p.total - p.queued;

const lowerFirst = (text: string) => text.charAt(0).toLowerCase() + text.slice(1);

/** One line saying where the pass stands, for the Gallery's header and the settings page. */
export function progressLine(p: MediaProgress | null): string {
  if (!p) return "";
  const counted = `${formatCount(handled(p))} of ${formatCount(p.total)}`;
  switch (p.state) {
    case "starting":
      return p.total > 0 ? `Up to date: ${formatCount(p.total)} photos and videos` : "Starts in a moment";
    case "idle":
      return p.total > 0 ? `Up to date: ${formatCount(p.total)} photos and videos` : "No photos or videos found";
    case "running":
      return `Reading ${counted}`;
    case "paused":
      return `Paused at ${counted}`;
    case "waiting":
      return `Waiting: ${lowerFirst(p.reason ?? "busy")}`;
  }
}

/** The stages' names as the settings page shows them. */
export const STAGE_LABELS: Record<string, string> = {
  metadata: "Details (dimensions, date taken, camera, place, length)",
  thumbnail: "Thumbnails for the Gallery",
  text: "Text in images",
};
