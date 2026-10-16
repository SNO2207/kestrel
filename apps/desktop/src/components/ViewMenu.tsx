import { Show, createSignal, onCleanup, onMount } from "solid-js";

import type { SortKey } from "../lib/api";
import {
  VIEW_MODES,
  locationKey,
  setOption,
  setViewMode,
  showExtensions,
  showHidden,
  viewModeAt,
  type ViewMode,
} from "../lib/views";
import { activeTab, refresh, setSortSpec, startsDescending, tabs } from "../state";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { groupByEntry } from "./GroupMenu";
import { Icon, type IconName } from "./Icon";

const SORT_KEYS: { key: SortKey; label: string }[] = [
  { key: "name", label: "Name" },
  { key: "modified", label: "Date modified" },
  { key: "type", label: "Type" },
  { key: "size", label: "Size" },
];

const modeIcon = (mode: ViewMode): IconName =>
  mode === "details" || mode === "list" ? "view-details" : mode === "tiles" || mode === "content" ? "view-tiles" : "view-icons";

/** The toolbar's View button: view modes, sort order, and the hidden items and extensions options. */
export function ViewMenu(props: { focusList: () => void }) {
  let button!: HTMLButtonElement;
  const [open, setOpen] = createSignal<{ x: number; y: number } | null>(null);
  /**
   * The last press was on the button while the menu was open. The menu closes itself on that
   * press, and the click that follows must not open it again.
   */
  let pressedToClose = false;

  onMount(() => {
    // Registered before any menu opens, so this runs ahead of the menu's own outside-press
    // handler, while the menu is still open.
    const onPress = (event: MouseEvent) => {
      pressedToClose = open() !== null && button.contains(event.target as Node);
    };
    window.addEventListener("mousedown", onPress, true);
    onCleanup(() => window.removeEventListener("mousedown", onPress, true));
  });

  const tab = activeTab;
  const location = () => {
    const current = tab();
    return current ? locationKey(current) : null;
  };
  const mode = () => viewModeAt(location());

  const entries = (): MenuEntry[] => {
    const current = tab();
    const where = location();
    if (!current || where === null) return [];
    const sort = current.sort;
    return [
      ...VIEW_MODES.map((info) => ({
        label: info.label,
        hint: `Ctrl+Shift+${info.digit}`,
        checked: mode() === info.mode,
        run: () => setViewMode(where, info.mode),
      })),
      "separator",
      ...SORT_KEYS.map(({ key, label }) => ({
        label: `Sort by ${label.toLowerCase()}`,
        checked: sort.key === key,
        run: () => setSortSpec(current.id, { key, desc: sort.key === key ? sort.desc : startsDescending(key) }),
      })),
      { label: "Ascending", checked: !sort.desc, run: () => setSortSpec(current.id, { key: sort.key, desc: false }) },
      { label: "Descending", checked: sort.desc, run: () => setSortSpec(current.id, { key: sort.key, desc: true }) },
      groupByEntry(current.id),
      "separator",
      {
        label: "Show hidden items",
        checked: showHidden(),
        run: () => {
          setOption("hidden", !showHidden());
          // Hidden items are left out by the engine, so every tab lists its folder again.
          tabs.forEach((each) => refresh(each.id));
        },
      },
      { label: "Show file extensions", checked: showExtensions(), run: () => setOption("extensions", !showExtensions()) },
    ];
  };

  const toggle = () => {
    const closing = pressedToClose;
    pressedToClose = false;
    if (closing || open()) return;
    const box = button.getBoundingClientRect();
    setOpen({ x: box.left, y: box.bottom + 4 });
  };

  const close = () => {
    setOpen(null);
    props.focusList();
  };

  return (
    <>
      <button
        ref={button}
        class="view-button"
        title="Change the view, sort order and what is shown"
        aria-haspopup="menu"
        aria-expanded={open() !== null}
        disabled={!tab()}
        onClick={toggle}
      >
        <Icon name={modeIcon(mode())} />
        <span>View</span>
      </button>
      <Show when={open()}>{(at) => <ContextMenu x={at().x} y={at().y} entries={entries()} onClose={close} />}</Show>
    </>
  );
}
