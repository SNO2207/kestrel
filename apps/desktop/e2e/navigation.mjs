// End-to-end test of where the merged features meet in navigation: Home, This PC and the
// Recycle Bin (special locations) and the inside of an archive, each in either pane, in history,
// in new tabs and a new window, in the saved layout, in the palette, with the folder tree and
// the disk usage hub following along.
//
// File changes happen only inside a scratch folder this script creates under the temp directory
// and removes afterwards: a zip made there with PowerShell's Compress-Archive, and one file that
// "Copy to other pane" extracts from it into another scratch folder. The Recycle Bin page is only
// looked at. The window's view settings (local storage) and its saved layout (the app's
// database) are saved first and put back at the end. It needs a dev build (the layout's test
// hook). Screenshots go to e2e/out/navigation, in dark and light.
//
//   KESTREL_SLOT=<n> node e2e/dev.mjs start
//   KESTREL_SLOT=<n> node e2e/navigation.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel, waitForKestrelWindow } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "navigation");
mkdirSync(out, { recursive: true });

// ---------- the scratch folder ----------

const sandbox = join(tmpdir(), `kestrel-e2e-navigation-${process.pid}`);
const packed = join(sandbox, "packed");
const dest = join(sandbox, "dest");
const zip = join(sandbox, "bundle.zip");
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(packed, "inner"), { recursive: true });
mkdirSync(dest, { recursive: true });
writeFileSync(join(packed, "top.txt"), "at the top of the archive");
writeFileSync(join(packed, "inner", "readme.txt"), "inside a folder inside the archive");
writeFileSync(join(sandbox, "plain.txt"), "plain");
// Windows' own zip writer, so the archive is one any program makes.
execFileSync("powershell", ["-NoProfile", "-Command", `Compress-Archive -Path '${packed}\\*' -DestinationPath '${zip}'`], { stdio: "ignore" });
assert(existsSync(zip), "the zip was made");

// ---------- the window ----------

let { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open", "preview-width"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
const savedPanes = new Map(await invoke("layout_load")).get("panes") ?? "";
/** Folders the Frequent list held before: any this suite adds (C:\, say) are forgotten after. */
const visitedBefore = new Set((await invoke("frecency_top", { limit: 1000 })).map((folder) => folder.path.toLowerCase()));
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();
await page.locator(".tab-panel.active .row").first().waitFor();

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const pane = (n) => page.locator(`.tab-panel.current[data-pane="${n}"]`);
const active = () => page.locator(".tab-panel.active");
const row = (name, scope = active()) => scope.locator(".row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const crumbs = () => page.locator(".crumbs .crumb").allTextContents();
const paneTitles = (n) => page.locator(`.pane-bar[data-pane="${n}"] .tab-title`).allTextContents();
const menu = page.locator(".menu").first();
const menuItem = (label) => menu.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) }).first();
const notice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 15_000 });
const comparable = (path) => path.toLowerCase().replace(/\\+$/, "");

const step = (await import("./steps.mjs")).stepper("navigation", () => page);

async function eventually(check, timeout = 15_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      return await check();
    } catch (error) {
      if (Date.now() > deadline) throw error;
      await page.waitForTimeout(100);
    }
  }
}

async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Types an address into the address bar of the pane with the keyboard. */
async function go(address) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(address);
  await page.keyboard.press("Enter");
}

/** Goes to a folder (or archive address) and waits until the breadcrumb ends with its name. */
async function open(path) {
  await go(path);
  await eventually(async () => assert.equal((await crumbs()).at(-1), basename(path)));
}

/** Gives a pane the keyboard, by a click on its tab strip's empty end. */
async function focusPane(n) {
  const bar = page.locator(`.pane-bar[data-pane="${n}"] .pane-tab.shown`);
  await bar.click();
  await eventually(async () => assert.equal(await page.locator(`.pane-bar[data-pane="${n}"]`).getAttribute("class").then((c) => c.includes("active")), true));
}

async function choosePalette(text, title, how = "Enter") {
  await page.keyboard.press("Control+k");
  const input = page.locator(".palette input");
  await input.waitFor();
  await input.fill(text);
  await page.locator('.palette-list[aria-busy="false"]').waitFor();
  const item = page.locator(".palette-item").filter({ has: page.locator(".palette-title", { hasText: exactly(title) }) }).first();
  await item.waitFor();
  await item.click();
  if (how !== "Enter") await page.keyboard.press(how);
}

const made = { windows: [] };

try {
  await step("Home, This PC and the Recycle Bin open in either pane, beside a folder or each other", async () => {
    await open(sandbox);
    await page.keyboard.press("Control+Shift+D");
    await pane(1).waitFor();
    // The second pane opens beside the first, which keeps the keyboard; given the keyboard, it
    // goes to This PC while the first keeps its folder.
    await focusPane(1);
    await go("This PC");
    await pane(1).locator(".pc-page").waitFor();
    assert.deepEqual(await paneTitles(1), ["This PC"]);
    await row("plain.txt", pane(0)).waitFor();
    assert.match(await page.locator(".statusbar").innerText(), /\d+ items/);
    // The first pane goes Home: two pages side by side.
    await focusPane(0);
    await go("Home");
    await pane(0).locator(".home").waitFor();
    await pane(1).locator(".pc-page").waitFor();
    assert.deepEqual(await paneTitles(0), ["Home"]);
    assert.deepEqual(await crumbs(), ["Home"]);
    await shotBoth("1-pages-in-both-panes");
    // Explorer's own names lead to the same pages.
    await focusPane(1);
    await go("shell:RecycleBinFolder");
    await pane(1).locator(".bin-page").waitFor();
    assert.deepEqual(await paneTitles(1), ["Recycle Bin"]);
  });

  await step("Back and Forward step through locations, folders and archives in each pane", async () => {
    await page.keyboard.press("Alt+ArrowLeft");
    await pane(1).locator(".pc-page").waitFor();
    await page.keyboard.press("Alt+ArrowRight");
    await pane(1).locator(".bin-page").waitFor();
    await focusPane(0);
    await page.keyboard.press("Alt+ArrowLeft");
    await row("plain.txt", pane(0)).waitFor();
    await page.keyboard.press("Alt+ArrowRight");
    await pane(0).locator(".home").waitFor();
    // Up from a drive is This PC, in either pane.
    await go("C:\\");
    await row("Windows", pane(0)).waitFor();
    await page.keyboard.press("Alt+ArrowUp");
    await pane(0).locator(".pc-page").waitFor();
  });

  await step("an archive opens as a folder in one pane, and Copy to other pane extracts into the other", async () => {
    await open(sandbox);
    await row("bundle.zip", pane(0)).dblclick();
    await eventually(async () => assert.equal((await crumbs()).at(-1), "bundle.zip"));
    await row("top.txt", pane(0)).waitFor();
    await row("inner", pane(0)).waitFor();
    assert.deepEqual(await paneTitles(0), ["bundle.zip"]);
    // Nothing inside an archive can be cut, renamed or deleted.
    await row("top.txt", pane(0)).click();
    for (const label of ["Cut", "Rename", "Delete"]) assert(await page.locator(`button.command[aria-label="${label}"]`).isDisabled(), `${label} is greyed out`);
    // The other pane shows the destination folder.
    await focusPane(1);
    await open(dest);
    await focusPane(0);
    await row("top.txt", pane(0)).click();
    await page.keyboard.press("Alt+c");
    await eventually(() => assert(existsSync(join(dest, "top.txt")), "the item was extracted into the other pane's folder"));
    await row("top.txt", pane(1)).waitFor();
    // Moving out of an archive is refused: it is read-only.
    await row("top.txt", pane(0)).click();
    await page.keyboard.press("Alt+x");
    await notice("read-only");
    assert(existsSync(join(dest, "top.txt")) && !existsSync(join(dest, "inner")));
    await shotBoth("2-archive-beside-a-folder");
  });

  await step("the palette lists the locations and the inside of an archive, and opens both", async () => {
    await choosePalette("This PC", "This PC");
    await pane(0).locator(".pc-page").waitFor();
    // A typed path goes on past the archive's name.
    await page.keyboard.press("Control+k");
    const input = page.locator(".palette input");
    await input.fill(`/${zip}\\`);
    await page.locator('.palette-list[aria-busy="false"]').waitFor();
    await page.locator(".palette-item .palette-title", { hasText: exactly("top.txt") }).waitFor();
    await page.locator(".palette-item .palette-title", { hasText: exactly("inner") }).waitFor();
    await page.keyboard.press("Escape");
    // Opened from the palette, an archive opens as a folder (as from the index, by its name).
    await choosePalette(`/${sandbox}\\bundle`, "bundle.zip");
    await row("top.txt", pane(0)).waitFor();
    await choosePalette("Recycle Bin", "Recycle Bin");
    await pane(0).locator(".bin-page").waitFor();
  });

  await step("a new tab opens where the tab is, and a new window too", async () => {
    // From the Recycle Bin, Ctrl+T opens another tab on it, in the same pane.
    await page.keyboard.press("Control+t");
    await eventually(async () => assert.deepEqual(await paneTitles(0), ["Recycle Bin", "Recycle Bin"]));
    await page.keyboard.press("Control+w");
    await eventually(async () => assert.deepEqual(await paneTitles(0), ["Recycle Bin"]));
    // Ctrl+N from inside the archive opens a window on it.
    await page.keyboard.press("Alt+ArrowLeft");
    await row("top.txt", pane(0)).waitFor();
    const before = await invoke("app_windows_dev");
    await page.keyboard.press("Control+n");
    const label = await eventually(async () => {
      const others = (await invoke("app_windows_dev")).filter((each) => !before.includes(each));
      assert.equal(others.length, 1);
      return others[0];
    });
    made.windows.push(label);
    const second = await waitForKestrelWindow(browser, label);
    await second.locator(".tab-panel.active .row .label", { hasText: exactly("top.txt") }).waitFor();
    assert.equal((await second.locator(".tab.active .tab-title").textContent())?.trim(), "bundle.zip");
    // Its tabs open on locations too.
    await second.keyboard.press("Control+l");
    await second.locator(".address-input").fill("This PC");
    await second.keyboard.press("Enter");
    await second.locator(".tab-panel.active .pc-page").waitFor();
    await second.keyboard.press("Control+w");
    await eventually(async () => assert.deepEqual(await invoke("app_windows_dev"), before));
    made.windows = [];
  });

  await step("the saved layout keeps locations and archive addresses", async () => {
    await focusPane(1);
    await go("Home");
    await pane(1).locator(".home").waitFor();
    const snapshot = await page.evaluate(() => window.__kestrelLayout.snapshot());
    assert.equal(snapshot.dual, true);
    const paths = snapshot.panes.flatMap((each) => each.tabs.map((tab) => comparable(tab.path)));
    assert(paths.includes(comparable(join(zip, ""))) || paths.includes(comparable(zip)), JSON.stringify(paths));
    assert(paths.includes("home"), JSON.stringify(paths));
  });

  await step("the folder tree follows the pane with the keyboard", async () => {
    await go(dest);
    await eventually(async () =>
      assert.equal(comparable((await page.locator(".tree-row.current").getAttribute("data-tree-path")) ?? ""), comparable(dest)),
    );
    await focusPane(0);
    await open(sandbox);
    await eventually(async () =>
      assert.equal(comparable((await page.locator(".tree-row.current").getAttribute("data-tree-path")) ?? ""), comparable(sandbox)),
    );
  });

  await step("This PC's drive opens the disk usage hub from either pane", async () => {
    await focusPane(1);
    await go("This PC");
    const tile = pane(1).locator(".pc-tile.drive").first();
    await tile.click({ button: "right" });
    await menuItem("Show disk usage").click();
    const hub = page.locator(".disk-hub");
    await hub.waitFor();
    await shotBoth("3-hub-from-this-pc");
    await hub.locator("h1").click();
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });
    await pane(1).locator(".pc-page").waitFor();
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  for (const label of made.windows) await invoke("plugin:window|close", { label }).catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  // One pane again, then the settings and the saved layout as they were.
  if (await page.locator(".pane-bar").count().catch(() => 0)) await page.keyboard.press("Control+Shift+D").catch(() => {});
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  for (const folder of await invoke("frecency_top", { limit: 1000 }).catch(() => [])) {
    if (!visitedBefore.has(folder.path.toLowerCase())) await invoke("frecency_forget", { path: folder.path }).catch(() => {});
  }
  await page.evaluate(() => window.__kestrelLayout?.flush()).catch(() => {});
  await invoke("layout_save", { name: "panes", value: savedPanes }).catch(() => {});
  await page.reload().catch(() => {});
  await browser.close().catch(() => {});
  rmSync(sandbox, { recursive: true, force: true });
}
