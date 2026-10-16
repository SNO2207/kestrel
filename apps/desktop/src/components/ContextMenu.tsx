import { For, Show, createSignal, onCleanup, onMount } from "solid-js";

import { Icon } from "./Icon";

export interface MenuItem {
  label: string;
  /** Keyboard shortcut shown beside the label. */
  hint?: string;
  disabled?: boolean;
  /** Marks an action that destroys data. */
  danger?: boolean;
  /** For an option that is on or off: whether it is on. Shown as a check mark. */
  checked?: boolean;
  run: () => void;
  /** Entries of a menu that opens beside this item, such as "Group by"; then `run` is unused. */
  submenu?: MenuEntry[];
}

export type MenuEntry = MenuItem | "separator";

/**
 * A pop-up menu at a point on screen. It closes itself on any outside interaction. An item with
 * a submenu opens it beside itself on hover, Enter or Right; Left or Esc closes the submenu
 * again (`onBack`), and choosing anything in it closes every menu.
 */
export function ContextMenu(props: {
  x: number;
  y: number;
  entries: MenuEntry[];
  onClose: () => void;
  /** For a submenu: closes just it, back to the menu it came from. */
  onBack?: () => void;
  /** Whether the menu takes the keyboard when it opens; a submenu opened by hovering doesn't. */
  focus?: boolean;
  /** For a submenu: where its right edge goes when there is no room to the right of its menu. */
  flipX?: number;
}) {
  let menu!: HTMLDivElement;
  const [position, setPosition] = createSignal({ left: props.x, top: props.y });
  const [active, setActive] = createSignal(-1);
  const [sub, setSub] = createSignal<{ index: number; x: number; y: number; flipX: number; focus: boolean } | null>(null);

  const items = () => props.entries.map((entry, index) => ({ entry, index }));
  /** A menu with any options in it gives every item a column for check marks. */
  const checkable = () => props.entries.some((entry) => entry !== "separator" && entry.checked !== undefined);
  const enabled = () =>
    items().filter(({ entry }) => entry !== "separator" && !entry.disabled).map(({ index }) => index);
  const back = () => (props.onBack ?? props.onClose)();

  const openSub = (index: number, focus: boolean) => {
    const item = menu.querySelector(`[data-menu-index="${index}"]`)?.getBoundingClientRect();
    if (!item) return;
    setSub({ index, x: item.right - 2, y: item.top - 5, flipX: menu.getBoundingClientRect().left + 2, focus });
  };

  const choose = (entry: MenuItem, index: number) => {
    if (entry.disabled) return;
    if (entry.submenu) return openSub(index, true);
    props.onClose();
    entry.run();
  };

  const step = (direction: 1 | -1) => {
    const order = enabled();
    if (order.length === 0) return;
    const at = order.indexOf(active());
    setActive(order[(at + direction + order.length) % order.length]);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    event.stopPropagation();
    event.preventDefault();
    const current = props.entries[active()];
    if (event.key === "ArrowDown") step(1);
    else if (event.key === "ArrowUp") step(-1);
    else if (event.key === "Escape") back();
    else if (event.key === "ArrowLeft") props.onBack?.();
    else if (event.key === "ArrowRight" && current && current !== "separator" && current.submenu && !current.disabled) openSub(active(), true);
    else if (event.key === "Enter" && current && current !== "separator") choose(current, active());
  };

  onMount(() => {
    // Keep the menu on screen when it opens near the right or bottom edge; a submenu without
    // room on the right of its menu opens on the left instead.
    const { width, height } = menu.getBoundingClientRect();
    const fits = props.x + width <= window.innerWidth - 4;
    setPosition({
      left: !fits && props.flipX !== undefined ? Math.max(4, props.flipX - width) : Math.max(4, Math.min(props.x, window.innerWidth - width - 4)),
      top: Math.max(4, Math.min(props.y, window.innerHeight - height - 4)),
    });
    if (props.focus !== false) menu.focus();

    const dismiss = (event: Event) => {
      if (!menu.contains(event.target as Node)) back();
    };
    window.addEventListener("mousedown", dismiss, true);
    window.addEventListener("wheel", dismiss, true);
    window.addEventListener("blur", props.onClose);
    window.addEventListener("resize", props.onClose);
    onCleanup(() => {
      window.removeEventListener("mousedown", dismiss, true);
      window.removeEventListener("wheel", dismiss, true);
      window.removeEventListener("blur", props.onClose);
      window.removeEventListener("resize", props.onClose);
    });
  });

  return (
    <div
      ref={menu}
      class="menu"
      role="menu"
      tabindex="-1"
      style={{ left: `${position().left}px`, top: `${position().top}px` }}
      onKeyDown={onKeyDown}
      onContextMenu={(event) => event.preventDefault()}
    >
      <For each={items()}>
        {({ entry, index }) => (
          <Show when={entry !== "separator" && entry} fallback={<div class="menu-separator" role="separator" />}>
            {(item) => (
              <button
                class="menu-item"
                classList={{ active: active() === index, danger: item().danger, open: sub()?.index === index }}
                role={item().checked === undefined ? "menuitem" : "menuitemcheckbox"}
                aria-checked={item().checked}
                aria-haspopup={item().submenu ? "menu" : undefined}
                aria-expanded={item().submenu ? sub()?.index === index : undefined}
                data-menu-index={index}
                disabled={item().disabled}
                onMouseEnter={() => {
                  setActive(index);
                  if (item().submenu && !item().disabled) openSub(index, false);
                  else setSub(null);
                }}
                onClick={() => choose(item(), index)}
              >
                <Show when={checkable()}>
                  <span class="menu-check">
                    <Show when={item().checked}>
                      <Icon name="check" size={14} />
                    </Show>
                  </span>
                </Show>
                <span class="menu-label">{item().label}</span>
                <Show when={item().hint}>
                  <span class="menu-hint">{item().hint}</span>
                </Show>
                <Show when={item().submenu}>
                  <span class="menu-more">
                    <Icon name="chevron" size={12} />
                  </span>
                </Show>
              </button>
            )}
          </Show>
        )}
      </For>
      <Show when={sub()}>
        {(open) => {
          const entry = props.entries[open().index];
          return (
            <Show when={entry !== "separator" && entry.submenu}>
              {(entries) => (
                <ContextMenu
                  x={open().x}
                  y={open().y}
                  flipX={open().flipX}
                  entries={entries()}
                  focus={open().focus}
                  onClose={props.onClose}
                  onBack={() => {
                    setSub(null);
                    menu.focus();
                  }}
                />
              )}
            </Show>
          );
        }}
      </Show>
    </div>
  );
}
