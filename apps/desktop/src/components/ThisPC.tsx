/**
 * This PC, as in Explorer: the user's folders, then devices and drives, then network
 * locations (mapped drives, connected or not). Each drive shows its name, label and letter,
 * how full it is (the bar turns red from 90 % full, as Explorer's does), "N GB free of M GB"
 * and its file system. A drive still being asked about (a network drive that is slow to
 * answer) says so and fills in when its answer comes.
 *
 * Double-click or Enter opens; the arrow keys move between tiles; typing jumps to a name.
 * The context menu offers Open, Open in new tab, Show disk usage (once the disk usage hub
 * is there; see `setDiskUsageHandler` in places.ts), Eject for removable media, Properties
 * and the Windows menu. Drives and folders take drops like any place in the sidebar.
 */
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { api, type Folder } from "../lib/api";
import { formatSize } from "../lib/format";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import type { DriveInfo } from "../lib/placesApi";
import { placesApi } from "../lib/placesApi";
import {
  analyseDiskUsage,
  canAnalyse,
  canEject,
  ejectDrive,
  isNearlyFull,
  loadThisPc,
  openLocation,
  thisPc,
  thisPcLoaded,
  usedShare,
} from "../places";
import { isDropPlace } from "../dragdrop";
import { pinPaths } from "../shortcuts";
import { notify } from "../state";
import type { MenuEntry } from "./ContextMenu";
import { Icon, hasIcon, type IconName } from "./Icon";
import { showMenu } from "./ShortcutDialogs";

const TYPE_AHEAD_RESET_MS = 700;

/** A tile of the page: a folder or a drive. */
type Tile = { kind: "folder"; folder: Folder; path: string; name: string } | { kind: "drive"; drive: DriveInfo; path: string; name: string };

function driveIcon(drive: DriveInfo): IconName {
  switch (drive.kind) {
    case "removable":
      return "usb";
    case "optical":
      return "disc";
    case "network":
      return "network-drive";
    default:
      return "drive";
  }
}

/** What a drive tile says under its bar. */
function driveDetail(drive: DriveInfo): string {
  switch (drive.state) {
    case "reading":
      return "Reading…";
    case "no-media":
      return drive.kind === "optical" ? "No disc" : "No media";
    case "disconnected":
      return drive.remote ? `Disconnected from ${drive.remote}` : "Disconnected";
    case "unavailable":
      return "Not available";
    default:
      return drive.total > 0 ? `${formatSize(drive.free)} free of ${formatSize(drive.total)}` : "";
  }
}

const dark = () => !window.matchMedia("(prefers-color-scheme: light)").matches;

export function ThisPcPage(props: LocationPageProps) {
  let page!: HTMLDivElement;
  const [selected, setSelected] = createSignal<string | null>(null);
  const typeAhead = { prefix: "", at: 0 };

  const folders = createMemo<Tile[]>(() => thisPc().folders.map((folder) => ({ kind: "folder", folder, path: folder.path, name: folder.name })));
  const local = createMemo<Tile[]>(() =>
    thisPc()
      .drives.filter((drive) => drive.kind !== "network")
      .map((drive) => ({ kind: "drive", drive, path: drive.path, name: drive.name })),
  );
  const network = createMemo<Tile[]>(() =>
    thisPc()
      .drives.filter((drive) => drive.kind === "network")
      .map((drive) => ({ kind: "drive", drive, path: drive.path, name: drive.name })),
  );
  const tiles = createMemo(() => [...folders(), ...local(), ...network()]);

  // Read again whenever the page is shown or refreshed.
  createEffect(on(() => props.revision, () => void loadThisPc()));

  createEffect(() => {
    const count = tiles().length;
    setLocationStatus(props.tabId, [`${count} ${count === 1 ? "item" : "items"}`, ...(selected() ? ["1 selected"] : [])]);
  });
  onCleanup(() => setLocationStatus(props.tabId, null));

  onMount(() => {
    props.focusRef(() => page.focus());
    if (props.active) queueMicrotask(() => takeFocusIfFree(page));
  });

  const open = (tile: Tile, inNewTab = false) => openLocation(tile.path, inNewTab);
  const elementOf = (path: string) => page.querySelector<HTMLElement>(`[data-tile="${CSS.escape(path)}"]`);
  const select = (tile: Tile | undefined) => {
    if (!tile) return;
    setSelected(tile.path);
    elementOf(tile.path)?.scrollIntoView({ block: "nearest" });
  };

  /** The tile in a direction from the selected one, by where the tiles are on screen. */
  const tileTowards = (direction: "up" | "down"): Tile | undefined => {
    const list = tiles();
    const from = list.find((tile) => tile.path === selected());
    const box = from && elementOf(from.path)?.getBoundingClientRect();
    if (!box) return list[0];
    let best: { tile: Tile; score: number } | undefined;
    for (const tile of list) {
      const other = elementOf(tile.path)?.getBoundingClientRect();
      if (!other) continue;
      const vertical = direction === "down" ? other.top - box.bottom : box.top - other.bottom;
      if (vertical < -1) continue;
      const score = vertical * 4 + Math.abs(other.left - box.left);
      if (!best || score < best.score) best = { tile, score };
    }
    return best?.tile;
  };

  const menuFor = (tile: Tile, x: number, y: number) => {
    const entries: MenuEntry[] = [
      { label: "Open", hint: "Enter", run: () => open(tile) },
      { label: "Open in new tab", run: () => open(tile, true) },
    ];
    if (tile.kind === "drive") {
      const drive = tile.drive;
      if (canAnalyse() || canEject(drive)) entries.push("separator");
      if (canAnalyse()) entries.push({ label: "Show disk usage", run: () => analyseDiskUsage(drive.path) });
      if (canEject(drive)) entries.push({ label: "Eject", run: () => void ejectDrive(drive) });
    } else {
      entries.push("separator", { label: "Pin to sidebar", run: () => void pinPaths([tile.path]) });
    }
    entries.push(
      "separator",
      { label: "Properties", hint: "Alt+Enter", run: () => void api.showProperties(tile.path).catch(notify) },
      "separator",
      { label: "Show more options", hint: "Shift+F10", run: () => void placesApi.shellPathsMenu([tile.path], x, y, dark(), false).catch(notify) },
    );
    showMenu(x, y, entries);
  };

  const menuAtSelection = (windows: boolean) => {
    const tile = tiles().find((candidate) => candidate.path === selected()) ?? tiles()[0];
    if (!tile) return;
    setSelected(tile.path);
    const box = elementOf(tile.path)?.getBoundingClientRect();
    const x = (box?.left ?? 0) + 40;
    const y = (box?.top ?? 0) + Math.min(box?.height ?? 0, 40);
    if (windows) void placesApi.shellPathsMenu([tile.path], x, y, dark(), true).catch(notify);
    else menuFor(tile, x, y);
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
        return handled(), select(at < 0 ? list[0] : tileTowards("down"));
      case "ArrowUp":
        return handled(), select(at < 0 ? list[0] : tileTowards("up"));
      case "Home":
        return handled(), select(list[0]);
      case "End":
        return handled(), select(list[list.length - 1]);
      case "Enter":
        if (event.altKey && current) return handled(), void api.showProperties(current.path).catch(notify);
        if (current) return handled(), open(current, event.ctrlKey);
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

  const TileView = (tileProps: { tile: Tile }) => {
    const tile = () => tileProps.tile;
    return (
      <div
        class="pc-tile"
        classList={{
          selected: selected() === tile().path,
          drive: tile().kind === "drive",
          "drop-target": isDropPlace(tile().path),
          unavailable: tile().kind === "drive" && (tile() as Extract<Tile, { kind: "drive" }>).drive.state !== "ready",
        }}
        role="option"
        aria-selected={selected() === tile().path}
        data-tile={tile().path}
        data-place={tile().path}
        data-place-name={tile().name}
        title={tile().kind === "drive" ? ((tile() as Extract<Tile, { kind: "drive" }>).drive.remote ?? tile().path) : tile().path}
        onMouseDown={(event) => {
          if (event.button === 0 || event.button === 2) setSelected(tile().path);
        }}
        onDblClick={() => open(tile())}
        onAuxClick={(event) => {
          if (event.button === 1) open(tile(), true);
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          event.stopPropagation();
          setSelected(tile().path);
          if (event.shiftKey) void placesApi.shellPathsMenu([tile().path], event.clientX, event.clientY, dark(), true).catch(notify);
          else menuFor(tile(), event.clientX, event.clientY);
        }}
      >
        <span class="pc-tile-icon">
          <Show
            when={tile().kind === "drive" && (tile() as Extract<Tile, { kind: "drive" }>).drive}
            fallback={<Icon name={(() => {
              const kind = (tile() as Extract<Tile, { kind: "folder" }>).folder.kind;
              return hasIcon(kind) ? kind : "folder";
            })()} size={32} />}
          >
            {(drive) => <Icon name={driveIcon(drive())} size={32} />}
          </Show>
        </span>
        <span class="pc-tile-body">
          <span class="pc-tile-name">{tile().name}</span>
          <Show when={tile().kind === "drive" && (tile() as Extract<Tile, { kind: "drive" }>).drive} fallback={<span class="pc-tile-detail">{tile().path}</span>}>
            {(drive) => (
              <>
                <Show when={drive().state === "ready" && drive().total > 0}>
                  <span class="meter pc-meter" classList={{ full: isNearlyFull(drive()) }} role="meter" aria-label={`${Math.round(usedShare(drive()) * 100)} % used`} aria-valuenow={Math.round(usedShare(drive()) * 100)} aria-valuemin={0} aria-valuemax={100}>
                    <span style={{ width: `${usedShare(drive()) * 100}%` }} />
                  </span>
                </Show>
                <span class="pc-tile-detail">
                  <span class="pc-free">{driveDetail(drive())}</span>
                  <Show when={drive().file_system}>
                    <span class="pc-fs">{drive().file_system}</span>
                  </Show>
                </span>
              </>
            )}
          </Show>
        </span>
      </div>
    );
  };

  const Group = (groupProps: { title: string; tiles: Tile[]; label: string }) => (
    <Show when={groupProps.tiles.length > 0}>
      <section class="pc-group" aria-label={groupProps.label}>
        <h2 class="pc-group-title">
          {groupProps.title}
          <span class="pc-group-count">{groupProps.tiles.length}</span>
        </h2>
        <div class="pc-tiles" role="listbox" aria-label={groupProps.title}>
          <For each={groupProps.tiles}>{(tile) => <TileView tile={tile} />}</For>
        </div>
      </section>
    </Show>
  );

  return (
    <div
      ref={page}
      class="pc-page"
      tabindex="0"
      aria-label="This PC"
      onKeyDown={onKeyDown}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) setSelected(null);
      }}
      onContextMenu={(event) => event.preventDefault()}
    >
      <Group title="Folders" label="Folders" tiles={folders()} />
      <Group title="Devices and drives" label="Devices and drives" tiles={local()} />
      <Group title="Network locations" label="Network locations" tiles={network()} />
      <Show when={thisPcLoaded() && tiles().length === 0}>
        <p class="pc-empty">No drives found.</p>
      </Show>
    </div>
  );
}
