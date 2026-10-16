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
  /** An image shown before the label, such as a file type's icon in the New menu. */
  icon?: string;
  run: () => void;
}

export type MenuEntry = MenuItem | "separator";

/** A pop-up menu at a point on screen. It closes itself on any outside interaction. */
export function ContextMenu(props: { x: number; y: number; entries: MenuEntry[]; onClose: () => void }) {
  let menu!: HTMLDivElement;
  const [position, setPosition] = createSignal({ left: props.x, top: props.y });
  const [active, setActive] = createSignal(-1);

  const items = () => props.entries.map((entry, index) => ({ entry, index }));
  /** A menu with any options in it gives every item a column for check marks. */
  const checkable = () => props.entries.some((entry) => entry !== "separator" && entry.checked !== undefined);
  const enabled = () =>
    items().filter(({ entry }) => entry !== "separator" && !entry.disabled).map(({ index }) => index);

  const choose = (entry: MenuItem) => {
    if (entry.disabled) return;
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
    else if (event.key === "Escape") props.onClose();
    else if (event.key === "Enter" && current && current !== "separator") choose(current);
  };

  onMount(() => {
    // Keep the menu on screen when it opens near the right or bottom edge.
    const { width, height } = menu.getBoundingClientRect();
    setPosition({
      left: Math.max(4, Math.min(props.x, window.innerWidth - width - 4)),
      top: Math.max(4, Math.min(props.y, window.innerHeight - height - 4)),
    });
    menu.focus();

    const dismiss = (event: Event) => {
      if (!menu.contains(event.target as Node)) props.onClose();
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
                classList={{ active: active() === index, danger: item().danger }}
                role={item().checked === undefined ? "menuitem" : "menuitemcheckbox"}
                aria-checked={item().checked}
                disabled={item().disabled}
                onMouseEnter={() => setActive(index)}
                onClick={() => choose(item())}
              >
                <Show when={checkable()}>
                  <span class="menu-check">
                    <Show when={item().checked}>
                      <Icon name="check" size={14} />
                    </Show>
                  </span>
                </Show>
                <Show when={item().icon}>
                  {(src) => <img class="menu-icon" src={src()} width="16" height="16" alt="" draggable={false} />}
                </Show>
                <span class="menu-label">{item().label}</span>
                <Show when={item().hint}>
                  <span class="menu-hint">{item().hint}</span>
                </Show>
              </button>
            )}
          </Show>
        )}
      </For>
    </div>
  );
}
