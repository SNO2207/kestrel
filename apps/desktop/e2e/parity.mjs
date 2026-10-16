// End-to-end test of Explorer parity: the command bar, New > every type Windows offers, New >
// Shortcut, shortcuts made by Alt+drop, Select none and Invert selection, the details pane,
// Shift+Delete's confirmation, Folder Options (protected files, single-click, the full path,
// sync status), Open in Terminal, Explorer's keyboard shortcuts, Open file location, Home,
// and a second window (Ctrl+N).
//
// File changes happen only in a scratch folder under the temp directory, removed afterwards.
// Outside it the suite only reads, and puts back what it changes: it visits C:\Windows\Help
// (forgotten again if the visit was its own), pins and unpins a folder of its own, records
// and forgets one file of its own as opened, turns the window to full screen and back, and
// restores the window's view, preview and Folder Options settings. Windows Terminal is not
// started (the projects feature's development hook records the launch), the clipboard is
// left alone, and the two native dialogs it opens (Shift+Delete's confirmation and a
// folder's Properties) are found by their windows and closed, choosing No. Needs a dev build.
//
//   KESTREL_SLOT=<n> node e2e/dev.mjs start
//   KESTREL_SLOT=<n> node e2e/parity.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel, waitForKestrelWindow } from "./connect.mjs";
import { png } from "./fixtures.mjs";
import { appExe } from "./slot.mjs";
import { webviews } from "./windows.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-parity-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "sub"), { recursive: true });
mkdirSync(join(sandbox, "cloud"));
writeFileSync(join(sandbox, "notes.txt"), "notes");
writeFileSync(join(sandbox, "alpha.txt"), "alpha alpha");
writeFileSync(join(sandbox, "beta.txt"), "beta");
writeFileSync(join(sandbox, "picture.png"), png(64, 48, (x, y) => [x * 4, y * 5, 160]));
writeFileSync(join(sandbox, "hidden.txt"), "hidden");
writeFileSync(join(sandbox, "protected.sys"), "protected");
writeFileSync(join(sandbox, "cloud", "plain.txt"), "here");
writeFileSync(join(sandbox, "cloud", "pinned.txt"), "kept");
writeFileSync(join(sandbox, "cloud", "online.txt"), "elsewhere");
const attrib = (flags, ...path) => execFileSync("attrib", [...flags, join(sandbox, ...path)]);
attrib(["+h"], "hidden.txt");
attrib(["+h", "+s"], "protected.sys");
attrib(["+p"], "cloud", "pinned.txt");
attrib(["+u"], "cloud", "online.txt");
const HELP = "C:\\Windows\\Help";

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.folder-options", "kestrel.view-options", "kestrel.views", "preview-open", "details-open", "search-scope"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
// Start from the defaults: details everywhere, hidden items hidden, no panes, Folder Options' defaults.
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const menu = page.locator(".menu");
const exactly = (text) => new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`);
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const menuItem = (label) => menu.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) });
const command = (label) => page.locator(`.commandbar .command[aria-label="${label}"]`);
const selectedCount = () => page.evaluate(() => document.querySelectorAll(".tab-panel.active .row.selected").length);
const onDisk = (...parts) => existsSync(join(sandbox, ...parts));
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}, target = page) =>
  target.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const parity = (method, ...args) => page.evaluate(([method, args]) => window.__kestrelParity[method](...args), [method, args]);
const setOptions = (change) => page.evaluate((change) => window.__kestrelParity.setOptions(change), change);
const drops = {
  keys: (keys) => page.evaluate((keys) => window.__kestrelDragDrop.keys(keys ?? undefined), keys ?? null),
  enter: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [paths, at]),
  drop: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [paths, at]),
  hint: () => page.evaluate(() => window.__kestrelDragDrop.hint()),
};

/** Captured through the debugging protocol, which unlike Playwright's own capture does not resize the window. */
async function shotBoth(name, target = page, targetSession = session) {
  for (const scheme of ["dark", "light"]) {
    await target.emulateMedia({ colorScheme: scheme });
    await target.waitForTimeout(200);
    const { data } = await targetSession.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `parity-${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await target.emulateMedia({ colorScheme: null });
}

async function centre(locator) {
  const box = await locator.boundingBox();
  assert(box, "element is on screen");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Opens a folder through the address bar and waits for it. */
async function open(path, target = page) {
  await target.keyboard.press("Control+l");
  await target.locator(".address-input").fill(path);
  await target.keyboard.press("Enter");
  await target.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, basename(path));
}

/** Waits for `check` to hold, retrying for up to `ms`. */
async function eventually(check, ms = 10_000) {
  const until = Date.now() + ms;
  for (;;) {
    try {
      return await check();
    } catch (error) {
      if (Date.now() > until) throw error;
      await page.waitForTimeout(150);
    }
  }
}

// ---------- native dialogs ----------

// Lists the visible dialogs (class #32770) of the slot's kestrel.exe, and closes those whose
// title contains KESTREL_E2E_CLOSE, as their close button would (No, or Cancel).
const DIALOGS = String.raw`
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class KestrelE2eDialogs {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr lParam);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lParam);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder text, int max);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int max);
  [DllImport("user32.dll")] static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr w, IntPtr l);
  public static string Run(uint pid, string close) {
    var found = new List<string>();
    EnumWindows((top, unused) => {
      uint owner;
      GetWindowThreadProcessId(top, out owner);
      if (owner != pid || !IsWindowVisible(top)) return true;
      var kind = new StringBuilder(64);
      GetClassName(top, kind, 64);
      if (kind.ToString() != "#32770") return true;
      var title = new StringBuilder(512);
      GetWindowText(top, title, 512);
      var text = title.ToString();
      found.Add("\"" + text.Replace("\\", "\\\\").Replace("\"", "\\\"") + "\"");
      if (close.Length > 0 && text.Contains(close)) PostMessage(top, 0x0010, IntPtr.Zero, IntPtr.Zero);
      return true;
    }, IntPtr.Zero);
    return "[" + String.Join(",", found) + "]";
  }
}
"@
$exe = $env:KESTREL_E2E_EXE
$app = Get-CimInstance Win32_Process -Filter "Name = 'kestrel.exe'" | Where-Object { $_.ExecutablePath -eq $exe } | Select-Object -First 1
if ($app) { [KestrelE2eDialogs]::Run([uint32]$app.ProcessId, [string]$env:KESTREL_E2E_CLOSE) } else { "[]" }
`;

/** The titles of the app's open dialogs; closes, choosing No, those whose title contains `close`. */
function dialogs(close = "") {
  const encoded = Buffer.from(DIALOGS, "utf16le").toString("base64");
  const output = execFileSync("powershell", ["-NoProfile", "-EncodedCommand", encoded], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
    env: { ...process.env, KESTREL_E2E_EXE: appExe, KESTREL_E2E_CLOSE: close },
  });
  return JSON.parse(output.trim() || "[]");
}

/** Waits for a dialog whose title contains `part`, then closes it. Returns its title. */
async function closeDialog(part, ms = 15_000) {
  const until = Date.now() + ms;
  for (;;) {
    const title = dialogs().find((text) => text.includes(part));
    if (title) {
      dialogs(part);
      return title;
    }
    if (Date.now() > until) throw new Error(`no dialog with "${part}" in its title appeared; open: ${dialogs().join(", ")}`);
    await page.waitForTimeout(250);
  }
}

// ---------- what the suite adds outside its folder, to take away again ----------

await page.waitForFunction(() => typeof window.__kestrelParity === "object" && typeof window.__kestrelShortcuts === "object");
const visitedBefore = (await invoke("frecency_top", { limit: 500 })).some((folder) => folder.path.toLowerCase() === HELP.toLowerCase());
const made = { pin: null, opened: null, second: null, windows: [] };

try {
  await step("the scratch folder opens under the command bar", async () => {
    await list.waitFor();
    await open(sandbox);
    await row("notes.txt").waitFor();
    await page.locator(".commandbar").waitFor();
    assert(await command("Cut").isDisabled(), "nothing is selected to cut");
    await row("notes.txt").click();
    assert(!(await command("Cut").isDisabled()), "a selection can be cut");
    await shotBoth("1-command-bar");
  });

  await step("New lists every type Windows offers and makes a text document to rename", async () => {
    const kinds = await invoke("new_menu");
    await list.click({ position: { x: 300, y: 400 } });
    await command("New").click();
    await menu.waitFor();
    const labels = await menu.locator(".menu-label").allTextContents();
    assert.equal(labels[0], "Folder");
    assert(labels.includes("Shortcut"), labels.join(", "));
    for (const kind of kinds) assert(labels.includes(kind.label), `${kind.label} is offered`);
    const text = kinds.find((kind) => kind.id === ".txt");
    assert(text, "a text document is always offered");
    await shotBoth("2-new-menu");
    await menuItem(text.label).click();
    const input = panel.locator("input.rename");
    await input.waitFor();
    assert.equal(await input.inputValue(), `${text.base}.txt`);
    await page.keyboard.type("fresh");
    await page.keyboard.press("Enter");
    await row("fresh.txt").waitFor();
    assert(onDisk("fresh.txt") && statSync(join(sandbox, "fresh.txt")).size === 0);
  });

  await step("New makes the other types from their templates and registered bytes", async () => {
    const kinds = (await invoke("new_menu")).filter((kind) => kind.kind !== "shortcut" && kind.id !== ".txt");
    for (const kind of kinds) {
      const name = await invoke("new_file", { parent: sandbox, kind: kind.id });
      assert(name.startsWith(kind.base) && name.toLowerCase().endsWith(kind.id), `${kind.label}: ${name}`);
      const size = statSync(join(sandbox, name)).size;
      // Templates may be empty (Office's own are, for Word and PowerPoint); registered bytes are not.
      if (kind.kind === "data") assert(size > 0, `${name} has the registry's bytes`);
    }
    // A second of the same type gets a number, as in Explorer.
    const again = await invoke("new_file", { parent: sandbox, kind: ".txt" });
    const first = await invoke("new_file", { parent: sandbox, kind: ".txt" });
    assert.match(first, /\(2\)\.txt$/, `${again}, then ${first}`);
    console.log(`    made ${kinds.map((kind) => kind.label).join(", ") || "no other types"} as well`);
  });

  await step("New > Shortcut asks what it points at and makes the shortcut", async () => {
    await list.click({ position: { x: 300, y: 500 } });
    await command("New").click();
    await menuItem("Shortcut").click();
    const dialog = page.locator(".shortcut-dialog");
    await dialog.waitFor();
    await dialog.locator("input").first().fill(join(sandbox, "notes.txt"));
    await eventually(async () => assert.equal(await dialog.locator("input").nth(1).inputValue(), "notes"));
    await shotBoth("3-new-shortcut");
    await dialog.locator("button", { hasText: "Create" }).click();
    await dialog.waitFor({ state: "detached" });
    await row("notes.lnk").waitFor();
    const target = await invoke("shortcut_target_dev", { link: join(sandbox, "notes.lnk") });
    assert.equal(target.toLowerCase(), join(sandbox, "notes.txt").toLowerCase());
  });

  await step("the new files are listed once each, however fast they came", async () => {
    // Files made in quick succession reach the index twice, from the change journal and from
    // the folder's own listing; each must still be listed once (`kestrel-index` `Sync::finish`).
    await page.waitForTimeout(1500);
    await list.focus();
    await page.keyboard.press("F5");
    await page.waitForTimeout(500);
    const labels = await panel.locator(".row .label").allTextContents();
    assert.equal(new Set(labels).size, labels.length, labels.join(", "));
  });

  await step("Alt+drop makes shortcuts, as in Explorer", async () => {
    const at = await centre(row("sub"));
    await drops.keys({ ctrl: false, shift: false, alt: true });
    await drops.enter([join(sandbox, "alpha.txt")], at);
    await eventually(async () => assert.deepEqual(await drops.hint(), { verb: "Create shortcuts", name: "sub", to: "in" }));
    await page.locator(".drop-hint", { hasText: "Create shortcuts in sub" }).waitFor();
    await shotBoth("4-alt-drop");
    const result = await drops.drop([join(sandbox, "alpha.txt")], at);
    assert.deepEqual(result, { effect: "link", names: ["alpha.txt - Shortcut.lnk"] });
    assert(onDisk("alpha.txt"), "the item itself stays where it was");
    const target = await invoke("shortcut_target_dev", { link: join(sandbox, "sub", "alpha.txt - Shortcut.lnk") });
    assert.equal(target.toLowerCase(), join(sandbox, "alpha.txt").toLowerCase());
    await drops.keys(undefined);
  });

  await step("Select none and Invert selection, from the command bar's menu", async () => {
    await list.click({ position: { x: 300, y: 600 } });
    await page.keyboard.press("Control+a");
    const total = await panel.locator(".row").count();
    await eventually(async () => assert.equal(await selectedCount(), total));
    await command("More").click();
    await menuItem("Select none").click();
    await eventually(async () => assert.equal(await selectedCount(), 0));
    await row("alpha.txt").click();
    await command("More").click();
    await shotBoth("5-more-menu");
    await menuItem("Invert selection").click();
    await eventually(async () => assert.equal(await selectedCount(), total - 1));
    assert(!(await row("alpha.txt").evaluate((element) => element.classList.contains("selected"))));
    // The status bar adds up what is selected.
    await page.locator(".statusbar .selection-size").waitFor();
  });

  await step("the details pane shows the selection's properties and takes the preview's place", async () => {
    await row("picture.png").click();
    await page.keyboard.press("Alt+Shift+P");
    const pane = page.locator(".details-pane");
    await pane.waitFor();
    await pane.locator(".details-name", { hasText: "picture.png" }).waitFor();
    await pane.locator('.details-line[data-label="Size"]').waitFor();
    await eventually(async () => {
      const text = await pane.locator(".details-list").innerText();
      assert(/64/.test(text) && /48/.test(text), `the picture's dimensions are shown: ${text}`);
    });
    await shotBoth("6-details-one");
    await row("beta.txt").click({ modifiers: ["Control"] });
    await pane.locator(".details-name", { hasText: "2 items selected" }).waitFor();
    await pane.locator('.details-line[data-label="Total size"]').waitFor();
    await shotBoth("7-details-many");
    await page.keyboard.press("Escape");
    await pane.locator(".details-kind", { hasText: "This folder" }).waitFor();
    // The preview pane takes the same place: opening one closes the other.
    await page.keyboard.press("Alt+P");
    await page.locator(".preview-pane:not(.details-pane)").waitFor();
    await pane.waitFor({ state: "detached" });
    await page.keyboard.press("Alt+Shift+P");
    await pane.waitFor();
    assert.equal(await page.locator(".preview-pane:not(.details-pane)").count(), 0, "the preview pane closed");
    await page.keyboard.press("Alt+Shift+P");
    await pane.waitFor({ state: "detached" });
  });

  await step("Shift+Delete asks first, as Explorer does, and No keeps the file", async () => {
    await row("beta.txt").click();
    await list.focus();
    await page.keyboard.press("Shift+Delete");
    const title = await closeDialog("Delete");
    console.log(`    Windows asked: "${title}"`);
    await page.waitForTimeout(500);
    assert(onDisk("beta.txt"), "nothing is deleted without a yes");
  });

  await step("Alt+Enter with nothing selected shows the folder's properties", async () => {
    await page.keyboard.press("Escape");
    await list.focus();
    await page.keyboard.press("Alt+Enter");
    const title = await closeDialog(basename(sandbox));
    assert(title.includes("Properties") || title.includes(basename(sandbox)), title);
  });

  await step("Folder Options: protected operating system files stay hidden until asked for", async () => {
    await parity("openOptions");
    await page.locator(".options-dialog").waitFor();
    await shotBoth("8-folder-options");
    await page.locator(".options-dialog .options-check", { hasText: "Show hidden files" }).locator("input").check();
    await row("hidden.txt").waitFor();
    assert.equal(await row("protected.sys").count(), 0, "a hidden system file stays out");
    await page.locator(".options-dialog .options-check", { hasText: "Hide protected operating system files" }).locator("input").uncheck();
    await row("protected.sys").waitFor();
    await page.locator(".options-dialog .options-check", { hasText: "Hide protected operating system files" }).locator("input").check();
    await page.locator(".options-dialog .options-check", { hasText: "Show hidden files" }).locator("input").uncheck();
    await row("hidden.txt").waitFor({ state: "detached" });
    await page.locator(".options-dialog button", { hasText: "Done" }).click();
    await page.locator(".options-dialog").waitFor({ state: "detached" });
  });

  await step("Folder Options: single-click opens, and tabs can show the full path", async () => {
    await setOptions({ singleClick: true });
    await row("sub").click();
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
    await page.keyboard.press("Alt+ArrowUp");
    await row("notes.txt").waitFor();
    await setOptions({ singleClick: false });
    await setOptions({ fullPath: true });
    await eventually(async () => assert.equal(await page.locator(".tab.active .tab-title").innerText(), sandbox));
    await setOptions({ fullPath: false });
    await eventually(async () => assert.equal(await page.locator(".tab.active .tab-title").innerText(), basename(sandbox)));
  });

  await step("sync status shows in every view for files in a synced folder", async () => {
    await page.evaluate((root) => window.__kestrelSync.setTestRoots([root]), join(sandbox, "cloud"));
    await open(join(sandbox, "cloud"));
    const glyph = (name) => row(name).locator(".sync-glyph");
    await eventually(async () => {
      assert.equal(await glyph("plain.txt").getAttribute("data-sync"), "local");
      assert.equal(await glyph("pinned.txt").getAttribute("data-sync"), "pinned");
      assert.equal(await glyph("online.txt").getAttribute("data-sync"), "cloud");
    });
    await shotBoth("9-sync-details");
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit8");
    await panel.locator('.view-item[data-path$="online.txt"] .sync-glyph[data-sync="cloud"]').waitFor();
    await shotBoth("10-sync-tiles");
    await page.keyboard.press("Control+Shift+Digit6");
    await setOptions({ syncStatus: false });
    await eventually(async () => assert.equal(await panel.locator(".sync-glyph").count(), 0));
    await setOptions({ syncStatus: true });
    await page.evaluate(() => window.__kestrelSync.setTestRoots([]));
    await page.keyboard.press("Alt+ArrowUp");
    await row("notes.txt").waitFor();
  });

  await step("Open in Terminal starts Windows Terminal in the folder", async () => {
    await invoke("projects_probe", { on: true, allow: [] });
    await command("More").click();
    await menuItem("Open in Terminal").click();
    const launches = await eventually(async () => {
      const recorded = await invoke("projects_probe_take");
      assert.equal(recorded.length, 1);
      return recorded;
    });
    assert.equal(launches[0].dir.toLowerCase(), sandbox.toLowerCase());
    assert(launches[0].params.includes(sandbox) || launches[0].file.toLowerCase().includes("cmd"), JSON.stringify(launches[0]));
    await invoke("projects_probe", { on: false, allow: [] });
  });

  await step("Explorer's shortcuts: the address bar, the search box, F4, F6 and Ctrl+Shift+E", async () => {
    for (const keys of ["Alt+d", "Control+l", "F4"]) {
      await list.focus();
      await page.keyboard.press(keys);
      await page.locator(".address.editing").waitFor();
      await page.keyboard.press("Escape");
      await page.locator(".address.editing").waitFor({ state: "detached" });
    }
    for (const keys of ["Control+e", "Control+f"]) {
      await list.focus();
      await page.keyboard.press(keys);
      await eventually(async () => assert(await page.evaluate(() => document.activeElement?.closest(".searchbox") !== null)));
      await page.keyboard.press("Escape");
    }
    // Ctrl+Shift+E is the folder tree's: it asks for the folder to be shown there, not for a search.
    await page.evaluate(() => {
      window.__revealed = null;
      window.addEventListener("kestrel:reveal-in-tree", (event) => (window.__revealed = event.detail.path), { once: true });
    });
    await list.focus();
    await page.keyboard.press("Control+Shift+E");
    assert.equal((await page.evaluate(() => window.__revealed)).toLowerCase(), sandbox.toLowerCase());
    assert(await page.evaluate(() => document.activeElement?.closest(".searchbox") === null), "the search box keeps out of it");
    // F6 moves through the address bar, the search box, the sidebar and the files.
    await list.focus();
    const region = () =>
      page.evaluate(() => [".address", ".searchbox", ".sidebar", ".content"].find((selector) => document.activeElement?.closest(selector)) ?? "none");
    const order = [];
    for (let n = 0; n < 4; n++) {
      await page.keyboard.press("F6");
      await page.waitForTimeout(80);
      order.push(await region());
    }
    assert.deepEqual(order, [".address", ".searchbox", ".sidebar", ".content"]);
    await page.keyboard.press("Shift+F6");
    assert.equal(await region(), ".sidebar");
    await list.focus();
  });

  await step("Explorer's shortcuts: Ctrl+Shift+N, Ctrl+R, Backspace, Alt+Up and the mouse's back button", async () => {
    await list.focus();
    await page.keyboard.press("Control+Shift+N");
    const input = panel.locator("input.rename");
    await input.waitFor();
    await page.keyboard.press("Enter");
    await row("New folder").waitFor();
    assert(onDisk("New folder"));
    // Ctrl+R refreshes the folder, as F5 does, rather than reloading the page.
    await page.evaluate(() => (window.__notReloaded = true));
    writeFileSync(join(sandbox, "late.txt"), "late");
    await list.focus();
    await page.keyboard.press("Control+r");
    await row("late.txt").waitFor();
    assert(await page.evaluate(() => window.__notReloaded === true), "the page was not reloaded");
    await row("sub").dblclick();
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
    await list.focus();
    await page.keyboard.press("Backspace");
    await row("notes.txt").waitFor();
    await row("sub").dblclick();
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
    await page.keyboard.press("Alt+ArrowUp");
    await row("notes.txt").waitFor();
    // The mouse's back button (button 3) goes back to sub.
    await page.evaluate(() => {
      window.dispatchEvent(new MouseEvent("mousedown", { button: 3, bubbles: true }));
      window.dispatchEvent(new MouseEvent("mouseup", { button: 3, bubbles: true }));
    });
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
    await page.evaluate(() => window.dispatchEvent(new MouseEvent("mouseup", { button: 4, bubbles: true })));
    await row("notes.txt").waitFor();
  });

  await step("Explorer's shortcuts: Ctrl+wheel, middle-click and F11", async () => {
    const box = await list.boundingBox();
    await page.mouse.move(box.x + 200, box.y + 200);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, -120);
    await page.keyboard.up("Control");
    // Up makes items larger: from details, the list view, as in Explorer.
    await eventually(async () => assert.equal(await panel.locator(".filelist").getAttribute("data-view"), "list"));
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit6");
    await eventually(async () => assert.equal(await panel.locator(".filelist").getAttribute("data-view"), "details"));
    // Middle-click opens a folder in a new tab.
    const tabsBefore = await page.locator(".tab").count();
    await row("sub").click({ button: "middle" });
    await eventually(async () => assert.equal(await page.locator(".tab").count(), tabsBefore + 1));
    await page.locator(".tab.active .tab-title", { hasText: "sub" }).waitFor();
    await page.keyboard.press("Control+w");
    await eventually(async () => assert.equal(await page.locator(".tab").count(), tabsBefore));
    // F11: full screen and back.
    const fullScreen = () => page.evaluate(() => window.__TAURI_INTERNALS__.invoke("plugin:window|is_fullscreen", { label: "main" }));
    await list.focus();
    await page.keyboard.press("F11");
    await eventually(async () => assert.equal(await fullScreen(), true));
    await page.keyboard.press("F11");
    await eventually(async () => assert.equal(await fullScreen(), false));
  });

  await step("a search result's menu has Open file location", async () => {
    await page.keyboard.press("Control+f");
    await page.keyboard.type(`alpha in:${sandbox}`);
    await panel.locator(".row .label", { hasText: exactly("alpha.txt") }).waitFor();
    await row("alpha.txt").click({ button: "right" });
    await menuItem("Open file location").click();
    await page.locator(".crumb", { hasText: exactly(basename(sandbox)) }).waitFor();
    await eventually(async () => assert(await row("alpha.txt").evaluate((element) => element.classList.contains("selected"))));
  });

  await step("Home lists Quick access, Favorites and Recent files", async () => {
    await page.evaluate(() => window.__kestrelHome.windowsRecent(false));
    assert.equal(await invoke("frecency_visit", { path: HELP }), true);
    await invoke("pins_add", { paths: [join(sandbox, "sub")], group: null });
    made.pin = (await invoke("pins_board")).pins.find((pin) => pin.path.toLowerCase() === join(sandbox, "sub").toLowerCase());
    await page.evaluate(() => window.__kestrelShortcuts.refresh());
    made.opened = join(sandbox, "notes.txt");
    await invoke("record_opened", { path: made.opened });
    await page.locator(".home-entry").click();
    const home = page.locator(".tab-panel.active .home");
    await home.waitFor();
    assert.equal(await page.locator(".tab.active .tab-title").innerText(), "Home");
    const item = (section, name) =>
      home.locator(`.home-item[data-section="${section}"]`).filter({ has: page.locator(".label", { hasText: exactly(name) }) });
    await item("quick", "Help").waitFor();
    await item("favorites", "sub").waitFor();
    await item("recent", "notes.txt").waitFor();
    await item("recent", "notes.txt").click();
    await shotBoth("11-home");
    // The details pane follows Home's selection too, and the keyboard moves through it.
    await page.keyboard.press("Alt+Shift+P");
    await page.locator(".details-pane .details-name", { hasText: "notes.txt" }).waitFor();
    await shotBoth("11b-home-details");
    await page.keyboard.press("Alt+Shift+P");
    await page.locator(".details-pane").waitFor({ state: "detached" });
    await home.focus();
    await page.keyboard.press("Home");
    await eventually(async () => assert.equal(await home.locator(".home-item.selected").getAttribute("data-section"), "quick"));
    await page.keyboard.press("End");
    const last = await home.locator(".home-item").last().getAttribute("data-key");
    await eventually(async () => assert.equal(await home.locator(".home-item.selected").getAttribute("data-key"), last));
    // Remove from Recent takes it off Home's list, and nothing else.
    await item("recent", "notes.txt").click({ button: "right" });
    await menuItem("Remove from Recent").click();
    await item("recent", "notes.txt").waitFor({ state: "detached" });
    assert(onDisk("notes.txt"));
    // Opening a favorite goes there; the address "Home" comes back.
    await item("favorites", "sub").dblclick();
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill("Home");
    await page.keyboard.press("Enter");
    await home.waitFor();
    await page.keyboard.press("Alt+ArrowLeft");
    await page.locator(".crumb", { hasText: exactly("sub") }).waitFor();
  });

  await step("Kestrel can open to Home", async () => {
    await setOptions({ start: "home" });
    await page.reload();
    await page.locator(".tab-panel.active .home").waitFor();
    assert.equal(await page.locator(".tab").count(), 1);
    await setOptions({ start: "profile" });
    await page.reload();
    await panel.locator(".row").first().waitFor();
  });

  await step("Ctrl+N opens a second window in the same folder; both work, and it closes", async () => {
    await open(sandbox);
    await row("notes.txt").waitFor();
    await list.focus();
    const windowsBefore = await invoke("app_windows_dev");
    assert.deepEqual(windowsBefore, ["main"], "only the first window is open; close any others first");
    await page.keyboard.press("Control+n");
    // The second window is main-2, or a later number if windows were opened before.
    const label = await eventually(async () => {
      const others = (await invoke("app_windows_dev")).filter((each) => !windowsBefore.includes(each));
      assert.equal(others.length, 1);
      return others[0];
    });
    made.windows = [label];
    assert.match(label, /^main-\d+$/);
    const second = await waitForKestrelWindow(browser, label);
    made.second = { page: second, label };
    const secondPanel = second.locator(".tab-panel.active");
    await secondPanel.locator(".row .label", { hasText: exactly("notes.txt") }).waitFor();
    assert.equal(await second.evaluate(() => window.__kestrelParity.label()), label);
    // The harness still finds the first window first.
    const again = await connectToKestrel();
    assert.equal(await again.page.evaluate(() => window.__kestrelParity.label()), "main");
    assert.deepEqual(await invoke("app_windows_dev"), ["main", label]);
    // Each window watches the folder it shows: a file made outside appears in both.
    writeFileSync(join(sandbox, "outside.txt"), "outside");
    await row("outside.txt").waitFor();
    await secondPanel.locator(".row .label", { hasText: exactly("outside.txt") }).waitFor();
    // The second window's preview has a webview of its own.
    await secondPanel.locator(".row", { hasText: "picture.png" }).click();
    await second.keyboard.press("Alt+P");
    await second.locator(".preview-pane").waitFor();
    await eventually(async () => assert(webviews().filter((view) => view.visible).length >= 3, JSON.stringify(webviews())));
    const secondSession = await second.context().newCDPSession(second);
    await shotBoth("12-second-window", second, secondSession);
    await secondSession.detach().catch(() => {});
    await second.keyboard.press("Alt+P");
    await second.locator(".preview-pane").waitFor({ state: "detached" });
    // Its last tab closing closes it; the first window carries on.
    await second.keyboard.press("Control+w");
    await eventually(async () => assert.deepEqual(await invoke("app_windows_dev"), ["main"]));
    made.second = null;
    await open(join(sandbox, "sub"));
    await row("alpha.txt - Shortcut.lnk").waitFor();
    await again.browser.close().catch(() => {});
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  await drops.keys(undefined).catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  // Any window the suite opened and did not close is closed from the first.
  for (const label of made.windows) {
    if (label !== "main") await invoke("plugin:window|close", { label }).catch(() => {});
  }
  await invoke("projects_probe", { on: false, allow: [] }).catch(() => {});
  await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("plugin:window|set_fullscreen", { label: "main", value: false })).catch(() => {});
  if (made.pin) await invoke("pins_remove", { id: made.pin.id }).catch(() => {});
  if (made.opened) await invoke("forget_recent", { path: made.opened }).catch(() => {});
  if (!visitedBefore) await invoke("frecency_forget", { path: HELP }).catch(() => {});
  await page.evaluate(() => window.__kestrelSync?.setTestRoots([])).catch(() => {});
  await page.evaluate(() => window.__kestrelHome?.windowsRecent(true)).catch(() => {});
  // Put the window's settings back as they were, then leave it somewhere that still exists.
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  await page.reload().catch(() => {});
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await session.detach().catch(() => {});
  await browser.close();
  for (const name of ["hidden.txt", "protected.sys", join("cloud", "pinned.txt"), join("cloud", "online.txt")]) {
    try {
      execFileSync("attrib", ["-h", "-s", "-p", "-u", join(sandbox, name)], { stdio: "ignore" });
    } catch {
      // Already gone.
    }
  }
  rmSync(sandbox, { recursive: true, force: true });
  if (existsSync(sandbox)) console.log(`could not remove ${sandbox}: ${readdirSync(sandbox).join(", ")}`);
}
