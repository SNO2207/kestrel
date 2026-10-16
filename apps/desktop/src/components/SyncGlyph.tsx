import { Match, Show, Switch } from "solid-js";

import type { Row } from "../lib/api";
import { SYNC_LABELS, syncState } from "../lib/syncStatus";

/**
 * An item's sync status beside its name, in every view, inside OneDrive and other synced
 * folders: a cloud for online-only, a ringed check for available on this device, a filled
 * check for always available.
 */
export function SyncGlyph(props: { row: Pick<Row, "path" | "dir" | "attrs"> | undefined }) {
  const state = () => syncState(props.row);
  return (
    <Show when={state()}>
      {(current) => (
        <svg
          class="sync-glyph"
          classList={{ [current()]: true }}
          width="14"
          height="14"
          viewBox="0 0 14 14"
          role="img"
          aria-label={SYNC_LABELS[current()]}
          data-sync={current()}
        >
          <title>{SYNC_LABELS[current()]}</title>
          <Switch>
            <Match when={current() === "cloud"}>
              <path
                d="M4.2 10.5h6a2.3 2.3 0 0 0 .3-4.6 3.3 3.3 0 0 0-6.3-.8A2.7 2.7 0 0 0 4.2 10.5Z"
                fill="none"
                stroke="currentColor"
                stroke-width="1.2"
                stroke-linejoin="round"
              />
            </Match>
            <Match when={current() === "local"}>
              <circle cx="7" cy="7" r="5.2" fill="none" stroke="currentColor" stroke-width="1.2" />
              <path d="M4.6 7.1 6.3 8.7 9.4 5.5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" />
            </Match>
            <Match when={current() === "pinned"}>
              <circle cx="7" cy="7" r="5.8" fill="currentColor" />
              <path d="M4.6 7.1 6.3 8.7 9.4 5.5" fill="none" stroke="var(--surface)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
            </Match>
          </Switch>
        </svg>
      )}
    </Show>
  );
}
