// Ports and folders for one isolated dev instance of Kestrel ("slot"), so several can run at
// once, for example one per agent working in its own git worktree. Slot 0 is the default
// instance described in the README.
//
// KESTREL_RELEASE=1 makes the slot a release build (`tauri build`, no bundling) instead of
// `tauri dev`: dev.mjs builds and starts that, and the suites look for its page, which the
// app serves itself rather than the Vite dev server.

import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

export const slot = Number(process.env.KESTREL_SLOT ?? "0");

if (!Number.isInteger(slot) || slot < 0 || slot > 20) {
  throw new Error(`KESTREL_SLOT must be a whole number from 0 to 20, not ${process.env.KESTREL_SLOT}`);
}

/** Whether the slot runs a release build rather than the dev server. */
export const release = process.env.KESTREL_RELEASE === "1";
/** The Vite dev server's port. */
export const vitePort = 1420 + slot * 10;
/** The web view's debugging port. 9222 is avoided: other WebView2 apps commonly hold it. */
export const cdpPort = Number(process.env.KESTREL_CDP_PORT ?? 9333 + slot);
export const devUrl = `http://localhost:${vitePort}`;
/** Where the app's page comes from: the dev server, or in a release build the app itself. */
export const appUrl = release ? "http://tauri.localhost" : devUrl;
/** Each slot has its own app identity, so its web view keeps separate browser data. */
export const identifier = slot === 0 ? "dev.kestrel.explorer" : `dev.kestrel.explorer.slot${slot}`;
/** Where the slot keeps its index snapshot and database; slot 0 uses the app's default. */
export const dataDir = slot === 0 ? null : join(tmpdir(), "kestrel-slots", `slot${slot}`);

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
/**
 * Tauri compiles each instance's settings into its binary, so every slot needs a build of its
 * own; sharing one would make a second slot try to overwrite a running executable.
 */
export const targetDir = slot === 0 ? join(repo, "target") : join(repo, "target", "slots", `slot${slot}`);
/** The slot's executable, debug or release. */
export const appExe = join(targetDir, release ? "release" : "debug", "kestrel.exe");
