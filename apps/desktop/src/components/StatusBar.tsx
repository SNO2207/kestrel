import { Show } from "solid-js";

import { formatCount, formatDuration } from "../lib/format";
import { showsHome } from "../lib/home";
import { activeTab, isSearch, status } from "../state";
import { SelectionSize, ViewSwitches } from "./StatusExtras";

export function StatusBar() {
  const tab = activeTab;
  const count = () => tab()?.listing?.total ?? 0;
  const noun = () => {
    const current = tab();
    const plural = count() === 1 ? "" : "s";
    return current && isSearch(current) ? `result${plural}` : `item${plural}`;
  };

  return (
    <footer class="statusbar">
      {/* Home lists no folder, so it has no count of items. */}
      <Show when={!(tab() && showsHome(tab()!))}>
        <span>
          {formatCount(count())} {noun()}
        </span>
      </Show>
      <Show when={(tab()?.selected ?? 0) > 0}>
        <span>{formatCount(tab()!.selected)} selected</span>
        <SelectionSize />
      </Show>
      <Show when={tab()?.listing}>
        {(listing) => (
          <span class="timing" title="Time the engine took to produce this view">
            {formatDuration(listing().took_us)}
          </span>
        )}
      </Show>
      <span class="spacer" />
      <span class="index-state" classList={{ busy: status().state !== "ready" || status().refreshing }}>
        <Show when={status().state === "ready"} fallback={status().state === "scanning" ? "Building the index for the first time…" : "Loading index…"}>
          {formatCount(status().entries)} files indexed
          <Show when={status().refreshing}>, refreshing</Show>
        </Show>
      </span>
      <ViewSwitches />
    </footer>
  );
}
