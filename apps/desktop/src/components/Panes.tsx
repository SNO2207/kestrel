import { For, Show, createSignal } from "solid-js";

import { isDropTab } from "../dragdrop";
import { baseName } from "../lib/format";
import { MAX_SPLIT, MIN_SPLIT, focusPane, openFilter, resetSplit, setSplit, split, toggleSecondPane } from "../panes";
import { activeId, activePane, activeTab, closePane, closeTab, dual, isSearch, openTab, revealTab, shownTab, tabsIn, type Tab } from "../state";
import { Icon } from "./Icon";

const tabTitle = (tab: Tab) => (isSearch(tab) ? `Search: ${tab.query}` : baseName(tab.path) || tab.path);
const KEY_STEP = 0.02;

/**
 * A pane's own tab strip, at its top while two panes are shown; with one pane the title bar
 * holds the tabs. The strip of the pane with the keyboard is marked, and clicking anywhere in a
 * pane gives it the keyboard.
 */
export function PaneBar(props: { pane: number }) {
  const active = () => activePane() === props.pane;
  return (
    <div class="pane-bar" classList={{ active: active() }} data-pane={props.pane} role="tablist" aria-label={props.pane === 0 ? "First pane" : "Second pane"}>
      <div class="pane-tabs">
        <For each={tabsIn(props.pane)}>
          {(tab) => (
            <div
              class="tab pane-tab"
              classList={{ active: tab.id === activeId(), shown: tab.id === shownTab(props.pane), "drop-target": isDropTab(tab.id) }}
              role="tab"
              data-tab-id={tab.id}
              aria-selected={tab.id === shownTab(props.pane)}
              title={isSearch(tab) ? tab.query : tab.path}
              onMouseDown={(event) => {
                if (event.button === 0) {
                  revealTab(tab.id);
                  focusPane(props.pane);
                }
                // Middle-click closes, as in a browser.
                if (event.button === 1) closeTab(tab.id);
              }}
            >
              <span class="tab-title">{tabTitle(tab)}</span>
              <button class="tab-close" aria-label="Close tab" onMouseDown={(event) => event.stopPropagation()} onClick={() => closeTab(tab.id)}>
                <Icon name="close" size={12} />
              </button>
            </div>
          )}
        </For>
        <button
          class="tab-new"
          aria-label="New tab"
          title="New tab (Ctrl+T)"
          onClick={() => {
            openTab(undefined, props.pane);
            focusPane(props.pane);
          }}
        >
          <Icon name="plus" size={14} />
        </button>
      </div>
      <button
        class="pane-close"
        aria-label="Close this pane"
        title="Close this pane"
        onClick={() => (props.pane === 1 ? toggleSecondPane() : closePane(0))}
      >
        <Icon name="close" size={12} />
      </button>
    </div>
  );
}

/** The bar between the two panes: drag it to share the room between them, double-click to halve it. */
export function Splitter() {
  const [dragging, setDragging] = createSignal(false);

  /** The room the panes share: the content area, less the preview pane when it is open. */
  const room = (content: HTMLElement) => {
    const box = content.getBoundingClientRect();
    const preview = content.querySelector(":scope > .preview-pane")?.getBoundingClientRect();
    return { left: box.left, width: (preview ? preview.left : box.right) - box.left };
  };

  const start = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const content = handle.parentElement!;
    handle.setPointerCapture(event.pointerId);
    setDragging(true);
    event.preventDefault();
    const move = (moved: PointerEvent) => {
      const { left, width } = room(content);
      if (width > 0) setSplit((moved.clientX - left) / width);
    };
    const end = () => {
      setDragging(false);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };

  const onKey = (event: KeyboardEvent) => {
    const step = event.key === "ArrowLeft" ? -KEY_STEP : event.key === "ArrowRight" ? KEY_STEP : 0;
    if (step === 0) return;
    event.preventDefault();
    event.stopPropagation();
    setSplit(split() + step);
  };

  return (
    <div
      class="pane-splitter"
      classList={{ dragging: dragging() }}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the panes"
      aria-valuenow={Math.round(split() * 100)}
      aria-valuemin={MIN_SPLIT * 100}
      aria-valuemax={MAX_SPLIT * 100}
      tabindex="0"
      title="Drag to resize the panes. Double-click to share the room equally."
      onPointerDown={start}
      onDblClick={resetSplit}
      onKeyDown={onKey}
    />
  );
}

/** The toolbar's buttons for the second pane (Ctrl+Shift+D) and the filter box (Ctrl+Shift+L). */
export function LayoutButtons() {
  return (
    <>
      <button
        class="toolbar-toggle filter-toggle"
        classList={{ on: (activeTab()?.filter ?? null) !== null }}
        title="Filter by name (Ctrl+Shift+L)"
        aria-label="Filter by name"
        aria-pressed={(activeTab()?.filter ?? null) !== null}
        disabled={!activeTab()}
        onClick={() => {
          const tab = activeTab();
          if (tab) openFilter(tab.id);
        }}
      >
        <svg class="icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true">
          <path d="M2.5 3.5h11l-4.25 5v4l-2.5 1.5v-5.5Z" />
        </svg>
      </button>
      <button
        class="toolbar-toggle pane-toggle"
        classList={{ on: dual() }}
        title="Second pane (Ctrl+Shift+D)"
        aria-label="Second pane"
        aria-pressed={dual()}
        onClick={toggleSecondPane}
      >
        <svg class="icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" aria-hidden="true">
          <path d="M2.5 3.5h11v9h-11ZM8 3.5v9" />
          <Show when={dual()}>
            <path d="M9.5 5.5h2.5M9.5 7.5h2.5M9.5 9.5h2.5" stroke-linecap="round" />
          </Show>
        </svg>
      </button>
    </>
  );
}
