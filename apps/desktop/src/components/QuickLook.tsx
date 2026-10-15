import { Show, createEffect, onCleanup, onMount } from "solid-js";

import { activeId } from "../state";
import { focusedItem, quickLookOpen, setQuickLookOpen, stepFocus } from "../previewState";
import { PreviewBody } from "./PreviewBody";

/** Targets that keep keys for themselves while Quick Look is open, such as a focused video. */
const isMedia = (target: EventTarget | null) => target instanceof HTMLMediaElement;

/**
 * Quick Look: Space on a focused item shows it large in a centred overlay. Space or Escape
 * closes it; the arrow keys move to the previous or next item without closing.
 */
export function QuickLook(props: { focusList: () => void }) {
  let overlay: HTMLDivElement | undefined;

  const close = () => {
    if (!quickLookOpen()) return;
    setQuickLookOpen(false);
    props.focusList();
  };

  onMount(() => {
    // Space reaches the window from the list, which does not use it.
    const onKeyDown = (event: KeyboardEvent) => {
      if (quickLookOpen() || event.key !== " " || event.ctrlKey || event.altKey || event.metaKey) return;
      if (!(event.target instanceof HTMLElement) || !event.target.classList.contains("list-viewport")) return;
      if (!focusedItem()) return;
      event.preventDefault();
      setQuickLookOpen(true);
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  // Nothing to show once the list loses its focused row (it navigated away), or the tab changed.
  createEffect(() => {
    if (quickLookOpen() && !focusedItem()) close();
  });
  createEffect((previous: number | undefined) => {
    const tab = activeId();
    if (previous !== undefined && previous !== tab) close();
    return tab;
  });
  // The overlay holds the focus whenever it opens or moves to another item; an element that
  // had focus inside the previous item's preview is gone, and browsers fire no event for that.
  createEffect(() => {
    if (!quickLookOpen()) return;
    focusedItem();
    queueMicrotask(() => {
      if (overlay && !overlay.contains(document.activeElement)) overlay.focus();
    });
  });

  const onKeyDown = (event: KeyboardEvent) => {
    const key = event.key;
    const handled = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    if (key === "Escape") return handled(), close();
    if (event.ctrlKey || event.altKey || event.metaKey || isMedia(event.target)) return;
    if (key === " ") return handled(), close();
    if (key === "ArrowLeft" || key === "ArrowUp") return handled(), stepFocus(-1);
    if (key === "ArrowRight" || key === "ArrowDown") return handled(), stepFocus(1);
  };

  return (
    <Show when={quickLookOpen() && focusedItem()}>
      <div
        class="quicklook-backdrop"
        onMouseDown={(event) => {
          if (event.target === event.currentTarget) close();
        }}
      >
        <div
          ref={overlay}
          class="quicklook"
          role="dialog"
          aria-modal="true"
          aria-label="Quick Look"
          tabindex="-1"
          onKeyDown={onKeyDown}
          onFocusOut={(event) => {
            // Focus inside the preview is lost when the next item replaces it; take it back so
            // the keys keep working. Focus that moved into the PDF frame is left alone.
            if (event.relatedTarget === null) {
              queueMicrotask(() => {
                if (quickLookOpen() && document.activeElement === document.body) overlay?.focus();
              });
            }
          }}
        >
          <PreviewBody item={focusedItem()} variant="quicklook" onClose={close} closeLabel="Close (Space or Esc)" />
        </div>
      </div>
    </Show>
  );
}
