import { Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";

import type { PreviewItem } from "../lib/preview";
import {
  DEFAULT_PANE_WIDTH,
  MIN_PANE_WIDTH,
  paneItem,
  paneOpen,
  paneWidth,
  quickLookOpen,
  setPaneOpen,
  setPaneWidth,
  togglePane,
} from "../previewState";
import { PreviewBody } from "./PreviewBody";

/** Room always left for the file list beside the pane. */
const LIST_MIN_WIDTH = 300;
const KEY_STEP = 16;
/** How long the list's focus must rest on an item before the pane loads it. */
const SETTLE_MS = 90;

/**
 * The preview pane, to the right of the file list. It shows the focused item, or the folder
 * itself when nothing is focused. Alt+P or the toolbar button toggles it; dragging its left
 * edge resizes it, and the width is remembered.
 */
export function PreviewPane() {
  const [dragging, setDragging] = createSignal(false);
  // While Quick Look is open the pane stays as it was, rather than loading every item a
  // second time behind the overlay; it catches up when Quick Look closes.
  const item = createMemo((previous: PreviewItem | null) => (quickLookOpen() ? previous : paneItem()), null);

  // The width lives in a variable on the root, which both the pane and the tab panels
  // beside it read (see the preview section of styles.css).
  createEffect(() => document.documentElement.style.setProperty("--preview-width", `${paneWidth()}px`));

  onMount(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.altKey && !event.ctrlKey && !event.metaKey && event.key.toLowerCase() === "p") {
        event.preventDefault();
        togglePane();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    onCleanup(() => window.removeEventListener("keydown", onKeyDown));
  });

  /** The widest the pane may be while leaving the list usable. */
  const maxWidth = (pane: HTMLElement) =>
    Math.max(MIN_PANE_WIDTH, (pane.parentElement?.clientWidth ?? window.innerWidth) - LIST_MIN_WIDTH);

  const startDrag = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const pane = handle.parentElement!;
    const startX = event.clientX;
    const startWidth = pane.getBoundingClientRect().width;
    handle.setPointerCapture(event.pointerId);
    setDragging(true);
    event.preventDefault();

    const move = (moved: PointerEvent) => {
      const width = Math.min(maxWidth(pane), startWidth + (startX - moved.clientX));
      setPaneWidth(width, false);
    };
    const end = () => {
      setDragging(false);
      setPaneWidth(paneWidth(), true);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };

  const onHandleKey = (event: KeyboardEvent) => {
    const pane = (event.currentTarget as HTMLElement).parentElement!;
    const step = event.key === "ArrowLeft" ? KEY_STEP : event.key === "ArrowRight" ? -KEY_STEP : 0;
    if (step === 0) return;
    event.preventDefault();
    event.stopPropagation();
    setPaneWidth(Math.min(maxWidth(pane), paneWidth() + step), true);
  };

  return (
    <Show when={paneOpen()}>
      <aside class="preview-pane" classList={{ dragging: dragging() }} aria-label="Preview">
        <div
          class="preview-resizer"
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize the preview pane"
          aria-valuenow={paneWidth()}
          aria-valuemin={MIN_PANE_WIDTH}
          tabindex="0"
          title="Drag to resize. Double-click to reset."
          onPointerDown={startDrag}
          onDblClick={() => setPaneWidth(DEFAULT_PANE_WIDTH, true)}
          onKeyDown={onHandleKey}
        />
        <PreviewBody item={item()} variant="pane" settle={SETTLE_MS} onClose={() => setPaneOpen(false)} closeLabel="Close the preview pane (Alt+P)" />
      </aside>
    </Show>
  );
}

/** The toolbar's button for the preview pane. */
export function PreviewToggle() {
  return (
    <button
      class="preview-toggle"
      classList={{ on: paneOpen() }}
      title="Preview pane (Alt+P)"
      aria-label="Preview pane"
      aria-pressed={paneOpen()}
      onClick={togglePane}
    >
      <svg class="icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true">
        <path d="M2.5 3.5h11v9h-11ZM9.5 3.5v9" />
      </svg>
    </button>
  );
}
