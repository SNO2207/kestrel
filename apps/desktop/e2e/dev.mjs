// Starts and stops an isolated instance of Kestrel for end-to-end tests.
//
//   node e2e/dev.mjs start    builds if needed, launches, and returns once the window is up
//   node e2e/dev.mjs stop     stops the instance and everything it started
//   node e2e/dev.mjs status   says whether it is running
//
// KESTREL_SLOT picks the instance (default 0); each slot has its own ports, app identity and
// data folder, so agents in separate worktrees can test at the same time. The instance runs
// detached and logs to e2e/out/slot<N>.log, so `start` can be called from a foreground shell.
//
// With KESTREL_RELEASE=1 the instance is a release build: `start` runs the same `tauri build`
// as `pnpm tauri build` (no installer bundling), with the slot's identity, and starts the
// kestrel.exe it produced, which serves its own front end. Pass the same variable to the
// suites. The build turns on the end-to-end hooks of a release build (`KESTREL_TEST_HOOKS`),
// which only the Windows menu's tests need; drag-and-drop tests need a dev build.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { appExe, appUrl, cdpPort, dataDir, devUrl, identifier, release, slot, targetDir, vitePort } from "./slot.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const app = join(here, "..");
const out = join(here, "out");
mkdirSync(out, { recursive: true });
const pidFile = join(out, `slot${slot}.pid`);
const logFile = join(out, `slot${slot}.log`);
const tauri = join(app, "node_modules", "@tauri-apps", "cli", "tauri.js");
/** First builds of the Rust backend can take several minutes; a release build longer. */
const START_TIMEOUT_MS = 20 * 60 * 1000;

async function windowIsUp() {
  try {
    const response = await fetch(`http://127.0.0.1:${cdpPort}/json/list`, { signal: AbortSignal.timeout(1500) });
    const pages = await response.json();
    return pages.some((page) => page.title === "Kestrel" && page.url.startsWith(appUrl));
  } catch {
    return false;
  }
}

function portOwners(ports) {
  const script = `Get-NetTCPConnection -LocalPort ${ports.join(",")} -State Listen -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique`;
  try {
    return execFileSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" })
      .split(/\s+/)
      .filter(Boolean)
      .map(Number);
  } catch {
    return [];
  }
}

function kill(pid) {
  try {
    execFileSync("taskkill", ["/T", "/F", "/PID", String(pid)], { stdio: "ignore" });
  } catch {
    // Already gone.
  }
}

async function stop() {
  if (existsSync(pidFile)) {
    kill(Number(readFileSync(pidFile, "utf8")));
    rmSync(pidFile, { force: true });
  }
  // The app outlives its launcher if the launcher was killed first, so also stop whatever
  // still holds this slot's ports: the dev server and the web view (whose parent is the app).
  for (const pid of portOwners([vitePort, cdpPort])) kill(pid);
  // Either build of the slot, whichever mode started it.
  const exes = ["debug", "release"].map((profile) => join(targetDir, profile, "kestrel.exe").toLowerCase().replace(/'/g, "''"));
  const script = `Get-CimInstance Win32_Process -Filter "Name = 'kestrel.exe'" | Where-Object { $_.ExecutablePath -and @('${exes.join("','")}') -contains $_.ExecutablePath.ToLower() } | Select-Object -ExpandProperty ProcessId`;
  for (const pid of execFileSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" }).split(/\s+/).filter(Boolean)) {
    kill(Number(pid));
  }
  console.log(`slot ${slot} stopped`);
}

/** Builds the release executable with the slot's identity, as `pnpm tauri build` does. */
function buildRelease(env, log) {
  const config = { identifier };
  const built = spawnSync(process.execPath, [tauri, "build", "--no-bundle", "--config", JSON.stringify(config)], {
    cwd: app,
    env,
    stdio: ["ignore", log, log],
    windowsHide: true,
    timeout: START_TIMEOUT_MS,
  });
  if (built.status !== 0 || !existsSync(appExe)) {
    const tail = readFileSync(logFile, "utf8").split("\n").slice(-30).join("\n");
    throw new Error(`the release build of slot ${slot} failed (exit ${built.status}). Log tail:\n${tail}`);
  }
}

async function start() {
  if (await windowIsUp()) {
    console.log(`slot ${slot} already running at ${appUrl} (debugging port ${cdpPort})`);
    return;
  }
  if (portOwners([vitePort, cdpPort]).length) {
    throw new Error(`ports ${vitePort}/${cdpPort} are in use by something else; run \`node e2e/dev.mjs stop\` or pick another KESTREL_SLOT`);
  }
  if (dataDir) mkdirSync(dataDir, { recursive: true });
  const env = {
    ...process.env,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}`,
    CARGO_TARGET_DIR: targetDir,
    ...(dataDir ? { KESTREL_DATA_DIR: dataDir } : {}),
    // Test instances run side by side and must not fight over (or take from the owner) the
    // quick launcher's system-wide shortcut. Set it yourself to test the real one.
    KESTREL_GLOBAL_HOTKEY: process.env.KESTREL_GLOBAL_HOTKEY ?? "off",
    // Nor do they read the owner's photos and videos in the background: the media library starts
    // with no folders (media.mjs gives it a scratch folder). Set it to `default` to test the real ones.
    KESTREL_MEDIA_SCOPES: process.env.KESTREL_MEDIA_SCOPES ?? "none",
  };
  const log = openSync(logFile, "w");
  const started = Date.now();
  let child;
  if (release) {
    buildRelease(env, log);
    child = spawn(appExe, [], {
      cwd: dirname(appExe),
      env: { ...env, KESTREL_TEST_HOOKS: "1" },
      detached: true,
      stdio: ["ignore", log, log],
    });
  } else {
    const config = {
      identifier,
      build: { devUrl, beforeDevCommand: `pnpm dev --port ${vitePort} --strictPort` },
    };
    child = spawn(process.execPath, [tauri, "dev", "--config", JSON.stringify(config)], {
      cwd: app,
      env,
      detached: true,
      stdio: ["ignore", log, log],
      windowsHide: true,
    });
  }
  writeFileSync(pidFile, String(child.pid));
  child.unref();

  while (!(await windowIsUp())) {
    if (Date.now() - started > START_TIMEOUT_MS) {
      const tail = readFileSync(logFile, "utf8").split("\n").slice(-30).join("\n");
      await stop();
      throw new Error(`slot ${slot} did not start within ${START_TIMEOUT_MS / 60000} minutes. Log tail:\n${tail}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  const mode = release ? `release build ${appExe}` : appUrl;
  console.log(`slot ${slot} ready: ${mode} (debugging port ${cdpPort}) after ${Math.round((Date.now() - started) / 1000)} s; log: ${logFile}`);
}

const command = process.argv[2];
if (command === "start") await start();
else if (command === "stop") await stop();
else if (command === "status") console.log((await windowIsUp()) ? `slot ${slot} running` : `slot ${slot} not running`);
else {
  console.log("usage: node e2e/dev.mjs start|stop|status   (KESTREL_SLOT selects the instance, KESTREL_RELEASE=1 a release build)");
  process.exitCode = 1;
}
