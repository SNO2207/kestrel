import { For, Index, type JSX, Show, createSignal, onCleanup, onMount } from "solid-js";

import { baseName } from "../lib/format";
import { COLORS, type Color, type Pin, shortcutsApi } from "../lib/shortcutsApi";
import { board, changePins, createTag, folderName, pinName, setTag, tags, tagsFor } from "../shortcuts";
import { ContextMenu, type MenuEntry } from "./ContextMenu";

// ---------- menus ----------

const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);

/** Shows a menu for something in the sidebar, at a point in the window. */
export const showMenu = (x: number, y: number, entries: MenuEntry[]) => setMenu({ x, y, entries });

// ---------- dialogs ----------

export interface NameAnswer {
  name: string;
  color: Color | null;
}

interface NameRequest {
  title: string;
  label: string;
  initial: string;
  placeholder?: string;
  /** The button that accepts, such as "Save". */
  action: string;
  /** Also asks for a colour, starting from `initial`; `allowNone` offers "No colour". */
  color?: { initial: Color | null; allowNone: boolean };
}

type Dialog =
  | { kind: "name"; request: NameRequest; resolve: (answer: NameAnswer | null) => void }
  | { kind: "pin"; pin: Pin }
  | { kind: "tags"; paths: string[] };

const [dialog, setDialog] = createSignal<Dialog | null>(null);

/** Asks for a name, and a colour if `request.color` is set. Resolves to `null` if cancelled. */
export function askName(request: NameRequest): Promise<NameAnswer | null> {
  return new Promise((resolve) => setDialog({ kind: "name", request, resolve }));
}

/** Edits a pin's name, group and number key. */
export const editPin = (pin: Pin) => setDialog({ kind: "pin", pin });

/** Shows which tags the items have, to put tags on them or take them off. */
export const openTagPicker = (paths: string[]) => {
  if (paths.length > 0) setDialog({ kind: "tags", paths });
};

/** Whether a dialog of the sidebar's is open, so app shortcuts leave the list alone. */
export const dialogOpen = () => dialog() !== null;

const capitalised = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** The menus and dialogs of the shortcuts. `onClose` hands focus back to the list. */
export function ShortcutDialogs(props: { onClose: () => void }) {
  const close = () => {
    setDialog(null);
    props.onClose();
  };
  const of = <K extends Dialog["kind"]>(kind: K) => {
    const open = dialog();
    return open?.kind === kind ? (open as Extract<Dialog, { kind: K }>) : null;
  };
  return (
    <>
      <Show when={menu()}>
        {(open) => <ContextMenu x={open().x} y={open().y} entries={open().entries} onClose={() => setMenu(null)} />}
      </Show>
      <Show when={of("name")}>
        {(open) => (
          <NameDialog
            request={open().request}
            onDone={(answer) => {
              open().resolve(answer);
              close();
            }}
          />
        )}
      </Show>
      <Show when={of("pin")}>{(open) => <PinDialog pin={open().pin} onClose={close} />}</Show>
      <Show when={of("tags")}>{(open) => <TagPicker paths={open().paths} onClose={close} />}</Show>
    </>
  );
}

/** A dialog's frame: keeps keys inside it, closes on Escape or a click outside, submits on Enter. */
function Shell(props: { title: string; class?: string; onClose: () => void; onSubmit?: () => void; children: JSX.Element }) {
  let panel!: HTMLDivElement;
  const focusFirst = () => {
    const first = panel.querySelector<HTMLElement>("input, select, button:not(:disabled)");
    first?.focus();
    return first;
  };
  onMount(() => {
    const first = focusFirst();
    if (first instanceof HTMLInputElement) first.select();
    // Keys pressed while focus has strayed outside the dialog, as when the button it was on
    // is redrawn, must not reach the list behind it, where Delete deletes.
    const guard = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        event.preventDefault();
        props.onClose();
      } else if (!panel.contains(document.activeElement)) {
        event.stopPropagation();
        event.preventDefault();
        focusFirst();
      }
    };
    window.addEventListener("keydown", guard, true);
    onCleanup(() => window.removeEventListener("keydown", guard, true));
  });
  const onKeyDown = (event: KeyboardEvent) => {
    // The app's own shortcuts must not act on the list behind the dialog.
    event.stopPropagation();
    if (event.key === "Enter" && !(event.target instanceof HTMLButtonElement)) {
      event.preventDefault();
      props.onSubmit?.();
    } else if (event.key === "Tab") {
      const focusable = [...panel.querySelectorAll<HTMLElement>("input, select, button:not(:disabled)")];
      const [first, last] = [focusable[0], focusable[focusable.length - 1]];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
  };
  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div
        ref={panel}
        class={`dialog shortcut-dialog ${props.class ?? ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={props.title}
        onKeyDown={onKeyDown}
      >
        <h2 class="dialog-title">{props.title}</h2>
        {props.children}
      </div>
    </div>
  );
}

/** A row of colours to choose from. */
export function Swatches(props: { value: Color | null; allowNone: boolean; onChange: (color: Color | null) => void }) {
  return (
    <div class="swatches" role="radiogroup" aria-label="Colour">
      <Show when={props.allowNone}>
        <button
          type="button"
          class="swatch none"
          role="radio"
          aria-checked={props.value === null}
          aria-label="No colour"
          title="No colour"
          onClick={() => props.onChange(null)}
        />
      </Show>
      <For each={COLORS}>
        {(color) => (
          <button
            type="button"
            class="swatch"
            data-color={color}
            role="radio"
            aria-checked={props.value === color}
            aria-label={capitalised(color)}
            title={capitalised(color)}
            onClick={() => props.onChange(color)}
          />
        )}
      </For>
    </div>
  );
}

function NameDialog(props: { request: NameRequest; onDone: (answer: NameAnswer | null) => void }) {
  const [name, setName] = createSignal(props.request.initial);
  const [color, setColor] = createSignal<Color | null>(props.request.color?.initial ?? null);
  const submit = () => {
    if (name().trim()) props.onDone({ name: name().trim(), color: color() });
  };
  return (
    <Shell title={props.request.title} onClose={() => props.onDone(null)} onSubmit={submit}>
      <div class="shortcut-dialog-body">
        <label class="dialog-field">
          <span>{props.request.label}</span>
          <input
            spellcheck={false}
            value={name()}
            placeholder={props.request.placeholder ?? ""}
            maxLength={120}
            onInput={(event) => setName(event.currentTarget.value)}
          />
        </label>
        <Show when={props.request.color}>
          {(options) => (
            <div class="dialog-field">
              <span>Colour</span>
              <Swatches value={color()} allowNone={options().allowNone} onChange={setColor} />
            </div>
          )}
        </Show>
      </div>
      <div class="dialog-footer">
        <span class="dialog-spacer" />
        <button class="dialog-button" onClick={() => props.onDone(null)}>
          Cancel
        </button>
        <button class="dialog-button primary" disabled={!name().trim()} onClick={submit}>
          {props.request.action}
        </button>
      </div>
    </Shell>
  );
}

const NO_GROUP = "none";
const NEXT_FREE = "auto";

function PinDialog(props: { pin: Pin; onClose: () => void }) {
  const [alias, setAlias] = createSignal(props.pin.alias ?? "");
  const [group, setGroup] = createSignal(props.pin.group === null ? NO_GROUP : String(props.pin.group));
  const [key, setKey] = createSignal(props.pin.slot === null ? NEXT_FREE : String(props.pin.slot));
  const ownName = () => folderName(props.pin.path);

  const save = async () => {
    // Read everything now: once the dialog closes, its props can no longer be read.
    const pin = props.pin;
    const named = alias().trim();
    const nextAlias = named === "" || named === ownName() ? null : named;
    const nextKey = key() === NEXT_FREE ? null : Number(key());
    const nextGroup = group() === NO_GROUP ? null : Number(group());
    props.onClose();
    await changePins(async () => {
      if (nextAlias !== pin.alias) await shortcutsApi.setAlias(pin.id, nextAlias);
      if (nextKey !== pin.slot) await shortcutsApi.setKey(pin.id, nextKey);
      // A pin moved to another group goes to its end.
      if (nextGroup !== pin.group) await shortcutsApi.movePin(pin.id, nextGroup, Number.MAX_SAFE_INTEGER);
    });
  };

  return (
    <Shell title={`Edit ${pinName(props.pin)}`} onClose={props.onClose} onSubmit={() => void save()}>
      <div class="shortcut-dialog-body">
        <label class="dialog-field">
          <span>Name in the sidebar</span>
          <input
            spellcheck={false}
            value={alias()}
            placeholder={ownName()}
            maxLength={120}
            onInput={(event) => setAlias(event.currentTarget.value)}
          />
        </label>
        <label class="dialog-field">
          <span>Group</span>
          <select value={group()} onChange={(event) => setGroup(event.currentTarget.value)}>
            <option value={NO_GROUP}>No group</option>
            <For each={board().groups}>{(option) => <option value={String(option.id)}>{option.name}</option>}</For>
          </select>
        </label>
        <label class="dialog-field">
          <span>Shortcut</span>
          <select value={key()} onChange={(event) => setKey(event.currentTarget.value)}>
            <option value={NEXT_FREE}>Next free number{props.pin.slot === null && props.pin.key ? ` (now Ctrl+${props.pin.key})` : ""}</option>
            <For each={[1, 2, 3, 4, 5, 6, 7, 8, 9]}>{(n) => <option value={String(n)}>Ctrl+{n}</option>}</For>
          </select>
        </label>
        <p class="shortcut-dialog-note" title={props.pin.path}>
          {props.pin.path}
        </p>
      </div>
      <div class="dialog-footer">
        <span class="dialog-spacer" />
        <button class="dialog-button" onClick={props.onClose}>
          Cancel
        </button>
        <button class="dialog-button primary" onClick={() => void save()}>
          Save
        </button>
      </div>
    </Shell>
  );
}

/** Puts tags on the items or takes them off, and makes new tags. */
function TagPicker(props: { paths: string[]; onClose: () => void }) {
  const [name, setName] = createSignal("");
  const [color, setColor] = createSignal<Color>("blue");
  const title = () => (props.paths.length === 1 ? `Tags for ${baseName(props.paths[0])}` : `Tags for ${props.paths.length} items`);
  /** How many of the items have the tag. */
  const holders = (tagId: number) => props.paths.filter((path) => tagsFor(path).some((tag) => tag.id === tagId)).length;
  const add = async () => {
    if (!name().trim()) return;
    const made = await createTag(name().trim(), color(), props.paths);
    if (made) setName("");
  };

  return (
    <Shell title={title()} class="tag-picker" onClose={props.onClose} onSubmit={() => void add()}>
      <div class="shortcut-dialog-body">
        <div class="tag-choices" role="group" aria-label="Tags">
          <Show when={tags().length === 0}>
            <p class="shortcut-dialog-note">No tags yet. Make one below.</p>
          </Show>
          {/* By position, so a tag's button, and the focus on it, survive the refresh a click causes. */}
          <Index each={tags()}>
            {(tag) => {
              const count = () => holders(tag().id);
              const state = () => (count() === 0 ? "false" : count() === props.paths.length ? "true" : "mixed");
              return (
                <button
                  type="button"
                  class="tag-choice"
                  role="checkbox"
                  aria-checked={state()}
                  onClick={() => void setTag(props.paths, tag(), state() !== "true")}
                >
                  <span class="tag-check" aria-hidden="true" />
                  <span class="tag-dot" data-color={tag().color} />
                  <span class="tag-choice-name">{tag().name}</span>
                </button>
              );
            }}
          </Index>
        </div>
        <div class="tag-new">
          <label class="dialog-field">
            <span>New tag</span>
            <input
              spellcheck={false}
              value={name()}
              placeholder="Name"
              maxLength={120}
              onInput={(event) => setName(event.currentTarget.value)}
            />
          </label>
          <Swatches value={color()} allowNone={false} onChange={(next) => next && setColor(next)} />
        </div>
      </div>
      <div class="dialog-footer">
        <button class="dialog-button" disabled={!name().trim()} onClick={() => void add()}>
          Add tag
        </button>
        <span class="dialog-spacer" />
        <button class="dialog-button primary" onClick={props.onClose}>
          Done
        </button>
      </div>
    </Shell>
  );
}
