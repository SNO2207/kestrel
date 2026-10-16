/**
 * Home, Explorer's start page, and where a window opens (`openLayout` in panes.ts asks).
 *
 * Home is a special location (lib/locations.ts, registered in placesSetup.ts): its address is
 * "Home" (typed into the address bar, it goes there, as in Explorer), it lists no folder, and
 * `HomeView` draws it in place of the file list.
 */
import { folderOptions, type StartLocation } from "./folderOptions";
import { locationOf } from "./locations";
import { parityApi } from "./parityApi";

/** Home's address. */
export const HOME_PATH = "Home";
/** This PC's address, as `places.ts` names it; repeated here so this file stays light. */
const THIS_PC = "shell:MyComputerFolder";

/** The other spellings of Home the address bar takes: Explorer's shell name and its class id. */
export const HOME_ALIASES = ["Home\\", "shell:Home", "shell:::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}"];

/** Whether `path` is Home's address, in any of its spellings. */
export const isHome = (path: string | null | undefined): boolean =>
  typeof path === "string" && locationOf(path)?.address === HOME_PATH;

/** Where Folder Options say a window opens: Home, This PC, or the user folder (`profile`). */
export function startLocation(profile: string, start: StartLocation = folderOptions().start): string {
  if (start === "home") return HOME_PATH;
  if (start === "this-pc") return THIS_PC;
  return profile;
}

/** The folder this window was opened to show, if another window opened it (Ctrl+N). */
export const givenLocation = (): Promise<string | null> => parityApi.windowStart().catch(() => null);
