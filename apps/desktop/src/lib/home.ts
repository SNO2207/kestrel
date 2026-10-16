/**
 * Home, Explorer's start page, as a place a tab can be: its address is "Home" (typed into the
 * address bar, it goes there, as in Explorer). It lists no folder; `HomeView` draws it.
 *
 * Kept small and on its own so the registry of special locations (This PC, the Recycle Bin)
 * can take it in: a location is a name `isHome` recognises, and a view that stands in for the
 * file list while a tab is there.
 */
import { folderOptions } from "./folderOptions";
import { parityApi } from "./parityApi";

/** Home's address. */
export const HOME_PATH = "Home";

/** Whether `path` is Home's address: "Home" in any case, as typed or as a crumb gives it. */
export const isHome = (path: string | null | undefined): boolean =>
  typeof path === "string" && /^(home|shell:home)\\*$/i.test(path.trim());

/** Whether a tab shows Home now: it is there, and not showing search results from there. */
export const showsHome = (tab: { path: string; listing: unknown }): boolean => isHome(tab.path) && tab.listing === null;

/**
 * Where the window's first tab opens: the folder it was opened to show, if another window
 * opened it (Ctrl+N), otherwise where Folder Options say (Home, or `profile`).
 */
export async function firstLocation(profile: string): Promise<string> {
  const given = await parityApi.windowStart().catch(() => null);
  if (given) return given;
  return folderOptions().start === "home" ? HOME_PATH : profile;
}
