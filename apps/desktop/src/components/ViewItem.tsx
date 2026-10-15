import { Show, createEffect, createSignal } from "solid-js";

import { iconUrl, thumbUrl, type Row } from "../lib/api";
import { formatSize } from "../lib/format";
import { iconPixels, showsThumbnails, thumbPixels } from "../lib/viewLayout";
import { LOCKED_MESSAGE } from "../lib/access";
import { displayName, type ViewMode } from "../lib/views";
import { LockGlyph } from "./LockGlyph";

/** One file or folder in the icon, list or tile views. Details rows are drawn by the list. */
export function ViewItem(props: {
  index: number;
  /** `undefined` until its page of rows has arrived. */
  row: Row | undefined;
  mode: ViewMode;
  /** CSS pixels of the icon or thumbnail. */
  visual: number;
  selected: boolean;
  focused: boolean;
  renaming: boolean;
  showExtension: boolean;
  /** The item is in, or is, a folder this account can't open. */
  locked: boolean;
  /** Thumbnails may start loading: the view is not being scrolled. */
  loadThumbs: boolean;
  onMouseDown: (event: MouseEvent) => void;
  onDblClick: (event: MouseEvent) => void;
  onContextMenu: (event: MouseEvent) => void;
  onRenameDone: (name: string | null) => void;
}) {
  /** The thumbnail this item would show; folders and the small views show icons. */
  const thumbSrc = () => {
    const row = props.row;
    return row && !row.dir && showsThumbnails(props.mode) ? thumbUrl(row, thumbPixels(props.visual)) : null;
  };
  // A thumbnail is only asked for once scrolling pauses, so flinging through a large folder
  // does not queue thousands of them; once asked for, it stays.
  const [armed, setArmed] = createSignal<string | null>(null);
  createEffect(() => {
    const src = thumbSrc();
    if (src && props.loadThumbs) setArmed(src);
  });
  const wanted = () => {
    const src = thumbSrc();
    return src !== null && armed() === src ? src : null;
  };
  const [loaded, setLoaded] = createSignal<string | null>(null);
  /** The thumbnail has arrived and replaces the icon. */
  const thumbShown = () => wanted() !== null && loaded() === wanted();

  return (
    <div
      class="view-item"
      classList={{
        selected: props.selected,
        focused: props.focused,
        dimmed: props.row?.hidden || props.locked,
        locked: props.locked,
      }}
      role="option"
      aria-selected={props.selected}
      data-index={props.index}
      title={props.row && !props.renaming ? (props.locked ? `${props.row.name}\n${LOCKED_MESSAGE}` : props.row.name) : undefined}
      onMouseDown={(event) => props.onMouseDown(event)}
      onDblClick={(event) => props.onDblClick(event)}
      onContextMenu={(event) => props.onContextMenu(event)}
    >
      <Show when={props.row}>
        {(row) => (
          <>
            <span class="view-item-visual" style={{ width: `${props.visual}px`, height: `${props.visual}px` }}>
              <img
                class="view-item-icon"
                classList={{ covered: thumbShown() }}
                src={iconUrl(row(), iconPixels(props.visual))}
                alt=""
                draggable={false}
              />
              <Show when={wanted()}>
                {(src) => (
                  <img
                    class="view-item-thumb"
                    classList={{ shown: thumbShown() }}
                    src={src()}
                    alt=""
                    draggable={false}
                    decoding="async"
                    onLoad={(event) => setLoaded(event.currentTarget.getAttribute("src"))}
                  />
                )}
              </Show>
            </span>
            <span class="view-item-text">
              <Show when={props.renaming} fallback={<span class="label">{displayName(row(), props.showExtension)}</span>}>
                <RenameField name={row().name} dir={row().dir} onDone={props.onRenameDone} />
              </Show>
              <Show when={props.locked}>
                <LockGlyph />
              </Show>
              <Show when={props.mode === "tiles"}>
                <span class="view-item-detail">{typeLabel(row())}</span>
                <span class="view-item-detail">{row().dir && row().size === 0 ? "" : formatSize(row().size)}</span>
              </Show>
            </span>
          </>
        )}
      </Show>
    </div>
  );
}

const typeLabel = (row: Row) => (row.dir ? "Folder" : row.ext ? `${row.ext.toUpperCase()} file` : "File");

/** The box a name is edited in. It always edits the full name, extension included. */
function RenameField(props: { name: string; dir: boolean; onDone: (name: string | null) => void }) {
  return (
    <input
      class="rename"
      aria-label="New name"
      spellcheck={false}
      value={props.name}
      ref={(input) =>
        queueMicrotask(() => {
          input.focus();
          // Pre-select the name without its extension, as Explorer does.
          const dot = props.dir ? -1 : props.name.lastIndexOf(".");
          input.setSelectionRange(0, dot > 0 ? dot : props.name.length);
        })
      }
      onBlur={(event) => props.onDone(event.currentTarget.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") props.onDone(event.currentTarget.value);
        if (event.key === "Escape") props.onDone(null);
      }}
    />
  );
}
