import { For, Show, createSignal } from "solid-js";

import { formatCount } from "../lib/format";
import type { Chip, ContentStatus } from "../lib/searchApi";
import { readingOf, searchClauses, searchOptions, searchWordsAsTyped, setSearchOptions, stopSearch } from "../search";
import { SearchIcon } from "./SearchIcon";

const plural = (count: number, one: string, many: string) => `${formatCount(count)} ${count === 1 ? one : many}`;

/** What a search inside files did, in words. */
export function contentSummary(status: ContentStatus, limitMb: number): { main: string; skipped: string } {
  const where = status.place ? ` in ${status.place}` : "";
  const read = plural(status.searched, "file", "files");
  const main = !status.done
    ? `Searching inside files${where}: ${read} read, ${formatCount(status.matched)} found so far.`
    : status.full
      ? `Stopped after ${plural(status.matched, "match", "matches")}${where}; ${read} read.`
      : `${plural(status.matched, "file matches", "files match")}${where}; ${read} read.`;
  const skipped: string[] = [];
  if (status.binary > 0) skipped.push(`${plural(status.binary, "binary file", "binary files")} skipped`);
  if (status.too_large > 0) skipped.push(`${formatCount(status.too_large)} over ${limitMb >= 1000 ? `${limitMb / 1000} GB` : `${limitMb} MB`} skipped`);
  if (status.cloud > 0) skipped.push(`${plural(status.cloud, "online-only file", "online-only files")} not downloaded`);
  if (status.ignored > 0) skipped.push(`${formatCount(status.ignored)} ignored by .gitignore`);
  if (status.unreadable > 0) skipped.push(`${formatCount(status.unreadable)} couldn't be read`);
  return { main, skipped: skipped.join(", ") };
}

/** One chip: what a clause means, and the clause itself to edit. */
function SearchChip(props: { chip: Chip; onChange: (text: string) => void; onRemove: () => void }) {
  const [editing, setEditing] = createSignal(false);
  /** The text edited: the clause, or for the text looked for inside files, that text. */
  const source = () => props.chip.clause || props.chip.phrase;
  let done = false;
  const finish = (text: string | null) => {
    if (done) return;
    done = true;
    setEditing(false);
    if (text !== null && text.trim() !== source()) props.onChange(text.trim());
  };
  return (
    <span
      class="search-chip"
      // A data attribute, not a class: kinds such as "place" are class names elsewhere.
      data-kind={props.chip.kind}
      data-clause={props.chip.clause}
      title={props.chip.clause ? `${props.chip.clause}\nClick to change` : "Click to change"}
    >
      <Show
        when={editing()}
        fallback={
          <button
            class="search-chip-label"
            onClick={() => {
              done = false;
              setEditing(true);
            }}
          >
            {props.chip.label}
          </button>
        }
      >
        <input
          class="search-chip-input"
          aria-label={`Change “${props.chip.label}”`}
          spellcheck={false}
          value={source()}
          size={Math.max(6, source().length + 1)}
          ref={(input) =>
            queueMicrotask(() => {
              input.focus();
              input.select();
            })
          }
          onBlur={(event) => finish(event.currentTarget.value)}
          onKeyDown={(event) => {
            // Keys typed here are the chip's, not the list's.
            event.stopPropagation();
            if (event.key === "Enter") finish(event.currentTarget.value);
            if (event.key === "Escape") finish(null);
          }}
        />
      </Show>
      <button class="search-chip-remove" aria-label={`Remove “${props.chip.label}”`} title="Remove" onClick={() => props.onRemove()}>
        <SearchIcon name="close" size={10} />
      </button>
    </span>
  );
}

/**
 * Above search results: how the search box's text was read, as chips that can be changed (click
 * one to edit its clause) or removed, and how far a search inside files got.
 */
export function SearchBanner(props: { tabId: number }) {
  const reading = () => readingOf(props.tabId);
  const shown = () => {
    const read = reading();
    return !!read && (read.natural || read.content !== null || read.chips.some((chip) => chip.kind !== "name"));
  };
  /** The clauses the chips stand for, with one replaced or left out. */
  const clauses = (index: number, replacement: string | null): string[] =>
    (reading()?.chips ?? []).flatMap((chip, i) => {
      if (i === index) return replacement === null ? [] : [replacement];
      return [chip.clause || chip.phrase];
    });
  const change = (index: number, text: string | null) => {
    const chips = reading()?.chips ?? [];
    // The text looked for inside files: removing it searches names again.
    if (chips[index]?.kind === "content" && chips[index]?.clause === "" && text === null) setSearchOptions({ inside: false });
    searchClauses(props.tabId, clauses(index, text));
  };
  return (
    <Show when={shown() && reading()}>
      {(read) => (
        <div class="search-banner" role="region" aria-label="How the search was read">
          <Show when={read().chips.length > 0}>
            <div class="search-chips">
              <span class="search-banner-lead">{read().natural ? "Searching for" : read().content ? "Looking for" : "Filters"}</span>
              <For each={read().chips}>{(chip, i) => <SearchChip chip={chip} onChange={(text) => change(i(), text)} onRemove={() => change(i(), null)} />}</For>
              <Show when={read().natural}>
                <button class="search-banner-link" onClick={() => searchWordsAsTyped(props.tabId)}>
                  Search the words as typed
                </button>
              </Show>
            </div>
          </Show>
          <Show when={read().content}>
            {(status) => {
              const summary = () => contentSummary(status(), searchOptions().max_size_mb);
              return (
                <div class="search-progress" classList={{ running: !status().done }} role="status" aria-live="polite">
                  <SearchIcon name="inside" size={14} />
                  <span class="search-progress-main">{summary().main}</span>
                  <Show when={summary().skipped}>
                    <span class="search-progress-skipped">{summary().skipped}</span>
                  </Show>
                  <Show when={!status().done}>
                    <button class="search-banner-link" onClick={() => stopSearch(props.tabId)}>
                      Stop
                    </button>
                  </Show>
                </div>
              );
            }}
          </Show>
        </div>
      )}
    </Show>
  );
}
