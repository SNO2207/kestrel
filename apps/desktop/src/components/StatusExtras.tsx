import { Show, createEffect, createSignal, on, onCleanup } from "solid-js";

import { formatSize } from "../lib/format";
import { parityApi } from "../lib/parityApi";
import { locationKey, setViewMode, viewModeAt, type ViewMode } from "../lib/views";
import { activeTab, selectionOf } from "../state";
import { focusedItem } from "../previewState";
import { Icon } from "./Icon";

/** How long the selection must rest before its size is added up. */
const SETTLE_MS = 150;

/**
 * The size of the selected items, as Explorer's status bar shows it ("3 items selected
 * 1.2 MB"); folders count with the size the index knows.
 */
export function SelectionSize() {
  const [size, setSize] = createSignal<{ bytes: number; complete: boolean } | null>(null);
  let timer: number | undefined;
  let request = 0;
  createEffect(
    on(
      () => [activeTab()?.id, activeTab()?.selected, activeTab()?.revision, focusedItem()?.path] as const,
      ([tabId, count]) => {
        clearTimeout(timer);
        const asked = ++request;
        if (tabId === undefined || !count) return setSize(null);
        timer = window.setTimeout(() => {
          const selection = selectionOf(tabId);
          if (!selection) return setSize(null);
          void parityApi
            .selectionSummary(selection.set, selection.ranges)
            .then((summary) => asked === request && setSize({ bytes: summary.bytes, complete: summary.complete }))
            .catch(() => asked === request && setSize(null));
        }, SETTLE_MS);
      },
    ),
  );
  onCleanup(() => clearTimeout(timer));
  return (
    <Show when={size()}>
      {(known) => (
        <span class="selection-size" title="The selected items' size">
          {known().complete ? "" : "At least "}
          {formatSize(known().bytes)}
        </span>
      )}
    </Show>
  );
}

/** Explorer's two buttons at the right of the status bar: Details and Large icons. */
export function ViewSwitches() {
  const location = () => {
    const tab = activeTab();
    return tab?.listing ? locationKey(tab) : null;
  };
  const mode = () => viewModeAt(location());
  const button = (target: ViewMode, label: string, icon: "view-details" | "view-icons") => (
    <button
      class="status-view"
      classList={{ on: mode() === target }}
      title={label}
      aria-label={label}
      aria-pressed={mode() === target}
      disabled={location() === null}
      onClick={() => {
        const where = location();
        if (where) setViewMode(where, target);
      }}
    >
      <Icon name={icon} size={14} />
    </button>
  );
  return (
    <span class="status-views">
      {button("details", "Details (Ctrl+Shift+6)", "view-details")}
      {button("large", "Large icons (Ctrl+Shift+2)", "view-icons")}
    </span>
  );
}
