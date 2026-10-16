import { For, Show, createSignal } from "solid-js";

import type { SortKey, SortSpec } from "../lib/api";
import {
  CHOOSABLE,
  COLUMNS,
  MIN_WIDTH,
  columnSet,
  moveColumn,
  resetColumns,
  setWidths,
  toggleColumn,
  type ColumnId,
  type ColumnKind,
} from "../lib/columns";
import { ContextMenu, type MenuEntry } from "./ContextMenu";

/** How far a press on a header travels sideways before it drags the column. */
const DRAG_THRESHOLD = 5;

/**
 * The details view's column headers: click to sort, drag to reorder, drag an edge to resize
 * (double-click it to fit), and right-click for the columns to show and the fitting commands.
 * Folders and search results each have their own set (lib/columns.ts).
 */
export function ColumnHeaders(props: {
  kind: ColumnKind;
  sort: SortSpec;
  /** How far the rows are scrolled sideways; the headers follow. */
  shift: number;
  onSort: (key: SortKey) => void;
  /** The width a column's content needs, from the rows on screen. */
  measure: (id: ColumnId) => number;
}) {
  let header!: HTMLDivElement;
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  /** The column being dragged, where it would go, and the line that shows where. */
  const [moving, setMoving] = createSignal<{ id: ColumnId; to: number; at: number } | null>(null);
  const [resizing, setResizing] = createSignal<ColumnId | null>(null);
  /** A drag just ended on this header: its click must not sort. */
  let dragged = false;

  const order = () => columnSet(props.kind).order;
  const cells = () => [...header.querySelectorAll<HTMLElement>(".column-cell")];

  const fit = (id: ColumnId) => setWidths(props.kind, { [id]: Math.max(MIN_WIDTH, props.measure(id)) });
  const fitAll = () => setWidths(props.kind, Object.fromEntries(order().map((id) => [id, Math.max(MIN_WIDTH, props.measure(id))])));

  const openMenu = (event: MouseEvent, id: ColumnId | null) => {
    event.preventDefault();
    event.stopPropagation();
    const shown = order();
    const entries: MenuEntry[] = [
      ...(id ? [{ label: "Size column to fit", run: () => fit(id) }] : []),
      { label: "Size all columns to fit", run: fitAll },
      "separator",
      ...CHOOSABLE.map(
        (each): MenuEntry => ({
          label: COLUMNS[each].label,
          checked: shown.includes(each),
          disabled: each === "name",
          run: () => toggleColumn(props.kind, each),
        }),
      ),
      "separator",
      { label: "Reset columns", run: () => resetColumns(props.kind) },
    ];
    setMenu({ x: event.clientX, y: event.clientY, entries });
  };

  /** Follows a press on a header: a click sorts, a sideways drag moves the column. */
  const press = (id: ColumnId, event: PointerEvent) => {
    if (event.button !== 0) return;
    dragged = false;
    const startX = event.clientX;
    const move = (moved: PointerEvent) => {
      if (!moving() && Math.abs(moved.clientX - startX) < DRAG_THRESHOLD) return;
      dragged = true;
      // The gap nearest the pointer, between the shown columns.
      const boxes = cells().map((cell) => cell.getBoundingClientRect());
      const from = order().indexOf(id);
      let to = boxes.findIndex((box) => moved.clientX < box.left + box.width / 2);
      if (to < 0) to = boxes.length;
      const left = header.getBoundingClientRect().left;
      const at = (to < boxes.length ? boxes[to].left : boxes[boxes.length - 1].right) - left;
      // Dropping a column next to itself leaves it where it is.
      setMoving({ id, to: to > from ? to - 1 : to, at });
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      const done = moving();
      setMoving(null);
      if (done) moveColumn(props.kind, done.id, done.to);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  };

  /** Drags a column's right edge to resize it. */
  const resize = (id: ColumnId, event: PointerEvent) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const handle = event.currentTarget as HTMLElement;
    const cell = handle.parentElement!;
    const startX = event.clientX;
    const startWidth = cell.getBoundingClientRect().width;
    handle.setPointerCapture(event.pointerId);
    setResizing(id);
    const move = (moved: PointerEvent) => setWidths(props.kind, { [id]: Math.max(MIN_WIDTH, startWidth + moved.clientX - startX) });
    const end = () => {
      setResizing(null);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };

  return (
    <>
    <div
      ref={header}
      class="list-header"
      classList={{ resizing: resizing() !== null, moving: moving() !== null }}
      role="row"
      style={{ transform: props.shift ? `translateX(${-props.shift}px)` : undefined }}
      onContextMenu={(event) => openMenu(event, null)}
    >
      <For each={order()}>
        {(id) => {
          const info = COLUMNS[id];
          const sorted = () => props.sort.key === info.sort;
          return (
            <div class="column-cell" classList={{ dragging: moving()?.id === id }} data-column={id}>
              <button
                class="column-header"
                classList={{ [id]: true, numeric: info.numeric, sorted: sorted() }}
                aria-sort={sorted() ? (props.sort.desc ? "descending" : "ascending") : "none"}
                onPointerDown={(event) => press(id, event)}
                onClick={() => {
                  if (dragged) dragged = false;
                  else props.onSort(info.sort);
                }}
                onContextMenu={(event) => openMenu(event, id)}
              >
                <span class="column-label">{info.label}</span>
                <Show when={sorted()}>
                  <svg class="sort-arrow" classList={{ desc: props.sort.desc }} width="8" height="8" viewBox="0 0 8 8" aria-hidden="true">
                    <path d="M1 5.5 4 2.5l3 3" fill="none" stroke="currentColor" stroke-width="1.3" />
                  </svg>
                </Show>
              </button>
              <span
                class="column-resizer"
                role="separator"
                aria-orientation="vertical"
                aria-label={`Resize ${info.label}`}
                title="Drag to resize. Double-click to fit."
                onPointerDown={(event) => resize(id, event)}
                onDblClick={(event) => {
                  event.stopPropagation();
                  fit(id);
                }}
              />
            </div>
          );
        }}
      </For>
      <Show when={moving()}>{(drag) => <span class="column-drop" style={{ left: `${drag().at - 1}px` }} />}</Show>
    </div>
    {/* Outside the header, which moves sideways with the rows and would carry the menu along. */}
    <Show when={menu()}>
      {(open) => <ContextMenu x={open().x} y={open().y} entries={open().entries} onClose={() => setMenu(null)} />}
    </Show>
    </>
  );
}
