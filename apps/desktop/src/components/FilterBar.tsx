import { Show, createEffect, on, onMount } from "solid-js";

import { formatCount } from "../lib/format";
import { filterFocus } from "../panes";

/**
 * The filter box, above a tab's list: it narrows the listing to the names containing what is
 * typed (or matching it, with `*` and `?`), as it is typed, without leaving the folder. The
 * engine does the narrowing over the whole listing (`filter_set` in layout.rs). Esc clears and
 * closes it; Enter or Down hands the keyboard to the list.
 */
export function FilterBar(props: {
  text: string;
  /** Items shown, and how many there are without the filter. */
  shown: number;
  of: number | null;
  onChange: (text: string) => void;
  onClose: () => void;
  onDone: () => void;
}) {
  let input!: HTMLInputElement;
  const focus = () => {
    input.focus();
    input.select();
  };
  onMount(focus);
  // Ctrl+Shift+L, or the toolbar's button, on a box already open puts the keyboard back in the
  // active tab's box.
  createEffect(on(filterFocus, () => input.closest(".tab-panel.active") && focus(), { defer: true }));

  return (
    <div class="filter-bar" role="search">
      <svg class="icon" width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true">
        <path d="M2.5 3.5h11l-4.25 5v4l-2.5 1.5v-5.5Z" />
      </svg>
      <input
        ref={input}
        aria-label="Filter by name"
        placeholder="Filter by name"
        spellcheck={false}
        value={props.text}
        onInput={(event) => props.onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            props.onClose();
          } else if (event.key === "Enter" || event.key === "ArrowDown") {
            event.preventDefault();
            props.onDone();
          }
        }}
      />
      <Show when={props.text.trim() !== "" && props.of !== null}>
        <span class="filter-count">
          {formatCount(props.shown)} of {formatCount(props.of ?? 0)}
        </span>
      </Show>
      <button class="filter-close" aria-label="Clear and close the filter" title="Clear and close the filter (Esc)" onClick={() => props.onClose()}>
        <svg class="icon" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">
          <path d="M4 4l8 8M12 4l-8 8" />
        </svg>
      </button>
    </div>
  );
}
