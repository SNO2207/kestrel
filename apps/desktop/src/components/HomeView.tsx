import { For, Show, createEffect, createResource, createSignal, on, onCleanup, onMount } from "solid-js";

import { api, iconUrl, type Row } from "../lib/api";
import { baseName, formatDate, parentPath } from "../lib/format";
import { parityApi, type RecentFile } from "../lib/parityApi";
import { shortcutsApi, type Pin } from "../lib/shortcutsApi";
import { openNewWindow, revealInFolder } from "../parity";
import { setFocusedRow } from "../previewState";
import { board, changePins, folderName, forgetFolder, openPin, pinName } from "../shortcuts";
import { navigate, notify, openTab, tabs } from "../state";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { ParityIcon } from "./ParityIcon";
import { SyncGlyph } from "./SyncGlyph";

/** Frequent folders shown under Quick access. */
const QUICK_SHOWN = 12;
/** Recent files shown. */
const RECENT_SHOWN = 30;
const ONLINE_ONLY = 0x40_0000;

type Section = "quick" | "favorites" | "recent";

/** One thing Home lists. */
interface HomeItem {
  key: string;
  section: Section;
  path: string;
  name: string;
  dir: boolean;
  /** The folder it is in, as shown. */
  location: string;
  /** When it was used, for recent files. */
  used?: number;
  cloud?: boolean;
  pin?: Pin;
}

/** Windows' own Recent items are listed unless the end-to-end tests turn them off. */
const [windowsRecent, setWindowsRecent] = createSignal(true);

const extOf = (name: string) => {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
};

/** What the preview and details panes are told is focused. */
const asRow = (item: HomeItem): Row => ({
  name: item.name,
  path: item.path,
  location: item.location,
  dir: item.dir,
  size: 0,
  mtime: item.used ?? 0,
  ext: item.dir ? "" : extOf(item.name),
  hidden: false,
  attrs: item.cloud ? ONLINE_ONLY : 0,
});

/**
 * Home, Explorer's start page: Quick access (the folders opened most often and lately),
 * Favorites (the pinned places), and Recent (files opened lately, in any program as Windows
 * records them, or through Kestrel). Click to select (the preview and details panes follow),
 * double-click or Enter to open, right-click for the rest.
 */
export function HomeView(props: { tabId: number; focusRef: (focus: () => void) => void }) {
  let root!: HTMLDivElement;
  const [selected, setSelected] = createSignal<string | null>(null);
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  const tab = () => tabs.find((candidate) => candidate.id === props.tabId);

  // Read again whenever the tab is refreshed (F5) or comes back to Home.
  const [frequent, { refetch: refetchFrequent }] = createResource(
    () => tab()?.revision,
    () => shortcutsApi.frequent(QUICK_SHOWN),
  );
  const [recent, { refetch: refetchRecent }] = createResource(
    () => `${tab()?.revision}|${windowsRecent()}`,
    () => parityApi.recent(RECENT_SHOWN, windowsRecent()),
  );
  const reload = () => {
    void refetchFrequent();
    void refetchRecent();
  };

  const quickItems = (): HomeItem[] =>
    (frequent.error ? [] : (frequent() ?? [])).map((folder) => ({
      key: `quick:${folder.path}`,
      section: "quick",
      path: folder.path,
      name: folderName(folder.path),
      dir: true,
      location: parentPath(folder.path) ?? "",
    }));
  const favoriteItems = (): HomeItem[] =>
    board().pins.map((pin) => ({
      key: `favorites:${pin.id}`,
      section: "favorites",
      path: pin.path,
      name: pinName(pin),
      dir: pin.dir,
      location: parentPath(pin.path) ?? "",
      pin,
    }));
  const recentItems = (): HomeItem[] =>
    (recent.error ? [] : (recent() ?? [])).map((file: RecentFile) => ({
      key: `recent:${file.path}`,
      section: "recent",
      path: file.path,
      name: file.name,
      dir: false,
      location: file.location,
      used: file.used,
      cloud: file.cloud,
    }));
  const all = () => [...quickItems(), ...favoriteItems(), ...recentItems()];
  const find = (key: string | null) => all().find((item) => item.key === key);

  onMount(() => {
    props.focusRef(() => root.focus());
    window.addEventListener("focus", reload);
    onCleanup(() => {
      window.removeEventListener("focus", reload);
      setFocusedRow(props.tabId, null);
    });
  });

  // The panes follow the selected item; nothing selected leaves them empty.
  createEffect(
    on(selected, (key) => {
      const item = find(key);
      setFocusedRow(props.tabId, item ? asRow(item) : null);
    }),
  );

  const select = (item: HomeItem) => {
    setSelected(item.key);
    root.querySelector<HTMLElement>(`[data-key="${CSS.escape(item.key)}"]`)?.scrollIntoView({ block: "nearest" });
  };

  const open = (item: HomeItem, inNewTab = false) => {
    if (item.pin) return void openPin(item.pin, inNewTab);
    if (!item.dir) return void api.openPath(item.path).catch(notify).then(reload);
    if (inNewTab) openTab(item.path);
    else navigate(props.tabId, item.path);
  };

  const remove = (item: HomeItem) => {
    if (item.section === "quick") return void forgetFolder(item.path).then(reload);
    if (item.section === "favorites" && item.pin) return void changePins(() => shortcutsApi.unpin(item.pin!.id));
    if (item.section === "recent") void parityApi.forgetRecent(item.path).catch(notify).then(reload);
  };

  const entries = (item: HomeItem): MenuEntry[] => [
    { label: "Open", hint: "Enter", run: () => open(item) },
    ...(item.dir ? [{ label: "Open in new tab", run: () => open(item, true) }, { label: "Open in new window", run: () => openNewWindow(item.path) }] : []),
    { label: "Open file location", run: () => revealInFolder(item.path) },
    "separator",
    { label: "Copy as path", run: () => void navigator.clipboard.writeText(`"${item.path}"`).catch(notify) },
    "separator",
    item.section === "quick"
      ? { label: "Remove from Quick access", run: () => remove(item) }
      : item.section === "favorites"
        ? { label: "Unpin from sidebar", run: () => remove(item) }
        : { label: "Remove from Recent", run: () => remove(item) },
    "separator",
    { label: "Properties", hint: "Alt+Enter", run: () => void api.showProperties(item.path).catch(notify) },
  ];

  const openMenu = (item: HomeItem, x: number, y: number) => {
    select(item);
    setMenu({ x, y, entries: entries(item) });
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const items = all();
    if (items.length === 0) return;
    const at = items.findIndex((item) => item.key === selected());
    const current = at >= 0 ? items[at] : undefined;
    const move = (step: number) => {
      event.preventDefault();
      select(items[Math.min(items.length - 1, Math.max(0, at < 0 ? 0 : at + step))]);
    };
    switch (event.key) {
      case "ArrowDown":
      case "ArrowRight":
        return move(1);
      case "ArrowUp":
      case "ArrowLeft":
        return move(-1);
      case "Home":
        event.preventDefault();
        return select(items[0]);
      case "End":
        event.preventDefault();
        return select(items[items.length - 1]);
      case "Enter":
        event.preventDefault();
        if (current && !event.altKey) open(current, event.ctrlKey);
        else if (current) void api.showProperties(current.path).catch(notify);
        return;
      case "Escape":
        return setSelected(null);
      case "F10":
      case "ContextMenu": {
        if (event.key === "F10" && !event.shiftKey) return;
        event.preventDefault();
        if (!current) return;
        const box = root.querySelector(`[data-key="${CSS.escape(current.key)}"]`)?.getBoundingClientRect();
        if (box) openMenu(current, box.left + 24, box.bottom - 4);
      }
    }
  };

  const Item = (item: { value: HomeItem; tile?: boolean }) => (
    <div
      class="home-item"
      classList={{ tile: item.tile, selected: selected() === item.value.key, missing: item.value.pin?.missing }}
      role="option"
      aria-selected={selected() === item.value.key}
      data-key={item.value.key}
      data-path={item.value.path}
      data-section={item.value.section}
      title={item.value.pin?.missing ? `${item.value.path}\nMoved or deleted. Open it to look for it.` : item.value.path}
      onMouseDown={(event) => {
        if (event.button === 0) select(item.value);
        // Middle-click opens a folder in a new tab, as everywhere else.
        if (event.button === 1 && item.value.dir) {
          event.preventDefault();
          open(item.value, true);
        }
      }}
      onDblClick={() => open(item.value)}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
        openMenu(item.value, event.clientX, event.clientY);
      }}
    >
      <img class="home-icon" src={iconUrl({ dir: item.value.dir, ext: extOf(item.value.name), path: item.value.path }, item.tile ? 48 : 16)} alt="" draggable={false} />
      <span class="home-text">
        <span class="home-name">
          <span class="label">{item.value.name}</span>
          <SyncGlyph row={item.value.cloud === undefined ? undefined : asRow(item.value)} />
        </span>
        <Show when={item.tile}>
          <span class="home-location">{item.value.location || item.value.path}</span>
        </Show>
      </span>
      <Show when={!item.tile}>
        <span class="home-when">{item.value.used ? formatDate(item.value.used) : item.value.dir ? "Folder" : ""}</span>
        <span class="home-location" title={item.value.location}>
          {item.value.location}
        </span>
      </Show>
    </div>
  );

  return (
    <div
      ref={root}
      class="home"
      tabindex="0"
      role="listbox"
      aria-label="Home"
      onKeyDown={onKeyDown}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget || (event.target as HTMLElement).classList.contains("home-section")) setSelected(null);
      }}
      onContextMenu={(event) => event.preventDefault()}
    >
      <section class="home-section" aria-label="Quick access">
        <h2>
          <ParityIcon name="folder" size={14} />
          Quick access
        </h2>
        <Show when={quickItems().length > 0} fallback={<p class="home-empty">The folders you open most often will be here.</p>}>
          <div class="home-tiles">
            <For each={quickItems()}>{(item) => <Item value={item} tile />}</For>
          </div>
        </Show>
      </section>
      <section class="home-section" aria-label="Favorites">
        <h2>
          <ParityIcon name="star" size={14} />
          Favorites
        </h2>
        <Show when={favoriteItems().length > 0} fallback={<p class="home-empty">Pin a folder or file (Ctrl+D) to see it here and in the sidebar.</p>}>
          <div class="home-list">
            <For each={favoriteItems()}>{(item) => <Item value={item} />}</For>
          </div>
        </Show>
      </section>
      <section class="home-section" aria-label="Recent">
        <h2>
          <ParityIcon name="clock" size={14} />
          Recent
        </h2>
        <Show when={recentItems().length > 0} fallback={<p class="home-empty">Files you open will be here.</p>}>
          <div class="home-list">
            <div class="home-columns" aria-hidden="true">
              <span>Name</span>
              <span>Date opened</span>
              <span>Location</span>
            </div>
            <For each={recentItems()}>{(item) => <Item value={item} />}</For>
          </div>
        </Show>
      </section>
      <Show when={menu()}>
        {(at) => (
          <ContextMenu
            x={at().x}
            y={at().y}
            entries={at().entries}
            onClose={() => {
              setMenu(null);
              root.focus();
            }}
          />
        )}
      </Show>
    </div>
  );
}

// Development builds let the end-to-end tests leave the owner's own Recent items out.
if (import.meta.env.DEV) {
  Object.assign(window, { __kestrelHome: { windowsRecent: (on: boolean) => setWindowsRecent(on), name: baseName } });
}
