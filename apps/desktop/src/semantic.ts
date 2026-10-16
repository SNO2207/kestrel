/**
 * AI search in the window: Describe mode in the search box (`~` queries), the AI search page
 * (a special location, `AI_SEARCH`, registered in semanticSetup.ts), "Find similar" in an
 * image's or video's menu, and the backend's state, which the page and the describe bar share.
 *
 * Describe mode is the search box's toggle: while it is on, what is typed is sent as a
 * description (`~` and the text), and the box shows the text without the `~`. A query that is
 * already a description (typed with `~`, or with a `like:` clause, or from a saved search) is
 * sent as it is. The mode lasts until the app closes: left on across a restart, every search
 * would quietly be a description.
 */
import { createSignal } from "solid-js";

import type { MenuEntry } from "./components/ContextMenu";
import type { Row } from "./lib/api";
import { mediaApi } from "./lib/mediaApi";
import { semanticApi, type SemanticState } from "./lib/semanticApi";
import { activeTab, openSearch, setQuery } from "./state";

/** The AI search page's address. */
export const AI_SEARCH = "kestrel:ai-search";


const [describing, setDescribingSignal] = createSignal(false);
export { describing };

/** Whether a query asks for photos and videos by description. */
export const isDescribeQuery = (query: string) => /^\s*~/.test(query) || /(^|\s)!?like:/i.test(query);

/** What the search box shows for a query: in Describe mode, the description without its `~`. */
export const boxText = (query: string) => (describing() ? query.replace(/^\s*~\s?/, "") : query);

/** The query for what is typed in the search box. */
export const queryFor = (typed: string) => (describing() && typed.trim() !== "" && !isDescribeQuery(typed) ? `~${typed}` : typed);

/** Turns Describe mode on or off, carrying the search being typed over. */
export function setDescribing(on: boolean) {
  const tab = activeTab();
  const typed = tab ? boxText(tab.query) : "";
  setDescribingSignal(on);
  if (tab && typed.trim() !== "") setQuery(tab.id, on ? queryFor(typed) : typed.replace(/^\s*~\s?/, ""));
}

// ---------- the backend's state ----------

const [state, setState] = createSignal<SemanticState | null>(null);
export { state as semanticState };

let listening = false;

/** Asks the backend for its state again. */
export async function refreshSemantic() {
  const next = await semanticApi.state().catch(() => null);
  if (next) setState(next);
}

let refreshDue: number | undefined;

/** Keeps `semanticState` current from now on: the describing is the media library's pass, so
 * its progress (and its Pause, wherever pressed) changes the state too. */
export function watchSemantic() {
  if (!listening) {
    listening = true;
    void semanticApi.onChange(() => void refreshSemantic());
    void mediaApi.onProgress(() => {
      if (refreshDue !== undefined) return;
      refreshDue = window.setTimeout(() => {
        refreshDue = undefined;
        void refreshSemantic();
      }, 250);
    });
  }
  void refreshSemantic();
}

// ---------- Find similar ----------

const IMAGES = ["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "webp", "heic", "heif", "avif", "tif", "tiff", "jxl"];
const VIDEOS = ["mp4", "m4v", "mov", "avi", "wmv", "mkv", "webm", "3gp", "3g2", "mts", "m2ts"];

/** Whether AI search describes files of this extension. */
export const isMedia = (ext: string) => IMAGES.includes(ext.toLowerCase()) || VIDEOS.includes(ext.toLowerCase());

/** Searches everywhere for photos and videos that look like the one at `path`. */
export function findSimilar(tabId: number, path: string) {
  openSearch(tabId, `like:"${path}"`, null);
}

/** The item menu's AI search entry, for a single photo or video. */
export function similarEntries(tabId: number, sole: Row | undefined): MenuEntry[] {
  if (!sole || sole.dir || !isMedia(sole.ext)) return [];
  return [{ label: "Find similar", run: () => findSimilar(tabId, sole.path) }];
}
