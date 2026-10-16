import { Show } from "solid-js";

import { formatMoment, momentOf } from "../lib/moments";

/**
 * Where in a video an AI search matched, beside its name in every view; the preview's player
 * starts there. Nothing for other items, or once the search's results are gone.
 */
export function MomentBadge(props: { path: string }) {
  return (
    <Show when={momentOf(props.path) !== null}>
      <span class="moment-badge" title={`Matched at ${formatMoment(momentOf(props.path) ?? 0)}; the preview starts there`}>
        {formatMoment(momentOf(props.path) ?? 0)}
      </span>
    </Show>
  );
}
