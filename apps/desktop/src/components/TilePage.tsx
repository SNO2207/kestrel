/**
 * A page of tiles in groups, as This PC draws its folders and drives: the Libraries page, the
 * Network page and a computer's shares (parity2). Click selects, double-click or Enter opens
 * (Ctrl+Enter or a middle click in a new tab), the arrow keys move between tiles, typing jumps to
 * a name, and the context menu is the page's. Tiles take drops like any place (`data-place`) when
 * they are folders.
 */
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, type JSX } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { Icon, type IconName } from "./Icon";

const TYPE_AHEAD_RESET_MS = 700;

export interface Tile {
  /** Unique on the page; also where it leads. */
  path: string;
  name: string;
  /** The line under the name. */
  detail: string;
  icon: IconName;
  /** A folder, which takes files dropped on it. */
  place?: boolean;
  title?: string;
}

export interface TileGroup {
  title: string;
  tiles: Tile[];
  /** Said instead of tiles when there are none; the group is left out when not given. */
  empty?: JSX.Element;
}

export function TilePage(props: {
  page: LocationPageProps;
  label: string;
  groups: TileGroup[];
  onOpen: (tile: Tile, inNewTab: boolean) => void;
  menuFor: (tile: Tile | null, x: number, y: number, windows: boolean) => void;
  /** More of the page, above the groups (a note) or below them. */
  before?: JSX.Element;
  after?: JSX.Element;
}) {
  let root!: HTMLDivElement;
  const [selected, setSelected] = createSignal<string | null>(null);
  const typeAhead = { prefix: "", at: 0 };
  const tiles = createMemo(() => props.groups.flatMap((group) => group.tiles));

  createEffect(() => {
    const count = tiles().length;
    setLocationStatus(props.page.tabId, [`${count} ${count === 1 ? "item" : "items"}`, ...(selected() ? ["1 selected"] : [])]);
  });
  onCleanup(() => setLocationStatus(props.page.tabId, null));

  onMount(() => {
    props.page.focusRef(() => root.focus());
    if (props.page.active) queueMicrotask(() => takeFocusIfFree(root));
  });

  const elementOf = (path: string) => root.querySelector<HTMLElement>(`[data-tile="${CSS.escape(path)}"]`);
  const select = (tile: Tile | undefined) => {
    if (!tile) return;
    setSelected(tile.path);
    elementOf(tile.path)?.scrollIntoView({ block: "nearest" });
  };

  /** The tile in a direction from the selected one, by where the tiles are on screen. */
  const towards = (direction: "up" | "down"): Tile | undefined => {
    const list = tiles();
    const from = list.find((tile) => tile.path === selected());
    const box = from && elementOf(from.path)?.getBoundingClientRect();
    if (!box) return list[0];
    let best: { tile: Tile; score: number } | undefined;
    for (const tile of list) {
      const other = elementOf(tile.path)?.getBoundingClientRect();
      if (!other || tile === from) continue;
      const vertical = direction === "down" ? other.top - box.bottom : box.top - other.bottom;
      if (vertical < -1) continue;
      const score = vertical * 4 + Math.abs(other.left - box.left);
      if (!best || score < best.score) best = { tile, score };
    }
    return best?.tile ?? from;
  };

  const menuAtSelection = (windows: boolean) => {
    const tile = tiles().find((candidate) => candidate.path === selected()) ?? null;
    const box = (tile && elementOf(tile.path)?.getBoundingClientRect()) || root.getBoundingClientRect();
    props.menuFor(tile, box.left + 40, box.top + Math.min(box.height, 40), windows);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const list = tiles();
    const at = list.findIndex((tile) => tile.path === selected());
    const current = list[at];
    const handled = () => event.preventDefault();
    switch (event.key) {
      case "ArrowRight":
        return handled(), select(list[Math.min(list.length - 1, at + 1)]);
      case "ArrowLeft":
        return handled(), select(list[Math.max(0, at - 1)]);
      case "ArrowDown":
        return handled(), select(at < 0 ? list[0] : towards("down"));
      case "ArrowUp":
        return handled(), select(at < 0 ? list[0] : towards("up"));
      case "Home":
        return handled(), select(list[0]);
      case "End":
        return handled(), select(list[list.length - 1]);
      case "Enter":
        if (current) return handled(), props.onOpen(current, event.ctrlKey);
        return;
      case "Escape":
        return setSelected(null);
      case "ContextMenu":
        return handled(), menuAtSelection(false);
      case "F10":
        if (event.shiftKey) return handled(), menuAtSelection(true);
        return;
    }
    if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.altKey && !event.metaKey) {
      handled();
      const now = performance.now();
      const letter = event.key.toLowerCase();
      const fresh = now - typeAhead.at > TYPE_AHEAD_RESET_MS || typeAhead.prefix === letter;
      typeAhead.prefix = fresh ? letter : typeAhead.prefix + letter;
      typeAhead.at = now;
      const start = fresh ? at + 1 : Math.max(0, at);
      for (let step = 0; step < list.length; step++) {
        const tile = list[(start + step) % list.length];
        if (tile.name.toLowerCase().startsWith(typeAhead.prefix)) return select(tile);
      }
    }
  };

  return (
    <div
      ref={root}
      class="pc-page tile-page"
      tabindex="0"
      aria-label={props.label}
      onKeyDown={onKeyDown}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) setSelected(null);
      }}
      onContextMenu={(event) => {
        event.preventDefault();
        if (event.target === event.currentTarget || (event.target as Element).closest(".pc-group-title, .tile-page-note")) {
          setSelected(null);
          props.menuFor(null, event.clientX, event.clientY, false);
        }
      }}
    >
      {props.before}
      <For each={props.groups}>
        {(group) => (
          <Show when={group.tiles.length > 0 || group.empty}>
            <section class="pc-group" aria-label={group.title}>
              <h2 class="pc-group-title">
                {group.title}
                <span class="pc-group-count">{group.tiles.length}</span>
              </h2>
              <Show when={group.tiles.length > 0} fallback={<div class="tile-page-empty">{group.empty}</div>}>
                <div class="pc-tiles" role="listbox" aria-label={group.title}>
                  <For each={group.tiles}>
                    {(tile) => (
                      <div
                        class="pc-tile"
                        classList={{ selected: selected() === tile.path, "drop-target": !!tile.place && isDropPlace(tile.path) }}
                        role="option"
                        aria-selected={selected() === tile.path}
                        data-tile={tile.path}
                        data-place={tile.place ? tile.path : undefined}
                        data-place-name={tile.place ? tile.name : undefined}
                        title={tile.title ?? tile.path}
                        onMouseDown={(event) => {
                          if (event.button === 0 || event.button === 2) setSelected(tile.path);
                        }}
                        onDblClick={() => props.onOpen(tile, false)}
                        onAuxClick={(event) => {
                          if (event.button === 1) props.onOpen(tile, true);
                        }}
                        onContextMenu={(event) => {
                          event.preventDefault();
                          event.stopPropagation();
                          setSelected(tile.path);
                          props.menuFor(tile, event.clientX, event.clientY, event.shiftKey);
                        }}
                      >
                        <span class="pc-tile-icon">
                          <Icon name={tile.icon} size={32} />
                        </span>
                        <span class="pc-tile-body">
                          <span class="pc-tile-name">{tile.name}</span>
                          <span class="pc-tile-detail">{tile.detail}</span>
                        </span>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
            </section>
          </Show>
        )}
      </For>
      {props.after}
    </div>
  );
}
