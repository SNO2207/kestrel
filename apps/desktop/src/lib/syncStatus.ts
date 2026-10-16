/**
 * Sync status, as Explorer's Status column shows it for OneDrive and other cloud folders:
 * online-only (a cloud), available on this device (a check in a ring), or always kept on this
 * device (a filled check). It comes from each item's attributes, which listings carry, and is
 * shown only inside the folders a provider keeps in sync (`parity/syncroots.rs`) while the
 * Folder Options say so. Nothing is read from the files, so nothing is downloaded.
 */
import { createSignal } from "solid-js";

import type { Row } from "./api";
import { folderOptions } from "./folderOptions";
import { parityApi } from "./parityApi";

const OFFLINE = 0x1000;
const RECALL_ON_OPEN = 0x4_0000;
const PINNED = 0x8_0000;
const UNPINNED = 0x10_0000;
const RECALL_ON_DATA_ACCESS = 0x40_0000;
const ONLINE_ONLY = OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS | UNPINNED;

export type SyncState = "cloud" | "local" | "pinned";

export const SYNC_LABELS: Record<SyncState, string> = {
  cloud: "Available when online",
  local: "Available on this device",
  pinned: "Always available on this device",
};

const [roots, setRoots] = createSignal<string[]>([]);
/** Folders added by the end-to-end tests, which can't make a real sync root. */
const [testRoots, setTestRoots] = createSignal<string[]>([]);
let asked = false;

function ask() {
  if (asked) return;
  asked = true;
  void parityApi
    .syncRoots()
    .then((found) => setRoots(found.map((root) => root.toLowerCase())))
    .catch(() => {});
}

/** Whether `path` is in a synced folder. */
function inSyncRoot(path: string): boolean {
  ask();
  const lower = path.toLowerCase();
  return [...roots(), ...testRoots()].some((root) => lower === root || lower.startsWith(`${root}\\`));
}

/** An item's sync status, or `null` when it has none (or the option is off). */
export function syncState(row: Pick<Row, "path" | "dir" | "attrs"> | undefined): SyncState | null {
  if (!row || !folderOptions().syncStatus || row.attrs === undefined || !inSyncRoot(row.path)) return null;
  if (row.attrs & PINNED) return "pinned";
  if (row.attrs & ONLINE_ONLY) return "cloud";
  // A folder's status would be what everything inside it is, which is not known here.
  return row.dir ? null : "local";
}

// Development builds let the end-to-end tests treat a scratch folder as synced.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelSync: {
      setTestRoots: (paths: string[]) => setTestRoots(paths.map((path) => path.replace(/\\+$/, "").toLowerCase())),
    },
  });
}
