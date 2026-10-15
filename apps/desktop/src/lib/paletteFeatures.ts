/**
 * The palette's providers for the features built beside it: projects (`@`), tags (`#`), and
 * the shortcuts that take part in every query (pinned places, frequent folders, saved searches
 * and workspaces).
 *
 * Their items come from backend providers of the same ids (`projects/provider.rs`,
 * `shortcuts/provider.rs`), so both windows list them; these halves say what choosing one does.
 * That happens in the main window, except opening a pinned file, which needs no window of
 * Kestrel's and so runs in the quick launcher itself, as opening any file does.
 */
import { closeProjects, openProjects, selectProject, showInKestrel } from "../projects";
import { openPin, openPlace, restoreWorkspace, runSavedSearch, searchTag, showInFolder } from "../shortcuts";
import { notify, openTab } from "../state";
import { api } from "./api";
import type { PaletteHit, PaletteProvider } from "./palette";
import { COLORS, shortcutsApi, type Color } from "./shortcutsApi";

const pathOf = (hit: PaletteHit) => hit.path ?? hit.id;

/** What Tab puts in the input for a folder or file: its path after `/`, as for the index's. */
function pathCompletion(hit: PaletteHit): string | null {
  const path = hit.path;
  if (!path) return null;
  return `/${hit.dir && !path.endsWith("\\") ? `${path}\\` : path}`;
}

const projects: PaletteProvider = {
  id: "projects",
  prefix: "@",
  label: () => "Project",
  revealLabel: "show in Projects",
  run: (hit, mode) => {
    const root = pathOf(hit);
    if (mode === "reveal") {
      openProjects();
      return selectProject(root);
    }
    if (mode !== "new-tab") return showInKestrel(root);
    closeProjects();
    openTab(root);
  },
  complete: pathCompletion,
};

const tags: PaletteProvider = {
  id: "tags",
  prefix: "#",
  inDefault: false,
  label: () => "Tag",
  run: (hit, mode) => {
    const named = hit.icon?.startsWith("tag:") ? hit.icon.slice(4) : "";
    const color: Color = COLORS.find((each) => each === named) ?? "gray";
    searchTag({ id: Number(hit.id), name: hit.title, color }, mode === "new-tab");
  },
  complete: (hit) => `#${hit.title}`,
};

const pins: PaletteProvider = {
  id: "pins",
  label: (hit) => (!hit.path ? "Pin not found" : hit.dir ? "Pinned folder" : "Pinned file"),
  run: async (hit, mode) => {
    if (hit.path && !hit.dir && mode !== "reveal") return void api.openPath(hit.path).catch(notify);
    const pin = (await shortcutsApi.board()).pins.find((each) => String(each.id) === hit.id);
    if (!pin) return notify("That pin is no longer in the sidebar.");
    if (mode === "reveal" && !pin.missing) return showInFolder(pin.path);
    await openPin(pin, mode === "new-tab");
  },
  runsAnywhere: (hit, mode) => !!hit.path && !hit.dir && mode !== "reveal",
  complete: pathCompletion,
};

const frequent: PaletteProvider = {
  id: "frequent",
  label: () => "Frequent folder",
  run: (hit, mode) => {
    const path = pathOf(hit);
    if (mode === "reveal") return showInFolder(path);
    openPlace(path, true, mode === "new-tab");
  },
  complete: pathCompletion,
};

const searches: PaletteProvider = {
  id: "searches",
  label: () => "Saved search",
  run: async (hit, mode) => {
    const saved = (await shortcutsApi.searches()).find((each) => String(each.id) === hit.id);
    if (!saved) return notify("That saved search no longer exists.");
    await runSavedSearch(saved, mode === "new-tab");
  },
};

const workspaces: PaletteProvider = {
  id: "workspaces",
  label: () => "Workspace",
  run: async (hit) => {
    const workspace = (await shortcutsApi.workspaces()).find((each) => String(each.id) === hit.id);
    if (!workspace) return notify("That workspace no longer exists.");
    restoreWorkspace(workspace);
  },
};

/** Every feature provider, to register in both windows beside the actions and files. */
export const featureProviders = (): PaletteProvider[] => [projects, tags, pins, frequent, searches, workspaces];
