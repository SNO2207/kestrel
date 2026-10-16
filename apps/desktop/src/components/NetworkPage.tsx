/**
 * The Network location (`shell:NetworkPlacesFolder`), as Explorer's: the computers found on the
 * network and the network locations made by "Add a network location"; and a computer's own page
 * (`\\NAME`), its shares, each opening as a folder.
 *
 * Computers are found by the backend (`parity2::network`) on a thread of its own, with a time
 * limit: the page shows what was found in time, and the rest as it comes (`network-changed`).
 * On a network where Windows' computer browsing finds nothing (most home networks without SMB 1),
 * the page says so and offers typing `\\name` in the address bar, which always works.
 */
import { Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { api } from "../lib/api";
import type { LocationPageProps } from "../lib/locations";
import { parity2Api, type Neighbourhood, type NetworkPlace } from "../lib/parity2Api";
import { placesApi } from "../lib/placesApi";
import { showHidden } from "../lib/views";
import { addNetworkLocation, mapNetworkDrive, networkEntries } from "../parity2";
import { openLocation } from "../places";
import { pinPaths } from "../shortcuts";
import { notify } from "../state";
import type { MenuEntry } from "./ContextMenu";
import { showMenu } from "./ShortcutDialogs";
import { TilePage, type Tile } from "./TilePage";

/** The Network location's address, Explorer's own name for it. */
export const NETWORK = "shell:NetworkPlacesFolder";

const dark = () => !window.matchMedia("(prefers-color-scheme: light)").matches;

/** The name of the computer at `\\NAME`, or null for anything else. */
export const computerIn = (address: string): string | null => /^\\\\([^\\/]+)\\?$/.exec(address.trim())?.[1] ?? null;

/** The page's menu for a share or a network location (a folder), and for nothing in particular. */
function folderMenu(path: string, x: number, y: number, windows: boolean, refresh: () => void, more: MenuEntry[] = []) {
  if (windows) return void placesApi.shellPathsMenu([path], x, y, dark(), true).catch(notify);
  showMenu(x, y, [
    { label: "Open", hint: "Enter", run: () => openLocation(path) },
    { label: "Open in new tab", run: () => openLocation(path, true) },
    ...more,
    "separator",
    { label: "Pin to sidebar", run: () => void pinPaths([path]) },
    { label: "Properties", hint: "Alt+Enter", run: () => void api.showProperties(path).catch(notify) },
    "separator",
    { label: "Refresh", hint: "F5", run: refresh },
    { label: "Show more options", hint: "Shift+F10", run: () => void placesApi.shellPathsMenu([path], x, y, dark(), false).catch(notify) },
  ]);
}

export function NetworkPage(props: LocationPageProps) {
  const [found, setFound] = createSignal<Neighbourhood | null>(null);
  const [places, setPlaces] = createSignal<NetworkPlace[]>([]);

  const read = (fresh: boolean) => {
    setFound((before) => (before ? { ...before, complete: false } : null));
    void parity2Api.computers(fresh).then(setFound).catch(notify);
    void parity2Api.networkPlaces().then(setPlaces).catch(() => setPlaces([]));
  };
  // Refresh (F5) walks the network again; showing the page again reuses a recent walk.
  let first = true;
  createEffect(
    on(
      () => props.revision,
      () => {
        read(!first);
        first = false;
      },
    ),
  );
  onMount(() => {
    const stop = parity2Api.onNetworkChanged(setFound);
    onCleanup(() => void stop.then((unlisten) => unlisten()));
  });

  const computers = (): Tile[] =>
    (found()?.computers ?? []).map((computer) => ({
      path: computer.path,
      name: computer.name,
      detail: computer.comment || computer.group || "Computer",
      icon: "pc",
    }));
  const locations = (): Tile[] =>
    places().map((place) => ({ path: place.target, name: place.name, detail: place.target, icon: "share", place: true }));

  const menuFor = (tile: Tile | null, x: number, y: number, windows: boolean) => {
    const page: MenuEntry[] = [
      ...networkEntries(),
      "separator",
      { label: "Refresh", hint: "F5", run: () => read(true) },
    ];
    if (!tile) return showMenu(x, y, page);
    if (computerIn(tile.path)) {
      return showMenu(x, y, [
        { label: "Open", hint: "Enter", run: () => openLocation(tile.path) },
        { label: "Open in new tab", run: () => openLocation(tile.path, true) },
        "separator",
        { label: "Map network drive…", run: () => void mapNetworkDrive(tile.path) },
        "separator",
        ...page,
      ]);
    }
    folderMenu(tile.path, x, y, windows, () => read(true), [{ label: "Map network drive…", run: () => void mapNetworkDrive(tile.path) }]);
  };

  const searching = () => found() === null || !found()!.complete;
  const nothing = (
    <div class="tile-page-note">
      <Show when={!searching()} fallback={<p>Looking for computers on the network…</p>}>
        <p>No computers were found on the network.</p>
        <p class="hint">
          Windows finds computers this way only where the network's computer browsing answers, which most home networks don't. Type a computer's name in the address
          bar, such as <code>\\nas</code>, to see its shares.
          <Show when={found()?.error}>{(error) => <> Windows said: {error()}</>}</Show>
        </p>
      </Show>
    </div>
  );

  return (
    <TilePage
      page={props}
      label="Network"
      groups={[
        { title: "Computers", tiles: computers(), empty: nothing },
        { title: "Network locations", tiles: locations() },
      ]}
      onOpen={(tile, inNewTab) => openLocation(tile.path, inNewTab)}
      menuFor={menuFor}
      after={
        <div class="tile-page-actions">
          <button class="dialog-button" onClick={() => void mapNetworkDrive()}>
            Map network drive…
          </button>
          <button class="dialog-button" onClick={addNetworkLocation}>
            Add a network location
          </button>
        </div>
      }
    />
  );
}

/** The Network location's page: the Network itself, or a computer's at `\\NAME`. */
export function NetworkLocationPage(props: LocationPageProps) {
  return (
    <Show when={computerIn(props.address)} fallback={<NetworkPage {...props} />}>
      <ComputerPage {...props} />
    </Show>
  );
}

/** A computer's page, `\\NAME`: its shares, each opening as a folder. */
export function ComputerPage(props: LocationPageProps) {
  const [shares, setShares] = createSignal<string[] | null>(null);
  const [problem, setProblem] = createSignal<string | null>(null);
  const name = () => computerIn(props.address) ?? props.address;

  const read = () => {
    setProblem(null);
    void parity2Api
      .shares(name(), showHidden())
      .then((found) => setShares(found))
      .catch((error) => {
        setShares([]);
        setProblem(String(error));
      });
  };
  createEffect(on([() => props.revision, () => props.address, showHidden], read));

  const tiles = (): Tile[] =>
    (shares() ?? []).map((path) => ({ path, name: path.split("\\").pop() ?? path, detail: path, icon: "share", place: true }));

  const menuFor = (tile: Tile | null, x: number, y: number, windows: boolean) => {
    if (!tile) {
      return showMenu(x, y, [
        { label: "Map network drive…", run: () => void mapNetworkDrive(`\\\\${name()}`) },
        "separator",
        { label: "Refresh", hint: "F5", run: read },
      ]);
    }
    folderMenu(tile.path, x, y, windows, read, [{ label: "Map network drive…", run: () => void mapNetworkDrive(tile.path) }]);
  };

  return (
    <TilePage
      page={props}
      label={name()}
      groups={[
        {
          title: "Shared folders",
          tiles: tiles(),
          empty: (
            <div class="tile-page-note">
              <Show when={shares() !== null} fallback={<p>Asking {name()} for its shared folders…</p>}>
                <p>{problem() ?? `${name()} shares no folders this account can see.`}</p>
              </Show>
            </div>
          ),
        },
      ]}
      onOpen={(tile, inNewTab) => openLocation(tile.path, inNewTab)}
      menuFor={menuFor}
    />
  );
}
