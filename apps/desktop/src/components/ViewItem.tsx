import { Show, createEffect, createSignal } from "solid-js";

import { iconUrl, thumbUrl, type Row } from "../lib/api";
import { formatDate, formatSize } from "../lib/format";
import { hasDimensions, imageSize } from "../lib/imageSizes";
import { iconPixels, showsThumbnails, thumbPixels } from "../lib/viewLayout";
import { LOCKED_MESSAGE } from "../lib/access";
import { displayName, type ViewMode } from "../lib/views";
import type { GitBadge } from "../lib/projectsApi";
import { alwaysIcons, isZipRow, ntfsTint } from "../parity2";
import { GitGlyph } from "./GitGlyph";
import { HitSnippet } from "./HitSnippet";
import { ItemCheck } from "./ItemCheck";
import { LockGlyph } from "./LockGlyph";
import { SyncGlyph } from "./SyncGlyph";
import { TagDots } from "./TagDots";
import { MomentBadge } from "./MomentBadge";

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
  /** Files dragged over the window would be dropped into this folder. */
  dropTarget: boolean;
  /** Thumbnails may start loading: the view is not being scrolled. */
  loadThumbs: boolean;
  /** The item's git status in a folder inside a repository; ignored items are dimmed. */
  git?: GitBadge;
  /** View > Show > Item check boxes: the item has one, shown under the pointer and when selected. */
  checkbox?: boolean;
  onMouseDown: (event: MouseEvent) => void;
  onDblClick: (event: MouseEvent) => void;
  onContextMenu: (event: MouseEvent) => void;
  onRenameDone: (name: string | null) => void;
}) {
  /** The thumbnail this item would show; folders and the small views show icons. */
  const thumbSrc = () => {
    const row = props.row;
    // Folder Options' "Always show icons, never thumbnails" (parity2.ts).
    return row && !row.dir && showsThumbnails(props.mode) && !alwaysIcons() ? thumbUrl(row, thumbPixels(props.visual)) : null;
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
        dimmed: props.row?.hidden || props.locked || props.git?.ignored,
        locked: props.locked,
        "drop-target": props.dropTarget,
        "ntfs-compressed": ntfsTint(props.row?.attrs) === "compressed",
        "ntfs-encrypted": ntfsTint(props.row?.attrs) === "encrypted",
      }}
      role="option"
      aria-selected={props.selected}
      data-index={props.index}
      data-path={props.row?.path}
      data-folder={props.row?.dir ? "" : undefined}
      data-zip={isZipRow(props.row) ? "" : undefined}
      title={props.row && !props.renaming ? (props.locked ? `${props.row.name}\n${LOCKED_MESSAGE}` : hitTitle(props.row)) : undefined}
      onMouseDown={(event) => props.onMouseDown(event)}
      onDblClick={(event) => props.onDblClick(event)}
      onContextMenu={(event) => props.onContextMenu(event)}
    >
      <Show when={props.row}>
        {(row) => (
          <>
            <Show when={props.checkbox}>
              <ItemCheck checked={props.selected} />
            </Show>
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
              <Show
                when={props.renaming}
                fallback={
                  <span class="view-item-name">
                    <span class="label">{displayName(row(), props.showExtension)}</span>
                    <Show when={props.locked}>
                      <LockGlyph />
                    </Show>
                    <Show when={props.git?.change}>{(change) => <GitGlyph change={change()} dir={row().dir} />}</Show>
                    <SyncGlyph row={row()} />
                    <TagDots path={row().path} />
                    <MomentBadge path={row().path} />
                  </span>
                }
              >
                <RenameField name={row().name} dir={row().dir} onDone={props.onRenameDone} />
              </Show>
              <Show when={props.mode === "tiles"}>
                <span class="view-item-detail">{typeLabel(row())}</span>
                <span class="view-item-detail">{row().dir && row().size === 0 ? "" : formatSize(row().size)}</span>
              </Show>
              <Show when={props.mode === "content"}>
                <span class="view-item-detail">{typeLabel(row())}</span>
              </Show>
              {/* A search inside files' match, where the view has room for a line of it. */}
              <Show when={SNIPPET_VIEWS.has(props.mode) && row().hit}>{(hit) => <HitSnippet hit={hit()} />}</Show>
            </span>
            <Show when={props.mode === "content"}>
              <ContentDetails row={row()} load={props.loadThumbs} />
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}

/**
 * The Content view's two columns of details, as Explorer lays them out: date modified and size,
 * then an image's dimensions, read from its header when it comes into view.
 */
function ContentDetails(props: { row: Row; load: boolean }) {
  // Like thumbnails, sizes are only asked for once scrolling pauses.
  const dimensions = () => (hasDimensions(props.row) ? imageSize(props.row, props.load) : null);
  return (
    <>
      <span class="content-details">
        <Show when={props.row.mtime > 0}>
          <span class="content-detail">
            <span class="content-key">Date modified:</span> {formatDate(props.row.mtime)}
          </span>
        </Show>
        <Show when={!(props.row.dir && props.row.size === 0)}>
          <span class="content-detail">
            <span class="content-key">Size:</span> {formatSize(props.row.size)}
          </span>
        </Show>
      </span>
      <span class="content-details">
        <Show when={dimensions()}>
          {(size) => (
            <span class="content-detail">
              <span class="content-key">Dimensions:</span> {size()[0]} x {size()[1]}
            </span>
          )}
        </Show>
      </span>
    </>
  );
}

const typeLabel = (row: Row) => (row.dir ? "Folder" : row.ext ? `${row.ext.toUpperCase()} file` : "File");

/** The views with room for a match's line under the name; the icon views show it on hover. */
const SNIPPET_VIEWS = new Set<ViewMode>(["list", "tiles", "content"]);

/** An item's tooltip: its name, and a search inside files' match. */
const hitTitle = (row: Row) => (row.hit ? `${row.name}\nLine ${row.hit.line}: ${row.hit.text}` : row.name);

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
