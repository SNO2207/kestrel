import { Show } from "solid-js";

import { dropHint } from "../dragdrop";

/**
 * Says what a drop would do while files are dragged over a folder. Windows shows a copy cursor
 * over the window whatever the drop will do, so this is where a move is announced.
 */
export function DropHint() {
  return (
    <Show when={dropHint()}>
      {(hint) => (
        <div class="drop-hint" role="status">
          {hint().verb} {hint().to ?? "to"} <strong>{hint().name}</strong>
        </div>
      )}
    </Show>
  );
}
