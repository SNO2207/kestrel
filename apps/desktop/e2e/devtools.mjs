// End-to-end test of the developer tools: the terminal pane, the Developer tools page, the
// query service, the MCP server (kestrel-mcp) and `kestrel-cli --live`.
//
// Everything happens in a scratch folder under %TEMP% that this script creates and removes.
// The terminal runs the default shell (PowerShell 7 if installed, else Windows PowerShell) in
// the sandbox, `echo`es, follows a folder change, opens a link to a file, runs a second
// terminal with a long-running console program (ping to 127.0.0.1), and a third from a folder's
// menu; closing them must leave none of their processes running. A project in the sandbox has
// an npm script that echoes a marker, which the projects hub's Run starts in the pane once the
// setting says so (skipped where npm isn't installed). The MCP server is started over stdio
// against this slot's Kestrel and answers search_files for a file in the sandbox (by name, inside
// it, and as a phrase, as the window's search box reads one); kestrel-cli
// is built in the slot's target folder and asked the same with --live. The system clipboard is
// never used (the Copy button and terminal copy and paste are manual checks).
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/devtools.mjs`. Screenshots go to e2e/out/devtools, dark and light.

import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel, waitForKestrelWindow } from "./connect.mjs";
import { appExe, release, targetDir } from "./slot.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "out", "devtools");
mkdirSync(out, { recursive: true });

// ---------- the sandbox ----------

// Short names, so a printed path fits on one line of the terminal (links are found per line).
const sandboxName = `kestrel-e2e-dt-${process.pid}`;
const sandbox = join(tmpdir(), sandboxName);
const alpha = join(sandbox, "alpha");
const beta = join(sandbox, "beta");
const scripted = join(sandbox, "scripted");
const needleName = `needle-${process.pid}.txt`;
const needle = join(alpha, needleName);
const marker = `kestrel-e2e-${process.pid}`;
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(alpha, { recursive: true });
mkdirSync(beta, { recursive: true });
mkdirSync(scripted, { recursive: true });
writeFileSync(needle, "found by the MCP server\n");
writeFileSync(join(beta, "beta-file.txt"), "beta\n");
writeFileSync(
  join(scripted, "package.json"),
  JSON.stringify({ name: "scripted", version: "1.0.0", scripts: { hello: `echo script-${marker}` } }, null, 2),
);

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["terminal-height", "kestrel.projects"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
// The pane starts at its default height.
await page.evaluate(() => localStorage.removeItem("terminal-height"));
await page.reload();

const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const panel = page.locator(".tab-panel.active");
const item = (name) => panel.locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`) }) });
const pane = page.locator(".terminal-pane:not([hidden])");
const termTabs = page.locator(".terminal-tab");
const tabs = () => page.evaluate(() => window.__kestrelTerminal.tabs());
const text = (key) => page.evaluate((key) => window.__kestrelTerminal.text(key), key);
const step = (await import("./steps.mjs")).stepper("devtools", () => page, { timed: true });

async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(250);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

async function openFolder(path, expectName) {
  // The file list takes the keyboard first: in a terminal, Ctrl+L belongs to the shell.
  await panel.locator(".list-viewport").first().click({ position: { x: 5, y: 5 } }).catch(() => {});
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await item(expectName).first().waitFor();
}

/** Waits until `check` holds, polling. */
async function until(what, check, timeout = 20_000) {
  const started = Date.now();
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() - started > timeout) throw new Error(`timed out waiting for ${what}`);
    await page.waitForTimeout(100);
  }
}

/** The terminal tab `key`'s text, once it contains `needle` `times` times. */
async function waitForText(key, needle, times = 1, timeout = 30_000) {
  return until(
    `${JSON.stringify(needle)} x${times} in terminal ${key}`,
    async () => {
      const all = await text(key);
      return all.split(needle).length - 1 >= times ? all : null;
    },
    timeout,
  ).catch(async (error) => {
    console.log(`--- terminal ${key} ---\n${await text(key)}\n---`);
    throw error;
  });
}

/** Waits for a terminal to have the keyboard. */
const terminalFocused = () =>
  page.waitForFunction(() => !!document.activeElement?.closest(".terminal-host.shown"), null, { timeout: 10_000 });

/** A prompt has appeared: the shell is ready for input. */
const promptIn = (folder) => (folder.endsWith("\\") ? folder : `${folder}>`);

/** Whether processes are running, asked of Windows itself rather than of Kestrel. */
function runningPids(pids) {
  if (pids.length === 0) return [];
  const script = `Get-Process -Id ${pids.join(",")} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id`;
  // Exits with 1 when some are gone; what it prints is the answer either way.
  const listed = spawnSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" });
  return (listed.stdout ?? "")
    .split(/\s+/)
    .filter(Boolean)
    .map(Number);
}

// ---------- an MCP client over stdio ----------

function startMcp(exe, pipe) {
  const child = spawn(exe, ["--pipe", pipe], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
  let buffer = "";
  const waiting = new Map();
  const stderr = [];
  child.stderr.on("data", (chunk) => stderr.push(chunk.toString()));
  child.stdout.on("data", (chunk) => {
    buffer += chunk.toString();
    let newline;
    while ((newline = buffer.indexOf("\n")) >= 0) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (!line) continue;
      const message = JSON.parse(line);
      assert.equal(message.jsonrpc, "2.0", `only JSON-RPC on stdout: ${line}`);
      if (message.id !== undefined && waiting.has(message.id)) {
        waiting.get(message.id)(message);
        waiting.delete(message.id);
      }
    }
  });
  let next = 1;
  const send = (message) => child.stdin.write(`${JSON.stringify(message)}\n`);
  return {
    request(method, params) {
      const id = next++;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error(`no answer to ${method}; stderr: ${stderr.join("")}`)), 30_000);
        waiting.set(id, (message) => {
          clearTimeout(timer);
          resolve(message);
        });
        send({ jsonrpc: "2.0", id, method, params });
      });
    },
    notify: (method, params = {}) => send({ jsonrpc: "2.0", method, params }),
    async call(name, args) {
      const response = await this.request("tools/call", { name, arguments: args });
      assert(response.result, `${name}: ${JSON.stringify(response)}`);
      return { error: response.result.isError === true, text: response.result.content?.[0]?.text ?? "" };
    },
    close: () => child.kill(),
  };
}

let defaults = null;
const allPids = new Set();
try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
  defaults = (await invoke("devtools_state")).settings;
  // The suite starts from the default settings: the service on, scripts in Windows Terminal.
  await invoke("devtools_set_settings", { settings: { ...defaults, default_shell: null, scripts_in_terminal: false, query_service: true, log_queries: true } });
  assert.deepEqual(await invoke("terminal_dev_sessions"), [], "no terminal is open when the page has just loaded");

  let first;
  await step("Ctrl+` opens the terminal pane in the folder on screen, with the default shell", async () => {
    await openFolder(alpha, needleName);
    assert.equal(await pane.count(), 0, "no pane before it is asked for");
    await page.keyboard.press("Control+Backquote");
    await pane.waitFor();
    const [tab] = await until("the shell starts", async () => {
      const list = await tabs();
      return list.length === 1 && list[0].session !== null ? list : null;
    });
    first = tab.key;
    allPids.add(tab.pid);
    const state = await invoke("terminal_shells");
    const expected = state.shells.find((shell) => shell.id === state.default);
    assert.equal(await termTabs.first().locator(".terminal-tab-title").innerText(), expected.name, "the tab is named after the shell");
    assert(["pwsh", "powershell"].includes(state.default), `the default shell is PowerShell: ${state.default}`);
    await waitForText(first, promptIn(alpha));
    await terminalFocused();
    // The pane is a row of the window's grid, under the files and beside the sidebar.
    const box = await pane.boundingBox();
    const list = await panel.boundingBox();
    assert(box.y >= list.y + list.height - 1, "the pane is below the file list");
    const side = await page.locator(".sidebar").boundingBox();
    assert(box.x >= side.x + side.width - 1, "the sidebar runs down beside it");
  });

  await step("typed commands run, and keys stay in the terminal", async () => {
    await page.keyboard.type(`echo ${marker}`);
    await page.keyboard.press("Enter");
    // Once as typed, once as printed.
    await waitForText(first, marker, 2);
    // Fast typing arrives in order (each key is not a request of its own racing the others).
    const long = Array.from({ length: 12 }, (_, n) => `w${n}Temp${n}x`).join("-");
    await page.keyboard.type(`echo ${long}`);
    await page.keyboard.press("Enter");
    const echoed = await waitForText(first, long, 2);
    assert(echoed.split("\n").some((line) => line.trim() === long), "the shell got the keys in order");
    // Backspace would go back and Ctrl+T open a tab, were the keys the window's.
    const fileTabs = await page.locator(".titlebar .tab").count();
    await page.keyboard.type("xy");
    await page.keyboard.press("Backspace");
    await page.keyboard.press("Control+t");
    await page.keyboard.press("Escape");
    await page.waitForTimeout(300);
    assert.equal(await page.locator(".titlebar .tab").count(), fileTabs, "Ctrl+T went to the shell");
    assert(await item(needleName).count(), "Backspace went to the shell: the folder is still on screen");
  });

  await step("dragging the pane's edge resizes the terminal, and the shell learns its new size", async () => {
    const before = await pane.boundingBox();
    const rowsBefore = (await page.evaluate((key) => window.__kestrelTerminal.size(key), first)).rows;
    const edge = await page.locator(".terminal-resizer").boundingBox();
    await page.mouse.move(edge.x + 200, edge.y + edge.height / 2);
    await page.mouse.down();
    await page.mouse.move(edge.x + 200, edge.y - 100, { steps: 5 });
    await page.mouse.up();
    const after = await until("the pane grows", async () => {
      const box = await pane.boundingBox();
      return box.height > before.height + 80 ? box : null;
    });
    const size = await until("the terminal fits its taller box", async () => {
      const now = await page.evaluate((key) => window.__kestrelTerminal.size(key), first);
      return now.rows > rowsBefore ? now : null;
    });
    assert(after.y < before.y, "it grew upwards");
    // The shell itself is told: PowerShell reports the pseudoconsole's height.
    await page.locator(".terminal-host.shown").click();
    await page.keyboard.type("$Host.UI.RawUI.WindowSize.Height");
    await page.keyboard.press("Enter");
    await until("the shell reports its rows", async () => (await text(first)).split("\n").some((line) => line.trim() === String(size.rows)));
    const remembered = await page.evaluate(() => Number(localStorage.getItem("terminal-height")));
    assert(Math.abs(remembered - after.height) <= 1, `the height is remembered: ${remembered} for ${after.height}`);
  });

  await step("cd to this folder takes the shell to the folder on screen", async () => {
    await openFolder(beta, "beta-file.txt");
    await page.locator(".terminal-cd").click();
    await waitForText(first, promptIn(beta));
    await terminalFocused();
    await page.keyboard.type("(Get-Location).Path");
    await page.keyboard.press("Enter");
    const all = await waitForText(first, beta, 3);
    assert(all.split("\n").some((line) => line.trim() === beta), "the shell reports the new folder");
  });

  await step("a path printed in the terminal opens in Kestrel when clicked", async () => {
    await page.keyboard.type(`echo '${needle}'`);
    await page.keyboard.press("Enter");
    await waitForText(first, needle, 2);
    // The printed line (not the command line, which quotes it): where xterm drew it.
    const point = await page.evaluate((path) => {
      const rows = [...document.querySelectorAll(".terminal-host.shown .xterm-rows > div")];
      const line = rows.filter((row) => row.textContent.trim() === path).at(-1);
      if (!line) return null;
      const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
      const range = document.createRange();
      let node = walker.nextNode();
      while (node && !node.textContent.includes("needle")) node = walker.nextNode();
      if (!node) return null;
      const at = node.textContent.indexOf("needle");
      range.setStart(node, at);
      range.setEnd(node, at + 1);
      const box = range.getBoundingClientRect();
      return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
    }, needle);
    assert(point, "the printed path is on screen");
    // Hovering makes it a link once the backend has said the file is there (the terminal's box
    // then says what a click does); the pointer is nudged until it has.
    // xterm asks again about a line only once the pointer has left it, so it comes from the
    // strip above (it may have rested on that line while it was still empty).
    const linked = () => page.evaluate(() => document.querySelector(".terminal-host.shown")?.title ?? "");
    const strip = await page.locator(".terminal-bar").boundingBox();
    for (let nudge = 0; nudge < 10 && !(await linked()); nudge++) {
      await page.mouse.move(point.x, strip.y + strip.height / 2);
      await page.mouse.move(point.x, point.y, { steps: 4 });
      await page.waitForTimeout(300);
    }
    assert.equal(await linked(), "Show this file in Kestrel", "the path became a link");
    await page.mouse.click(point.x, point.y);
    await page.waitForFunction((name) => document.querySelector(".tab-panel.active .row.selected .label")?.textContent === name, needleName, { timeout: 10_000 });
    await shotBoth("1-terminal");
  });

  await step("paths are found in output with spaces, positions after them and WSL's drives, never shares", async () => {
    const found = await page.evaluate(async () => {
      const { findPaths } = await import("/src/lib/terminalLinks.ts");
      const lines = [
        "    Directory: C:\\Users\\me\\My Folder",
        "src\\main.rs at C:\\Code\\app\\src\\main.rs:10:5: error",
        "PS C:\\Users\\me> ls",
        "see (C:\\x\\Program.cs(12,4)) and /mnt/c/Users/me/notes.md.",
        "\\\\server\\share\\file.txt and 'C:\\a b\\c.txt'",
      ];
      return lines.map((line) => findPaths(line).map((match) => [match.start, match.candidates.map((c) => `${c.path}|${c.length}`)]));
    });
    // Longest first; the backend says which exist, and the longest that does is the link.
    assert.deepEqual(found[0], [[15, ["C:\\Users\\me\\My Folder|21", "C:\\Users\\me\\My|14"]]]);
    assert(found[1][0][1].includes("C:\\Code\\app\\src\\main.rs|23"), `the line and column are left out: ${found[1]}`);
    assert.deepEqual(found[2], [[3, ["C:\\Users\\me|11"]]]);
    assert(found[3][0][1].includes("C:\\x\\Program.cs|15"), `MSBuild's (line,col) too: ${found[3]}`);
    assert.deepEqual(found[3][1], [32, ["C:\\Users\\me\\notes.md|24"]], "WSL's /mnt/c is C:");
    assert.deepEqual(found[4], [[29, ["C:\\a b\\c.txt|12", "C:\\a|4"]]], "a share is no link; a quoted path is");
  });

  let second;
  await step("Ctrl+Shift+` opens a second terminal, which runs a console program", async () => {
    await page.locator(".terminal-host.shown").click();
    await page.keyboard.press("Control+Shift+Backquote");
    const list = await until("a second shell", async () => {
      const list = await tabs();
      return list.length === 2 && list[1].session !== null ? list : null;
    });
    second = list[1].key;
    assert(list[1].active, "the new tab is the active one");
    allPids.add(list[1].pid);
    assert.equal(await termTabs.count(), 2);
    // It opened where the file list is now: alpha, where the link led.
    await waitForText(second, promptIn(alpha));
    await terminalFocused();
    await page.keyboard.type("ping -n 120 127.0.0.1");
    await page.keyboard.press("Enter");
    await waitForText(second, "127.0.0.1", 3);
    const sessions = await until("ping joins the second shell's job", async () => {
      const running = await invoke("terminal_dev_sessions");
      const mine = running.find((each) => each.pid === list[1].pid);
      return mine && mine.processes.length >= 2 ? running : null;
    });
    for (const running of sessions) for (const pid of running.processes) allPids.add(pid);
    assert.equal(sessions.length, 2);
    await shotBoth("2-two-terminals");
  });

  await step("a folder's menu opens a terminal there", async () => {
    await openFolder(sandbox, "beta");
    await item("beta").first().click({ button: "right" });
    await page.locator(".menu .menu-item", { hasText: /^Open terminal here$/ }).click();
    const list = await until("a third shell", async () => {
      const list = await tabs();
      return list.length === 3 && list[2].session !== null ? list : null;
    });
    allPids.add(list[2].pid);
    await waitForText(list[2].key, promptIn(beta));
    // Switching tabs shows the other terminal, still running.
    await termTabs.nth(1).click();
    assert.equal((await tabs())[1].active, true);
    await waitForText(second, "127.0.0.1", 4);
  });

  await step("hiding the pane keeps the shells; Ctrl+` brings it back", async () => {
    await page.locator(".terminal-hide").click();
    await pane.waitFor({ state: "detached" }).catch(() => {});
    assert.equal(await page.locator(".terminal-pane:not([hidden])").count(), 0);
    assert.equal((await invoke("terminal_dev_sessions")).length, 3, "hidden, not ended");
    await page.keyboard.press("Control+Backquote");
    await pane.waitFor();
    assert.equal(await termTabs.count(), 3);
  });

  await step("closing the terminals ends every process they started", async () => {
    const before = await invoke("terminal_dev_sessions");
    for (const running of before) for (const pid of running.processes) allPids.add(pid);
    const pids = [...allPids];
    assert(pids.length >= 4, `three shells and ping: ${pids}`);
    assert.deepEqual(runningPids(pids).sort(), pids.slice().sort(), "all running before");
    while ((await termTabs.count()) > 0) {
      await termTabs.first().hover();
      await termTabs.first().locator(".terminal-tab-close").click();
    }
    assert.equal(await page.locator(".terminal-pane").count(), 0, "the pane goes with its last tab");
    assert.deepEqual(await invoke("terminal_dev_sessions"), []);
    await until("every process has ended", async () => (await invoke("terminal_dev_alive", { pids })).every((alive) => !alive));
    assert.deepEqual(runningPids(pids), [], "Windows lists none of them");
  });

  await step("closing a window ends the terminals it had", async () => {
    await openFolder(alpha, needleName);
    const windowsBefore = await invoke("app_windows_dev");
    assert.deepEqual(windowsBefore, ["main"], "only the first window is open; close any others first");
    await page.keyboard.press("Control+n");
    const label = await until("a second window", async () => (await invoke("app_windows_dev")).find((each) => !windowsBefore.includes(each)));
    const other = await waitForKestrelWindow(browser, label);
    await other.locator(".tab-panel.active .row .label", { hasText: needleName }).waitFor();
    await other.locator(".tab-panel.active .list-viewport").first().click({ position: { x: 5, y: 5 } });
    await other.keyboard.press("Control+Backquote");
    const opened = await until("a terminal in the second window", async () => {
      const running = (await invoke("terminal_dev_sessions")).filter((each) => each.window === label);
      return running.length === 1 ? running[0] : null;
    });
    await other.waitForFunction(() => !!document.activeElement?.closest(".terminal-host.shown"), null, { timeout: 10_000 });
    await other.keyboard.type("ping -n 120 127.0.0.1");
    await other.keyboard.press("Enter");
    const running = await until("ping runs in the second window's terminal", async () => {
      const mine = (await invoke("terminal_dev_sessions")).find((each) => each.id === opened.id);
      return mine && mine.processes.length >= 2 ? mine : null;
    });
    for (const pid of running.processes) allPids.add(pid);
    assert.deepEqual((await invoke("terminal_dev_sessions")).map((each) => each.window), [label], "the main window has none");
    // Its last tab closing closes the window (Ctrl+W from its file list: in a terminal it is the shell's).
    await other.locator(".tab-panel.active .list-viewport").first().click({ position: { x: 5, y: 5 } });
    await other.keyboard.press("Control+w");
    await until("the second window closes", async () => (await invoke("app_windows_dev")).length === 1);
    await until("its terminal's processes end", async () => (await invoke("terminal_dev_alive", { pids: running.processes })).every((alive) => !alive));
    assert.deepEqual(runningPids(running.processes), []);
    assert.deepEqual(await invoke("terminal_dev_sessions"), []);
  });

  await step("the Developer tools page shows the settings and the claude mcp add command", async () => {
    await openFolder(sandbox, "alpha");
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill("Developer tools");
    await page.keyboard.press("Enter");
    const devtools = page.locator(".devtools-page");
    await devtools.waitFor();
    await page.locator(".devtools-status[data-running='yes']").waitFor();
    const state = await invoke("devtools_state");
    const command = await page.locator(".devtools-command-text").innerText();
    assert.equal(command, state.claude_command);
    assert.match(command, /^claude mcp add --scope user kestrel -- ".*kestrel-mcp\.exe" --pipe "\\\\\.\\pipe\\kestrel-query-S-1-/, command);
    assert(state.mcp_path, "kestrel-mcp.exe is beside kestrel.exe");
    assert.equal(await page.locator(".devtools-tools li").count(), 7);
    const select = devtools.locator("select");
    const options = await select.locator("option").allInnerTexts();
    assert(options.some((o) => /PowerShell/.test(o)) && options.includes("Command Prompt"), `shells: ${options}`);
    assert.equal(await page.locator(".crumb").last().innerText(), "Developer tools");
  });

  let mcpExe;
  let pipe;
  await step("the MCP server answers search_files for a file in the sandbox", async () => {
    const state = await invoke("devtools_state");
    pipe = state.pipe;
    mcpExe = join(dirname(appExe), "kestrel-mcp.exe");
    assert(existsSync(mcpExe), `${mcpExe} was built with the app`);
    const mcp = startMcp(mcpExe, pipe);
    try {
      const init = await mcp.request("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "devtools-e2e", version: "1" } });
      assert.equal(init.result.serverInfo.name, "kestrel");
      mcp.notify("notifications/initialized");
      const tools = (await mcp.request("tools/list", {})).result.tools.map((tool) => tool.name).sort();
      assert.deepEqual(tools, ["disk_usage", "folder_size", "list_folder", "list_projects", "project_details", "recent_files", "search_files"]);
      const found = await until(
        "the index to hold the needle",
        async () => {
          const answer = await mcp.call("search_files", { query: needleName, limit: 5 });
          assert(!answer.error, answer.text);
          const result = JSON.parse(answer.text);
          return result.total > 0 ? result : null;
        },
        30_000,
      );
      assert.equal(found.results[0].path.toLowerCase(), needle.toLowerCase());
      assert.equal(found.results[0].kind, "file");
      // It searches as the window's search box does: inside files, with the line that matched...
      const inside = JSON.parse((await mcp.call("search_files", { query: "content:\"by the MCP\"", folder: sandbox })).text);
      assert.deepEqual(inside.results.map((entry) => entry.name), [needleName], JSON.stringify(inside));
      assert.deepEqual(inside.results[0].matched, { line: 1, text: "found by the MCP server", more_lines: 0 });
      // ...and plain phrases, saying what they were read as.
      const phrase = JSON.parse((await mcp.call("search_files", { query: "text files containing MCP", folder: sandbox })).text);
      assert.equal(phrase.ran_as, "ext:txt;md;log content:MCP");
      assert.deepEqual(phrase.results.map((entry) => entry.name), [needleName]);
      const listed = JSON.parse((await mcp.call("list_folder", { path: sandbox })).text);
      assert.deepEqual(listed.items.map((entry) => entry.name), ["alpha", "beta", "scripted"]);
      const size = JSON.parse((await mcp.call("folder_size", { path: sandbox })).text);
      assert(size.files >= 3 && size.folders === 3, JSON.stringify(size));
      const missing = await mcp.call("folder_size", { path: join(sandbox, "nowhere") });
      assert(missing.error && /not in Kestrel's index/.test(missing.text), missing.text);
    } finally {
      mcp.close();
    }
  });

  await step("the page lists what the MCP server asked", async () => {
    // The page reads the list again every two seconds while it is on screen.
    await page.locator(".devtools-log tbody tr", { hasText: needleName }).filter({ hasText: "kestrel-mcp.exe" }).first().waitFor({ timeout: 10_000 });
    const rows = await page.locator(".devtools-log tbody tr").allInnerTexts();
    assert(rows.some((row) => row.includes("Searched") && row.includes(needleName)), rows.join("\n"));
    await shotBoth("3-developer-tools");
  });

  await step("kestrel-cli --live asks the running Kestrel", async () => {
    const built = spawnSync("cargo", ["build", "-q", "-p", "kestrel-cli", ...(release ? ["--release"] : [])], {
      env: { ...process.env, CARGO_TARGET_DIR: targetDir },
      encoding: "utf8",
      timeout: 15 * 60 * 1000,
    });
    assert.equal(built.status, 0, built.stderr);
    const cli = join(dirname(appExe), "kestrel-cli.exe");
    const searched = execFileSync(cli, ["--live", "--pipe", pipe, "search", needleName], { encoding: "utf8" });
    assert.match(searched, new RegExp(needleName.replace(/\./g, "\\.")));
    assert.match(searched, /from the running Kestrel/);
    const sized = execFileSync(cli, ["--live", "--pipe", pipe, "--json", "size", sandbox], { encoding: "utf8" });
    assert.equal(JSON.parse(sized).folders, 3);
    const status = execFileSync(cli, ["--pipe", pipe, "status"], { encoding: "utf8" });
    assert.match(status, /^Kestrel .* index ready/);
  });

  await step("with the setting on, the projects hub's Run runs a script in the terminal pane", async () => {
    const npm = spawnSync("where", ["npm"], { encoding: "utf8" });
    if (npm.status !== 0) {
      console.log("    (npm isn't installed here; skipped)");
      return;
    }
    await page.locator(".devtools-check", { hasText: "Run project scripts in the terminal pane" }).locator("input").check();
    await until("the setting is saved", async () => (await invoke("devtools_state")).settings.scripts_in_terminal);
    await invoke("projects_probe", { on: true, allow: [sandbox] });
    await page.keyboard.press("Control+Shift+P");
    await page.locator(".projects-hub").waitFor();
    await page.locator(".projects-filter").fill(sandboxName);
    const card = page.locator(".project-card").filter({ has: page.locator(".project-name", { hasText: /^scripted$/ }) });
    await card.waitFor({ timeout: 60_000 });
    await card.click();
    const detail = page.locator(".project-detail");
    await detail.locator(".script", { has: page.locator(".script-name", { hasText: /^hello$/ }) }).first().locator(".script-run").click();
    await pane.waitFor();
    const [tab] = await until("the script's terminal starts", async () => {
      const list = await tabs();
      return list.length === 1 && list[0].session !== null ? list : null;
    });
    allPids.add(tab.pid);
    assert.equal(tab.title, "npm run hello");
    await waitForText(tab.key, `script-${marker}`, 2, 60_000);
    assert.deepEqual(await invoke("projects_probe_take"), [], "Windows Terminal was not started");
    await page.keyboard.press("Control+Shift+P");
    await page.locator(".terminal-tab-close").first().click({ force: true });
    await until("the script's shell ends", async () => (await invoke("terminal_dev_sessions")).length === 0);
  });
} catch (error) {
  // What the window showed when a step failed.
  await page.screenshot({ path: join(out, "failure.png") }).catch(() => {});
  throw error;
} finally {
  await invoke("projects_probe", { on: false, allow: [] }).catch(() => {});
  if (defaults) await invoke("devtools_set_settings", { settings: defaults }).catch(() => {});
  // Any terminal left open (a failed step) ends with the page's next load.
  await page.evaluate((saved) => {
    for (const [key, value] of saved) {
      if (value === null) localStorage.removeItem(key);
      else localStorage.setItem(key, value);
    }
  }, savedStorage);
  await page.reload().catch(() => {});
  await page.waitForTimeout(500);
  const left = [...allPids].filter((pid) => runningPids([pid]).length > 0);
  if (left.length > 0) console.log(`!!  processes still running: ${left.join(", ")}`);
  for (let attempt = 0; attempt < 20; attempt++) {
    try {
      rmSync(sandbox, { recursive: true, force: true });
      break;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
  await browser.close();
}
console.log("devtools: all steps passed");
