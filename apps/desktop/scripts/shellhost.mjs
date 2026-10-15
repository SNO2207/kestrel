// Builds kestrel-shellhost.exe, the helper that shows the Windows context menu, into the same
// folder as the kestrel.exe that `tauri build` is about to produce: Kestrel looks for it next
// to itself. Run by `beforeBuildCommand` (tauri.conf.json); `pnpm dev` builds the debug one.
//
// The Tauri CLI says whether the build is a debug one in TAURI_ENV_DEBUG. The helper is built
// for this computer, as the app is without `--target`, and in the same CARGO_TARGET_DIR.

import { spawnSync } from "node:child_process";

const debug = process.env.TAURI_ENV_DEBUG === "true";
const args = ["build", "-p", "kestrel-shellhost", ...(debug ? [] : ["--release"])];
console.log(`cargo ${args.join(" ")}`);
const built = spawnSync("cargo", args, { stdio: "inherit", shell: false });
process.exit(built.status ?? 1);
