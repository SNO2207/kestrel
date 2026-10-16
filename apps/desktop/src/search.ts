/**
 * The search box's searches: phrases in plain words, read into clauses shown as chips the owner
 * can edit, and searches inside files, whose matches stream into the results list.
 *
 * Every search goes through `search_run` (`src-tauri/src/search/`), registered here as the tabs'
 * search source (`setSearchSource` in state.ts). A search inside files keeps running after the
 * first answer; its progress arrives as `search-progress`, and each step shows what it found so
 * far in place of the results on screen (`updateSearch`), the selection kept. A tab that
 * leaves its search, or closes, stops its search inside files.
 *
 * The options ("Search inside files", regular expressions, case, whole words, .gitignore, the
 * size limit) are the window's, remembered in local storage.
 */
import { createEffect, createRoot, createSignal } from "solid-js";

import type { Listing, SortSpec } from "./lib/api";
import { locationOf } from "./lib/locations";
import { searchApi, type Chip, type ContentStatus, type SearchOptions } from "./lib/searchApi";
import { activeTab, isSearch, refresh, setQuery, setSearchSource, tabs, updateSearch } from "./state";

const OPTIONS_KEY = "kestrel.search-options";
const DEFAULT_OPTIONS: SearchOptions = { inside: false, regex: false, match_case: false, whole_word: false, gitignore: true, max_size_mb: 50 };
/** The size limits offered, in megabytes. */
export const SIZE_LIMITS = [1, 10, 50, 200, 1000];

function storedOptions(): SearchOptions {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(OPTIONS_KEY) ?? "null");
    if (stored && typeof stored === "object") return { ...DEFAULT_OPTIONS, ...(stored as Partial<SearchOptions>) };
  } catch {
    // Unreadable or unavailable: the defaults stand.
  }
  return { ...DEFAULT_OPTIONS };
}

const [options, setOptionsSignal] = createSignal<SearchOptions>(storedOptions());
export { options as searchOptions };

/** Changes the options and searches again with them, if the tab on screen is searching. */
export function setSearchOptions(change: Partial<SearchOptions>) {
  const next = { ...options(), ...change };
  setOptionsSignal(next);
  try {
    localStorage.setItem(OPTIONS_KEY, JSON.stringify(next));
  } catch {
    // Not remembered, which is harmless.
  }
  const tab = activeTab();
  if (tab && tab.query.trim() !== "") refresh(tab.id);
}

/** How a tab's search text was read, and how far its search inside files got. */
export interface SearchReading {
  /** The text it was read from. */
  text: string;
  natural: boolean;
  /** The query that ran. */
  query: string;
  chips: Chip[];
  content: ContentStatus | null;
}

const [readings, setReadings] = createSignal(new Map<number, SearchReading>(), { equals: false });

/** How the tab's search was read, while it shows search results. */
export const readingOf = (tabId: number): SearchReading | null => readings().get(tabId) ?? null;

function setReading(tabId: number, reading: SearchReading | null) {
  const map = readings();
  if (reading) map.set(tabId, reading);
  else map.delete(tabId);
  setReadings(map);
}

/** Tabs with a search inside files that may still be running. */
const reading = new Set<number>();

const folderOf = (tabId: number): string | null => {
  const tab = tabs.find((each) => each.id === tabId);
  return tab && !locationOf(tab.path) ? tab.path : null;
};

async function run(tabId: number, query: string, scope: string | null, sort: SortSpec, update: boolean): Promise<Listing> {
  let answer;
  try {
    answer = await searchApi.run({ tab: tabId, text: query, scope, folder: folderOf(tabId), sort, options: options(), update });
  } catch (error) {
    // The tab shows why (an invalid regular expression, say), not the last search's chips.
    reading.delete(tabId);
    setReading(tabId, null);
    throw error;
  }
  if (answer.content) reading.add(tabId);
  else reading.delete(tabId);
  setReading(tabId, { text: query, natural: answer.natural, query: answer.query, chips: answer.chips, content: answer.content });
  return answer.listing;
}

/**
 * Replaces the tab's search with these clauses, as when a chip is edited or removed. A phrase
 * becomes the clauses it was read as, which then show as chips of their own.
 */
export function searchClauses(tabId: number, clauses: string[]) {
  setQuery(
    tabId,
    clauses
      .map((clause) => clause.trim())
      .filter(Boolean)
      .join(" "),
  );
}

/** Searches for the words as typed, each one literal, rather than as a phrase. */
export function searchWordsAsTyped(tabId: number) {
  const text = readingOf(tabId)?.text ?? "";
  const words = text.split(/\s+/).filter(Boolean);
  searchClauses(tabId, words.map((word) => (word.includes('"') || word.includes(":") ? word : `"${word}"`)));
}

let started = false;

/** Becomes the tabs' search source and follows searches inside files. Call once, at startup. */
export function startSearch() {
  if (started) return;
  started = true;
  setSearchSource(run);
  void searchApi.onProgress((tabId, status) => {
    const known = readingOf(tabId);
    if (known) setReading(tabId, { ...known, content: status });
    const tab = tabs.find((each) => each.id === tabId);
    // Only a tab still searching takes what was found.
    if (tab && tab.query.trim() !== "") updateSearch(tabId);
  });
  createRoot(() => {
    // A tab that leaves its search, or closes, stops its search inside files.
    createEffect(() => {
      const searching = new Map(tabs.map((tab) => [tab.id, tab.query.trim() !== ""] as const));
      for (const tabId of [...reading]) {
        if (searching.get(tabId) === true) continue;
        reading.delete(tabId);
        setReading(tabId, null);
        void searchApi.cancel(tabId).catch(() => {});
      }
      for (const tabId of readings().keys()) {
        if (searching.get(tabId) !== true) setReading(tabId, null);
      }
    });
  });
  // Development builds let the end-to-end tests see how searches were read.
  if (import.meta.env.DEV) {
    Object.assign(window, {
      __kestrelSearch: {
        reading: (tabId: number) => readingOf(tabId),
        options: () => options(),
        setOptions: (change: Partial<SearchOptions>) => setSearchOptions(change),
      },
    });
  }
}

/** Stops the tab's search inside files where it got to; what it found stays shown. */
export function stopSearch(tabId: number) {
  // Kept, so that an update on its way shows what it found rather than starting it again.
  void searchApi.cancel(tabId, false).catch(() => {});
  const known = readingOf(tabId);
  if (known?.content) setReading(tabId, { ...known, content: { ...known.content, done: true } });
}

/** Whether the tab shows results of a search inside files that is still running. */
export const stillSearching = (tabId: number) => {
  const tab = tabs.find((each) => each.id === tabId);
  const content = readingOf(tabId)?.content;
  return !!tab && isSearch(tab) && !!content && !content.done;
};
