// Ports and folders for one isolated dev instance of Kestrel ("slot"), so several can run at
// once, for example one per agent working in its own git worktree. Slot 0 is the default
// instance described in the README.

import { join } from "node:path";
import { tmpdir } from "node:os";

export const slot = Number(process.env.KESTREL_SLOT ?? "0");

if (!Number.isInteger(slot) || slot < 0 || slot > 20) {
  throw new Error(`KESTREL_SLOT must be a whole number from 0 to 20, not ${process.env.KESTREL_SLOT}`);
}

/** The Vite dev server's port. */
export const vitePort = 1420 + slot * 10;
/** The web view's debugging port. 9222 is avoided: other WebView2 apps commonly hold it. */
export const cdpPort = Number(process.env.KESTREL_CDP_PORT ?? 9333 + slot);
export const devUrl = `http://localhost:${vitePort}`;
/** Each slot has its own app identity, so its web view keeps separate browser data. */
export const identifier = slot === 0 ? "dev.kestrel.explorer" : `dev.kestrel.explorer.slot${slot}`;
/** Where the slot keeps its index snapshot and database; slot 0 uses the app's default. */
export const dataDir = slot === 0 ? null : join(tmpdir(), "kestrel-slots", `slot${slot}`);
