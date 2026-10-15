// Starts and stops an isolated dev instance of Kestrel for end-to-end tests.
//
//   node e2e/dev.mjs start    builds if needed, launches, and returns once the window is up
//   node e2e/dev.mjs stop     stops the instance and everything it started
//   node e2e/dev.mjs status   says whether it is running
//
// KESTREL_SLOT picks the instance (default 0); each slot has its own ports, app identity and
// data folder, so agents in separate worktrees can test at the same time. The instance runs
// detached and logs to e2e/out/slot<N>.log, so `start` can be called from a foreground shell.

import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { cdpPort, dataDir, devUrl, identifier, slot, vitePort } from "./slot.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const app = join(here, "..");
const out = join(here, "out");
mkdirSync(out, { recursive: true });
const pidFile = join(out, `slot${slot}.pid`);
// Tauri compiles each instance's settings into its binary, so every slot needs a build of its
// own; sharing one would make a second slot try to overwrite a running executable.
const repo = join(app, "..", "..");
const targetDir = slot === 0 ? join(repo, "target") : join(repo, "target", "slots", `slot${slot}`);
const appExe = join(targetDir, "debug", "kestrel.exe");
const logFile = join(out, `slot${slot}.log`);
/** First builds of the Rust backend can take several minutes. */
const START_TIMEOUT_MS = 15 * 60 * 1000;

async function windowIsUp() {
  try {
    const response = await fetch(`http://127.0.0.1:${cdpPort}/json/list`, { signal: AbortSignal.timeout(1500) });
    const pages = await response.json();
    return pages.some((page) => page.title === "Kestrel" && page.url.startsWith(devUrl));
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
  const exe = appExe.toLowerCase().replace(/'/g, "''");
  const script = `Get-CimInstance Win32_Process -Filter "Name = 'kestrel.exe'" | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.ToLower() -eq '${exe}' } | Select-Object -ExpandProperty ProcessId`;
  for (const pid of execFileSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" }).split(/\s+/).filter(Boolean)) {
    kill(Number(pid));
  }
  console.log(`slot ${slot} stopped`);
}

async function start() {
  if (await windowIsUp()) {
    console.log(`slot ${slot} already running at ${devUrl} (debugging port ${cdpPort})`);
    return;
  }
  if (portOwners([vitePort, cdpPort]).length) {
    throw new Error(`ports ${vitePort}/${cdpPort} are in use by something else; run \`node e2e/dev.mjs stop\` or pick another KESTREL_SLOT`);
  }
  if (dataDir) mkdirSync(dataDir, { recursive: true });
  const config = {
    identifier,
    build: { devUrl, beforeDevCommand: `pnpm dev --port ${vitePort} --strictPort` },
  };
  const env = {
    ...process.env,
    WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${cdpPort}`,
    CARGO_TARGET_DIR: targetDir,
    ...(dataDir ? { KESTREL_DATA_DIR: dataDir } : {}),
  };
  const log = openSync(logFile, "w");
  const child = spawn(process.execPath, [join(app, "node_modules", "@tauri-apps", "cli", "tauri.js"), "dev", "--config", JSON.stringify(config)], {
    cwd: app,
    env,
    detached: true,
    stdio: ["ignore", log, log],
    windowsHide: true,
  });
  writeFileSync(pidFile, String(child.pid));
  child.unref();

  const started = Date.now();
  while (!(await windowIsUp())) {
    if (Date.now() - started > START_TIMEOUT_MS) {
      const tail = readFileSync(logFile, "utf8").split("\n").slice(-30).join("\n");
      await stop();
      throw new Error(`slot ${slot} did not start within ${START_TIMEOUT_MS / 60000} minutes. Log tail:\n${tail}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  console.log(`slot ${slot} ready at ${devUrl} (debugging port ${cdpPort}) after ${Math.round((Date.now() - started) / 1000)} s; log: ${logFile}`);
}

const command = process.argv[2];
if (command === "start") await start();
else if (command === "stop") await stop();
else if (command === "status") console.log((await windowIsUp()) ? `slot ${slot} running` : `slot ${slot} not running`);
else {
  console.log("usage: node e2e/dev.mjs start|stop|status   (KESTREL_SLOT selects the instance)");
  process.exitCode = 1;
}
