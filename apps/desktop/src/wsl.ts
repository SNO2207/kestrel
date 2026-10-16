/**
 * WSL's Linux distributions (`src-tauri/src/wsl.rs`): listed in This PC and the sidebar when WSL
 * is installed (from the registry, which starts nothing), browsed like folders through WSL's own
 * share, `\\wsl.localhost\<distro>`. This PC adopts each distribution's root, so Up from it
 * goes to This PC and its crumb leads the address. Inside one, the Attributes column shows each
 * item's Linux permissions where WSL gives them, asked for the rows on screen only.
 */
import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

import type { Row } from "./lib/api";
import { searchApi, type Distro } from "./lib/searchApi";

const [distros, setDistros] = createSignal<Distro[]>([]);
/** The installed distributions; none without WSL. */
export { distros as wslDistros };

const comparable = (path: string) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();

/** Whether a path is in WSL: on its share, or in a distribution listed (or stood in by a test). */
export function isWslPath(path: string): boolean {
  const lower = comparable(path);
  if (lower.startsWith("\\\\wsl.localhost\\") || lower.startsWith("\\\\wsl$\\")) return true;
  return distros().some((distro) => lower === comparable(distro.path) || lower.startsWith(`${comparable(distro.path)}\\`));
}

/** The distribution whose root `path` is, if any. */
export const distroAt = (path: string): Distro | undefined => distros().find((distro) => comparable(distro.path) === comparable(path));

/** Whether a path is a distribution's root, which This PC adopts. */
export const isWslRoot = (path: string) => distroAt(path) !== undefined;

/** A distribution's root as the breadcrumb and its tab call it: its name. */
export const wslTitle = (root: string): string | null => distroAt(root)?.name ?? null;

/** Reads the distributions again. */
export async function loadWsl() {
  try {
    setDistros(await searchApi.wslDistros());
  } catch (error) {
    console.error("WSL:", error);
  }
}

let started = false;

/** Reads the distributions, and again whenever the window comes back to the front. */
export function startWsl() {
  if (started) return;
  started = true;
  void loadWsl();
  window.addEventListener("focus", () => void loadWsl());
  // Development builds let the end-to-end tests read the list again after standing one in.
  if (import.meta.env.DEV) Object.assign(window, { __kestrelWsl: { reload: loadWsl, distros: () => distros() } });
}

// ---------- Linux permissions ----------

/** Permissions of items in WSL, by lower-cased path: a string such as `-rw-r--r--`, or null. */
const [modes, setModes] = createStore<Record<string, string | null>>({});
const asked = new Set<string>();

/** Asks for the permissions of the rows in WSL not asked about yet. */
export function checkModes(rows: Row[]) {
  const wanted = rows.map((row) => row.path).filter((path) => isWslPath(path) && !asked.has(path.toLowerCase()));
  if (wanted.length === 0) return;
  for (const path of wanted) asked.add(path.toLowerCase());
  searchApi
    .wslModes(wanted)
    .then((answers) => answers.forEach((mode, i) => setModes(wanted[i].toLowerCase(), mode)))
    .catch(() => wanted.forEach((path) => asked.delete(path.toLowerCase())));
}

/** A row's Linux permissions, once known. */
export const linuxMode = (row: Row): string | null => modes[row.path.toLowerCase()] ?? null;
