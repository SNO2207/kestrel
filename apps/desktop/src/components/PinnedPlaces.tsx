import { For, Show, createSignal } from "solid-js";

import { isDropPin, isDropPlace } from "../dragdrop";
import { shortcutsApi, type Pin, type PinGroup } from "../lib/shortcutsApi";
import {
  board,
  changePins,
  findPin,
  openPin,
  openPlace,
  pinName,
  showInFolder,
} from "../shortcuts";
import { activeTab } from "../state";
import { askName, editPin, showMenu } from "./ShortcutDialogs";
import { ShortcutIcon } from "./ShortcutIcon";

/** How far the pointer moves with the button down before a press becomes a reorder. */
const DRAG_THRESHOLD = 4;
/** An index past any list's end: "put it last". */
const LAST = Number.MAX_SAFE_INTEGER;

/** What is being dragged to a new place in the sidebar. */
type Dragged = { kind: "pin"; pin: Pin } | { kind: "group"; group: PinGroup };

/**
 * Where a dragged pin or group would land: the group and position the backend is given, and
 * what to draw. Positions count the other items there, leaving out the one being moved.
 */
type Spot =
  | { kind: "pin"; group: number | null; index: number; beside?: { id: number; after: boolean }; into?: number | null }
  | { kind: "group"; index: number; beside: { id: number; after: boolean } };

const pinsIn = (group: number | null) => board().pins.filter((pin) => pin.group === group);
const samePath = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

/** The pinned places: pins above every group, then each user-named group. */
export function PinnedPlaces() {
  let section!: HTMLElement;
  const [dragged, setDragged] = createSignal<Dragged | null>(null);
  const [spot, setSpot] = createSignal<Spot | null>(null);
  /** A reorder just ended, so the click that follows the release is not an open. */
  let justDragged = false;
  const current = () => activeTab()?.path ?? "";

  /** Where the dragged item would land with the pointer at `x`, `y`. */
  const spotAt = (x: number, y: number, item: Dragged): Spot | null => {
    const element = document.elementFromPoint(x, y);
    if (!element || !section.contains(element)) return null;
    const half = (box: DOMRect) => y > box.top + box.height / 2;
    if (item.kind === "group") {
      const block = element.closest<HTMLElement>("[data-group-block]");
      const groups = board().groups.filter((group) => group.id !== item.group.id);
      if (!block) return groups.length > 0 ? { kind: "group", index: 0, beside: { id: groups[0].id, after: false } } : null;
      const id = Number(block.dataset.groupBlock);
      if (id === item.group.id) return null;
      const after = half(block.getBoundingClientRect());
      return { kind: "group", index: groups.findIndex((group) => group.id === id) + (after ? 1 : 0), beside: { id, after } };
    }
    const row = element.closest<HTMLElement>("[data-pin-id]");
    const target = row && board().pins.find((pin) => pin.id === Number(row.dataset.pinId));
    if (row && target) {
      if (target.id === item.pin.id) return null;
      const after = half(row.getBoundingClientRect());
      const others = pinsIn(target.group).filter((pin) => pin.id !== item.pin.id);
      const index = others.findIndex((pin) => pin.id === target.id) + (after ? 1 : 0);
      return { kind: "pin", group: target.group, index, beside: { id: target.id, after } };
    }
    // Anywhere else in a group, its heading included, adds to the end of it.
    const zone = element.closest<HTMLElement>("[data-pin-zone]");
    const group = zone?.dataset.pinGroup ? Number(zone.dataset.pinGroup) : null;
    return { kind: "pin", group, index: LAST, into: group };
  };

  /** Follows a press on a pin or a group's heading; past a few pixels it becomes a reorder. */
  const press = (event: PointerEvent, item: Dragged) => {
    if (event.button !== 0) return;
    const start = { x: event.clientX, y: event.clientY };
    let moving = false;
    const move = (next: PointerEvent) => {
      if ((next.buttons & 1) === 0) return stop();
      if (!moving) {
        const far = Math.abs(next.clientX - start.x) > DRAG_THRESHOLD || Math.abs(next.clientY - start.y) > DRAG_THRESHOLD;
        if (!far) return;
        moving = true;
        setDragged(item);
      }
      setSpot(spotAt(next.clientX, next.clientY, item));
    };
    const up = () => {
      const landing = moving ? spot() : null;
      stop();
      if (!moving) return;
      justDragged = true;
      window.setTimeout(() => (justDragged = false));
      if (landing?.kind === "pin" && item.kind === "pin") {
        void changePins(() => shortcutsApi.movePin(item.pin.id, landing.group, landing.index));
      } else if (landing?.kind === "group" && item.kind === "group") {
        void changePins(() => shortcutsApi.moveGroup(item.group.id, landing.index));
      }
    };
    const cancel = (key: KeyboardEvent) => {
      if (key.key !== "Escape") return;
      key.stopPropagation();
      moving = false;
      stop();
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", stop);
      window.removeEventListener("keydown", cancel, true);
      setDragged(null);
      setSpot(null);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", stop);
    window.addEventListener("keydown", cancel, true);
  };

  const besides = (id: number, after: boolean, kind: Spot["kind"]) => {
    const at = spot();
    return at?.kind === kind && at.beside?.id === id && at.beside.after === after;
  };

  const newGroup = async () => {
    const answer = await askName({ title: "New group", label: "Name", initial: "", action: "Create", color: { initial: null, allowNone: true } });
    if (answer) await changePins(() => shortcutsApi.createGroup(answer.name, answer.color));
  };

  const groupMenu = (event: MouseEvent, group: PinGroup) => {
    event.preventDefault();
    event.stopPropagation();
    const folders = pinsIn(group.id).filter((pin) => pin.dir && !pin.missing);
    showMenu(event.clientX, event.clientY, [
      {
        label: "Open all in new tabs",
        disabled: folders.length === 0,
        run: () => folders.forEach((pin) => openPlace(pin.path, true, true)),
      },
      "separator",
      {
        label: "Rename and colour…",
        run: async () => {
          const answer = await askName({
            title: "Edit group",
            label: "Name",
            initial: group.name,
            action: "Save",
            color: { initial: group.color, allowNone: true },
          });
          if (answer) await changePins(() => shortcutsApi.updateGroup(group.id, { name: answer.name, color: answer.color ?? "" }));
        },
      },
      {
        label: group.collapsed ? "Expand" : "Collapse",
        run: () => void changePins(() => shortcutsApi.updateGroup(group.id, { collapsed: !group.collapsed })),
      },
      "separator",
      { label: "New group…", run: () => void newGroup() },
      "separator",
      {
        label: "Delete group",
        danger: true,
        run: () => void changePins(() => shortcutsApi.deleteGroup(group.id)),
      },
    ]);
  };

  const pinMenu = (event: MouseEvent, pin: Pin) => {
    event.preventDefault();
    event.stopPropagation();
    showMenu(event.clientX, event.clientY, [
      ...(pin.missing
        ? [{ label: "Find again", run: () => void findPin(pin) }]
        : [
            { label: "Open", run: () => void openPin(pin) },
            ...(pin.dir ? [{ label: "Open in new tab", run: () => void openPin(pin, true) }] : []),
            { label: "Show in folder", run: () => void showInFolder(pin.path) },
          ]),
      "separator",
      { label: "Edit…", hint: pin.key ? `Ctrl+${pin.key}` : undefined, run: () => editPin(pin) },
      "separator",
      { label: "Remove from sidebar", danger: true, run: () => void changePins(() => shortcutsApi.unpin(pin.id)) },
    ]);
  };

  const sectionMenu = (event: MouseEvent) => {
    event.preventDefault();
    showMenu(event.clientX, event.clientY, [{ label: "New group…", run: () => void newGroup() }]);
  };

  const PinRow = (props: { pin: Pin }) => {
    const pin = () => props.pin;
    const tip = () => {
      const lines = [pin().path];
      if (pin().missing) lines.push("Missing. Click to look for it by its file id.");
      else if (pin().key) lines.push(`Ctrl+${pin().key}`);
      return lines.join("\n");
    };
    return (
      <button
        class="shelf-item pin"
        classList={{
          missing: pin().missing,
          current: pin().dir && samePath(current(), pin().path),
          "drop-target": pin().dir && isDropPlace(pin().path),
          dragging: dragged()?.kind === "pin" && (dragged() as { pin: Pin }).pin.id === pin().id,
          "insert-before": besides(pin().id, false, "pin"),
          "insert-after": besides(pin().id, true, "pin"),
        }}
        title={tip()}
        data-pin-id={pin().id}
        data-place={pin().dir && !pin().missing ? pin().path : undefined}
        data-place-name={pinName(pin())}
        onPointerDown={(event) => press(event, { kind: "pin", pin: pin() })}
        onClick={(event) => {
          if (justDragged) return;
          void openPin(pin(), event.ctrlKey);
        }}
        onAuxClick={(event) => {
          if (event.button === 1) void openPin(pin(), true);
        }}
        onContextMenu={(event) => pinMenu(event, pin())}
      >
        <ShortcutIcon name={pin().missing ? "missing" : pin().dir ? "folder" : "file"} />
        <span class="shelf-label">{pinName(pin())}</span>
        <Show when={pin().missing}>
          <span class="shelf-note">Missing</span>
        </Show>
        <Show when={!pin().missing && pin().key}>
          <kbd class="shelf-key" aria-label={`Ctrl+${pin().key}`}>
            {pin().key}
          </kbd>
        </Show>
      </button>
    );
  };

  return (
    <section
      ref={section}
      class="pinned"
      classList={{ "drop-target": isDropPin(null), reordering: dragged() !== null }}
      aria-label="Pinned"
      data-pin-zone="Pinned"
      onContextMenu={sectionMenu}
    >
      <div class="shelf-head">
        <h2>Pinned</h2>
        <button class="shelf-add" title="New group" aria-label="New group" onClick={() => void newGroup()}>
          <ShortcutIcon name="plus" size={14} />
        </button>
      </div>
      <For each={pinsIn(null)}>{(pin) => <PinRow pin={pin} />}</For>
      <Show when={board().pins.length === 0}>
        <p class="shelf-hint">Drag folders here, or press Ctrl+D, to pin them.</p>
      </Show>
      <For each={board().groups}>
        {(group) => (
          <div
            class="pin-group"
            classList={{
              "drop-target": isDropPin(group.id),
              dragging: dragged()?.kind === "group" && (dragged() as { group: PinGroup }).group.id === group.id,
              "insert-before": besides(group.id, false, "group"),
              "insert-after": besides(group.id, true, "group"),
            }}
            data-group-block={group.id}
            data-pin-zone={group.name}
            data-pin-group={group.id}
            data-color={group.color ?? undefined}
          >
            <button
              class="pin-group-head"
              classList={{ "drop-into": spot()?.kind === "pin" && (spot() as { into?: number | null }).into === group.id }}
              aria-expanded={!group.collapsed}
              title={group.collapsed ? "Expand" : "Collapse"}
              onPointerDown={(event) => press(event, { kind: "group", group })}
              onClick={() => {
                if (!justDragged) void changePins(() => shortcutsApi.updateGroup(group.id, { collapsed: !group.collapsed }));
              }}
              onContextMenu={(event) => groupMenu(event, group)}
            >
              <ShortcutIcon name="chevron" size={12} class="pin-group-chevron" />
              <span class="pin-group-dot" />
              <span class="pin-group-name">{group.name}</span>
              <span class="pin-group-count">{pinsIn(group.id).length}</span>
            </button>
            <Show when={!group.collapsed}>
              <div class="pin-group-body" role="group" aria-label={group.name}>
                <For each={pinsIn(group.id)}>{(pin) => <PinRow pin={pin} />}</For>
                <Show when={pinsIn(group.id).length === 0}>
                  <p class="shelf-hint">Drag pins or folders here.</p>
                </Show>
              </div>
            </Show>
          </div>
        )}
      </For>
    </section>
  );
}
