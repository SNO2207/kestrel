/**
 * Libraries (`shell:Libraries`), as Explorer shows them: Documents, Music, Pictures, Videos and
 * any the owner made, each opening as one folder holding the items of every folder it includes
 * (`shell:Libraries\Documents`, listed by the backend's `parity2::libraries`). A library's
 * Properties are Windows' own (its `.library-ms` file's), where its folders are chosen.
 */
import { createEffect, on } from "solid-js";

import { api } from "../lib/api";
import type { LocationPageProps } from "../lib/locations";
import type { Library } from "../lib/parity2Api";
import { placesApi } from "../lib/placesApi";
import { libraries, loadLibraries } from "../parity2";
import { openLocation } from "../places";
import { pinPaths } from "../shortcuts";
import { notify } from "../state";
import type { MenuEntry } from "./ContextMenu";
import type { IconName } from "./Icon";
import { showMenu } from "./ShortcutDialogs";
import { TilePage, type Tile } from "./TilePage";

const dark = () => !window.matchMedia("(prefers-color-scheme: light)").matches;

/** A library's icon, by which of Windows' libraries it is. */
export const libraryIcon = (library: Pick<Library, "kind">): IconName => (library.kind === "library" ? "library" : library.kind);

/** What a library's tile says under its name: where it keeps its items. */
function describe(library: Library): string {
  const count = library.folders.length;
  if (count === 0) return library.missing.length > 0 ? "Its folders aren't there now" : "No folders yet";
  return count === 1 ? library.folders[0] : `${count} folders`;
}

export function LibrariesPage(props: LocationPageProps) {
  // Read again whenever the page is shown or refreshed.
  createEffect(on(() => props.revision, () => void loadLibraries()));

  const tiles = (): Tile[] =>
    libraries().map((library) => ({
      path: library.address,
      name: library.name,
      detail: describe(library),
      icon: libraryIcon(library),
      title: [library.name, ...library.folders].join("\n"),
    }));
  const libraryAt = (address: string) => libraries().find((library) => library.address === address);

  const menuFor = (tile: Tile | null, x: number, y: number, windows: boolean) => {
    const library = tile && libraryAt(tile.path);
    if (!tile || !library) {
      return showMenu(x, y, [{ label: "Refresh", hint: "F5", run: () => void loadLibraries() }]);
    }
    if (windows) return void placesApi.shellPathsMenu([library.file], x, y, dark(), true).catch(notify);
    const entries: MenuEntry[] = [
      { label: "Open", hint: "Enter", run: () => openLocation(library.address) },
      { label: "Open in new tab", run: () => openLocation(library.address, true) },
      "separator",
      { label: "Pin to sidebar", disabled: !library.save_folder, run: () => library.save_folder && void pinPaths([library.save_folder]) },
      "separator",
      { label: "Properties", hint: "Alt+Enter", run: () => void api.showProperties(library.file).catch(notify) },
      "separator",
      { label: "Show more options", hint: "Shift+F10", run: () => void placesApi.shellPathsMenu([library.file], x, y, dark(), false).catch(notify) },
    ];
    showMenu(x, y, entries);
  };

  return (
    <TilePage
      page={props}
      label="Libraries"
      groups={[{ title: "Libraries", tiles: tiles(), empty: <p class="pc-empty">This account has no libraries.</p> }]}
      onOpen={(tile, inNewTab) => openLocation(tile.path, inNewTab)}
      menuFor={menuFor}
    />
  );
}
