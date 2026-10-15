import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { shortcutsApi, type GotoHit } from "../lib/shortcutsApi";
import { folderName, openPlace } from "../shortcuts";
import { ShortcutIcon } from "./ShortcutIcon";

/** Typing pauses this long before folders are looked for again. */
const TYPING_MS = 40;

const [open, setOpen] = createSignal(false);

/** Opens Go to folder (Ctrl+G). */
export const openGoto = () => setOpen(true);
export const gotoOpen = open;

/**
 * Go to folder: type part of a folder's name and press Enter. The folders visited most come
 * first, then every folder in the index.
 */
export function GoToFolder(props: { onClose: () => void }) {
  return (
    <Show when={open()}>
      <Finder
        onClose={() => {
          setOpen(false);
          props.onClose();
        }}
      />
    </Show>
  );
}

function Finder(props: { onClose: () => void }) {
  let input!: HTMLInputElement;
  let list: HTMLDivElement | undefined;
  const [query, setQuery] = createSignal("");
  const [hits, setHits] = createSignal<GotoHit[]>([]);
  const [active, setActive] = createSignal(0);
  const [searched, setSearched] = createSignal<string | null>(null);

  let request = 0;
  createEffect(() => {
    const text = query();
    const id = ++request;
    const timer = window.setTimeout(async () => {
      try {
        const found = await shortcutsApi.goto(text);
        if (id !== request) return;
        setHits(found);
        setActive(0);
        setSearched(text);
      } catch {
        if (id === request) setHits([]);
      }
    }, text ? TYPING_MS : 0);
    onCleanup(() => clearTimeout(timer));
  });

  createEffect(on(active, () => list?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" })));

  onMount(() => input.focus());

  const choose = (hit: GotoHit | undefined, inNewTab: boolean) => {
    if (!hit) return;
    props.onClose();
    openPlace(hit.path, true, inNewTab);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // Everything typed here is for the finder, not the list behind it.
    event.stopPropagation();
    const count = hits().length;
    if (event.key === "Escape") {
      event.preventDefault();
      props.onClose();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (count > 0) setActive((at) => (at + (event.key === "ArrowDown" ? 1 : -1) + count) % count);
    } else if (event.key === "Enter") {
      event.preventDefault();
      choose(hits()[active()], event.ctrlKey);
    } else if (event.key === "Tab") {
      event.preventDefault();
    } else if (event.ctrlKey && event.key.toLowerCase() === "g") {
      event.preventDefault();
      props.onClose();
    }
  };

  return (
    <div
      class="goto-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div class="goto" role="dialog" aria-modal="true" aria-label="Go to folder" onKeyDown={onKeyDown}>
        <div class="goto-field">
          <ShortcutIcon name="folder" />
          <input
            ref={input}
            role="combobox"
            aria-expanded="true"
            aria-controls="goto-results"
            aria-activedescendant={hits().length > 0 ? `goto-hit-${active()}` : undefined}
            aria-label="Go to folder"
            placeholder="Go to folder"
            spellcheck={false}
            value={query()}
            onInput={(event) => setQuery(event.currentTarget.value)}
          />
        </div>
        <div ref={list} id="goto-results" class="goto-results" role="listbox" aria-label="Folders">
          <For each={hits()}>
            {(hit, index) => (
              <div
                id={`goto-hit-${index()}`}
                class="goto-hit"
                classList={{ active: active() === index() }}
                role="option"
                aria-selected={active() === index()}
                title={hit.path}
                onMouseMove={() => setActive(index())}
                onMouseDown={(event) => event.preventDefault()}
                onClick={(event) => choose(hit, event.ctrlKey)}
              >
                <ShortcutIcon name={hit.frecent ? "recent" : "folder"} />
                <span class="goto-name">{folderName(hit.path)}</span>
                <span class="goto-path">{hit.path}</span>
              </div>
            )}
          </For>
          <Show when={hits().length === 0 && searched() !== null}>
            <p class="goto-empty">
              {query().trim() ? `No folder matches “${query().trim()}”.` : "Folders you visit will be offered here. Type to look through every folder."}
            </p>
          </Show>
        </div>
        <div class="goto-footer">
          <span>
            <kbd>Enter</kbd> opens here
          </span>
          <span>
            <kbd>Ctrl+Enter</kbd> opens a new tab
          </span>
        </div>
      </div>
    </div>
  );
}
