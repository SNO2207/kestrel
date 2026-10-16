/**
 * The special locations of Explorer parity's second round (lib/locations.ts): Libraries and the
 * Network, with a computer's page (`\\NAME`) as a family of the Network's. A library
 * (`shell:Libraries\Documents`) and a share (`\\NAME\share`) are listed as folders, not pages;
 * these locations give them their breadcrumbs. Registered by placesSetup.ts after its own, so
 * the quick launcher's palette lists them too.
 */
import { NETWORK, NetworkLocationPage, computerIn } from "./components/NetworkPage";
import { LibrariesPage, libraryIcon } from "./components/LibrariesPage";
import { registerLocation, type Crumb } from "./lib/locations";
import { libraries } from "./parity2";

/** The list of libraries; a library is `shell:Libraries\<name>`. */
export const LIBRARIES = "shell:Libraries";
const LIBRARY = /^(?:shell:)?libraries\\([^\\]+)\\?$/i;

/** The crumbs of a library: Libraries, then the library. */
function libraryCrumbs(address: string): Crumb[] | null {
  const name = LIBRARY.exec(address.trim())?.[1];
  if (!name) return null;
  const library = libraries().find((each) => each.name.toLowerCase() === name.toLowerCase());
  return [
    { label: "Libraries", path: LIBRARIES, icon: "library" },
    { label: library?.name ?? name, path: `${LIBRARIES}\\${library?.name ?? name}`, icon: library ? libraryIcon(library) : "library" },
  ];
}

/** The crumbs of a path on a share: Network, the computer, then each folder. */
function shareCrumbs(address: string): Crumb[] | null {
  const match = /^\\\\([^\\/]+)\\(.+)$/.exec(address.trim());
  if (!match || match[2].replace(/\\+$/, "") === "") return null;
  const [, server, rest] = match;
  const crumbs: Crumb[] = [
    { label: "Network", path: NETWORK, icon: "network" },
    { label: server, path: `\\\\${server}`, icon: "pc" },
  ];
  let path = `\\\\${server}`;
  for (const part of rest.split("\\").filter(Boolean)) {
    path = `${path}\\${part}`;
    crumbs.push({ label: part, path });
  }
  return crumbs;
}

export function registerParity2Locations() {
  registerLocation({
    address: LIBRARIES,
    title: "Libraries",
    icon: "library",
    page: LibrariesPage,
    aliases: ["Libraries", "::{031E4825-7B94-4dc3-B131-E946B44C8DD5}", "shell:::{031E4825-7B94-4dc3-B131-E946B44C8DD5}"],
    keywords: "documents music pictures videos library",
    descendantCrumbs: libraryCrumbs,
  });

  registerLocation({
    address: NETWORK,
    title: (address) => computerIn(address) ?? "Network",
    icon: "network",
    page: NetworkLocationPage,
    aliases: ["Network", "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}", "shell:::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}"],
    keywords: "computers shares servers nas map network drive",
    // A computer, `\\NAME`, is a page of its shares; Up from it is the Network.
    claims: (address) => computerIn(address) !== null,
    parent: (address) => (computerIn(address) ? NETWORK : null),
    crumbs: (address) => {
      const computer = computerIn(address);
      const network: Crumb = { label: "Network", path: NETWORK, icon: "network" };
      return computer ? [network, { label: computer, path: `\\\\${computer}`, icon: "pc" }] : [network];
    },
    descendantCrumbs: shareCrumbs,
  });
}
