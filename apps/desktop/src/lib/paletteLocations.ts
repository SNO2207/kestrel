/**
 * The palette's special locations (lib/locations.ts): This PC, the Recycle Bin and any other a
 * feature registers, matched by title, by the names the address bar takes for them, and by a
 * few words of their own. Enter goes there, Ctrl+Enter in a new tab. The items are the front
 * end's own, so this provider has no backend half.
 */
import { closeProjects } from "../projects";
import { activeTab, navigate, openTab } from "../state";
import { allLocations, locationTitle } from "./locations";
import type { PaletteItem, PaletteProvider } from "./palette";

const ID = "locations";

export const locationsProvider: PaletteProvider = {
  id: ID,
  label: () => "Location",
  items: () =>
    allLocations()
      .filter((location) => location.inPalette !== false)
      .map(
        (location): PaletteItem => ({
          provider: ID,
          id: location.address,
          title: locationTitle(location.address) ?? location.address,
          icon: location.icon,
          keywords: [location.keywords ?? "", ...(location.aliases ?? [])].join(" "),
        }),
      ),
  run: (hit, mode) => {
    closeProjects();
    const tab = activeTab();
    if (mode === "new-tab" || !tab) openTab(hit.id);
    else navigate(tab.id, hit.id);
  },
  revealLabel: "open",
  complete: () => null,
};
