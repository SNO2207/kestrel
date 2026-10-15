// End-to-end test of the features working together, against the real app window.
//
// Each feature branch tested itself in the details view. This suite checks the places where
// they meet: drag and drop, undo, the Windows menu, the preview pane, Quick Look, batch rename
// and locked folders, all in the icon views, and the list view's sideways scrolling during a
// drag. Drops go through the dev build's test hook and drags out are rehearsed, as in
// dragdrop.mjs; the Windows menu runs the helper in `verbs` mode, as in shellmenu.mjs.
//
// File changes happen only inside a scratch folder this script creates under the temp
// directory and removes afterwards. One folder in it is made unreadable with icacls and given
// its permissions back, as in locked.mjs. The window's view and preview settings (local
// storage) are saved first and put back at the end. Screenshots go to e2e/out/integration.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/integration.mjs`.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "integration");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-integration-${process.pid}`);
const inbox = join(sandbox, "inbox");
const archive = join(inbox, "Archive");
const privateFolder = join(inbox, "private");
const outside = join(sandbox, "outside");
const many = join(sandbox, "many");
rmSync(sandbox, { recursive: true, force: true });
for (const folder of [archive, privateFolder, outside, many]) mkdirSync(folder, { recursive: true });
for (const name of ["a.txt", "b.txt", "c.txt"]) writeFileSync(join(inbox, name), `${name}\n`);
writeFileSync(join(privateFolder, "secret.txt"), "secret");
writeFileSync(join(outside, "dropped.txt"), "dropped");
for (let i = 0; i < 400; i++) writeFileSync(join(many, `item-${String(i).padStart(3, "0")}.txt`), "x");
const onDisk = (...parts) => existsSync(join(...parts));
const icacls = (...args) => execFileSync("icacls", [privateFolder, ...args], { stdio: "ignore" });
const LOCKED = "Your account can't open this folder";

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open", "preview-width"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
/** An item of the icon, list or tile views, by the name it shows. */
const item = (name) => panel.locator(".view-item").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const selectedNames = () => panel.locator(".view-item.selected .label").allTextContents();
const viewIs = (mode) => panel.locator(`.filelist[data-view="${mode}"]`).waitFor();
const menu = page.locator(".menu");
const menuLabels = () => menu.locator(".menu-label").allTextContents();
const notice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 15_000 });
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);

const hook = {
  keys: (keys) => page.evaluate((keys) => window.__kestrelDragDrop.keys(keys ?? undefined), keys ?? null),
  enter: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [paths, at]),
  over: (at) => page.evaluate((at) => window.__kestrelDragDrop.over(at.x, at.y), at),
  leave: () => page.evaluate(() => window.__kestrelDragDrop.leave()),
  drop: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [paths, at]),
};

/** The centre of an element, in the page's CSS pixels. */
async function centre(locator) {
  const box = await locator.boundingBox();
  assert(box, "element is on screen");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Captured through the debugging protocol, which unlike Playwright's own capture does not resize the window. */
async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Opens a folder through the address bar and waits for one of its items. */
async function open(path, expectName) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await panel.locator(".label", { hasText: exactly(expectName) }).first().waitFor();
}

/** Waits for the app to start the Windows menu helper and returns what it recorded. */
async function helperRun() {
  for (let attempt = 0; attempt < 150; attempt++) {
    const record = await invoke("shell_probe_take");
    if (record) return record;
    await page.waitForTimeout(100);
  }
  throw new Error("the helper was not started");
}

try {
  await step("the scratch folder opens in medium icons", async () => {
    await panel.locator(".row").first().waitFor();
    await open(inbox, "a.txt");
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit3");
    await viewIs("medium");
    for (const name of ["Archive", "private", "a.txt", "b.txt", "c.txt"]) await item(name).waitFor();
  });

  await step("a folder icon takes a drop, with its ring and the move hint", async () => {
    const dropped = join(outside, "dropped.txt");
    await hook.keys({ ctrl: false, shift: true });
    await hook.enter([dropped], await centre(item("Archive")));
    await panel.locator(".view-item.drop-target .label", { hasText: exactly("Archive") }).waitFor();
    await page.locator(".drop-hint", { hasText: "Move to Archive" }).waitFor();
    await shotBoth("1-icon-drop-target");
    const result = await hook.drop([dropped], await centre(item("Archive")));
    assert.deepEqual(result, { effect: "move", names: ["dropped.txt"] });
    // The shell has finished the move when the drop returns.
    assert(onDisk(archive, "dropped.txt") && !onDisk(dropped));
    assert.equal(await panel.locator(".drop-target").count(), 0);
    await hook.keys(undefined);
  });

  await step("Ctrl+Z undoes the drop, which went into the undo history", async () => {
    await list.focus();
    await page.keyboard.press("Control+z");
    await notice(/Undid move of dropped\.txt/);
    assert(onDisk(outside, "dropped.txt") && !onDisk(archive, "dropped.txt"));
    await page.keyboard.press("Control+y");
    await notice(/Redid move of dropped\.txt/);
    assert(onDisk(archive, "dropped.txt") && !onDisk(outside, "dropped.txt"));
  });

  await step("a press and a short move on selected icons starts a drag of all of them", async () => {
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut("prepare"));
    await item("a.txt").click();
    await item("c.txt").click({ modifiers: ["Control"] });
    assert.deepEqual(await selectedNames(), ["a.txt", "c.txt"]);
    const from = await centre(item("c.txt"));
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 14, from.y + 6, { steps: 3 });
    await page.mouse.up();
    await page.waitForFunction(() => window.__kestrelDragDrop.lastDragOut() !== null);
    assert.deepEqual(await page.evaluate(() => window.__kestrelDragDrop.lastDragOut()), { effect: "none", items: 2 });
    assert.deepEqual(await selectedNames(), ["a.txt", "c.txt"], "the drag keeps the selection");
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut(undefined));
  });

  await step("F2 on several selected icons opens batch rename", async () => {
    await page.keyboard.press("F2");
    const dialog = page.locator(".rename-dialog");
    await dialog.waitFor();
    await page.waitForFunction(() => document.querySelectorAll(".rename-preview-row").length === 2);
    await shotBoth("2-icon-batch-rename");
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert(onDisk(inbox, "a.txt") && onDisk(inbox, "c.txt"), "nothing was renamed");
  });

  await step("an icon's menu offers Open with and the Windows menu; Shift+right-click goes straight there", async () => {
    await item("b.txt").click({ button: "right" });
    await menu.waitFor();
    const labels = await menuLabels();
    assert(labels.includes("Open with…") && labels.at(-1) === "Show more options", labels.join(" | "));
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });

    await invoke("shell_probe", { mode: "verbs" });
    const at = await centre(item("b.txt"));
    await page.mouse.move(at.x, at.y);
    await page.keyboard.down("Shift");
    await page.mouse.click(at.x, at.y, { button: "right" });
    await page.keyboard.up("Shift");
    const record = await helperRun();
    assert.equal(record.error, null);
    assert.deepEqual(record.paths, [join(inbox, "b.txt")]);
    assert(record.args.includes("--extended"), record.args.join(" "));
    assert.equal(await menu.count(), 0, "Kestrel's own menu stays closed");
    await invoke("shell_probe", { mode: "off" });
  });

  await step("the preview pane follows the focus across icons, and Space opens Quick Look", async () => {
    await item("a.txt").click();
    await page.keyboard.press("Alt+p");
    const pane = page.locator(".preview-pane");
    await pane.waitFor();
    await pane.locator(".preview-name", { hasText: exactly("a.txt") }).waitFor();
    await page.keyboard.press("ArrowRight");
    await pane.locator(".preview-name", { hasText: exactly("b.txt") }).waitFor();
    await shotBoth("3-icon-preview-pane");
    await page.keyboard.press("Space");
    const quickLook = page.locator(".quicklook");
    await quickLook.locator(".preview-name", { hasText: exactly("b.txt") }).waitFor();
    await page.keyboard.press("ArrowRight");
    await quickLook.locator(".preview-name", { hasText: exactly("c.txt") }).waitFor();
    await shotBoth("4-icon-quick-look");
    await page.keyboard.press("Escape");
    await quickLook.waitFor({ state: "detached" });
    await page.keyboard.press("Alt+p");
    await pane.waitFor({ state: "detached" });
  });

  await step("a folder this account can't open shows its padlock in an icon view", async () => {
    icacls("/inheritance:r");
    await list.focus();
    // Answers are remembered per folder; refreshing asks again.
    await page.keyboard.press("F5");
    await item("private").locator(".lock-glyph").waitFor();
    assert.match(await item("private").getAttribute("class"), /\bdimmed\b/);
    assert.match(await item("private").getAttribute("title"), new RegExp(escape(LOCKED)));
    await shotBoth("5-icon-locked");
    await item("private").dblclick();
    await notice(LOCKED);
    // The padlock follows the name in every view, inside the item's own box.
    for (const [digit, mode] of [[5, "list"], [8, "tiles"]]) {
      await list.focus();
      await page.keyboard.press(`Control+Shift+Digit${digit}`);
      await viewIs(mode);
      const glyph = await item("private").locator(".lock-glyph").boundingBox();
      const label = await item("private").locator(".label").boundingBox();
      const box = await item("private").boundingBox();
      assert(glyph.x >= label.x + label.width - 1 && Math.abs(glyph.y + glyph.height / 2 - (label.y + label.height / 2)) <= 3, "beside the name");
      assert(glyph.x + glyph.width <= box.x + box.width && glyph.y + glyph.height <= box.y + box.height, "inside the item");
      await shotBoth(`6-${mode}-locked`);
    }
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit3");
    await viewIs("medium");
    icacls("/reset");
  });

  await step("in the list view, a drag near the right or left edge scrolls the columns", async () => {
    await open(many, "item-000.txt");
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit5");
    await viewIs("list");
    const box = await list.boundingBox();
    const scrollLeft = () => list.evaluate((element) => element.scrollLeft);
    assert.equal(await scrollLeft(), 0);
    await hook.enter([join(inbox, "a.txt")], { x: box.x + box.width - 8, y: box.y + box.height / 2 });
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .list-viewport").scrollLeft > 200);
    await hook.over({ x: box.x + box.width / 2, y: box.y + box.height / 2 });
    const stopped = await scrollLeft();
    await page.waitForTimeout(250);
    assert.equal(await scrollLeft(), stopped, "scrolling stops away from the edges");
    await hook.over({ x: box.x + 6, y: box.y + box.height / 2 });
    await page.waitForFunction((from) => document.querySelector(".tab-panel.active .list-viewport").scrollLeft < from - 100, stopped);
    await hook.leave();
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  try {
    icacls("/reset");
  } catch {
    // Already readable, or already gone.
  }
  await page
    .evaluate(() => {
      window.__kestrelDragDrop?.leave();
      window.__kestrelDragDrop?.keys(undefined);
      window.__kestrelDragDrop?.rehearseDragOut(undefined);
    })
    .catch(() => {});
  await invoke("shell_probe", { mode: "off" }).catch(() => {});
  await page.keyboard.up("Shift").catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
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
  rmSync(sandbox, { recursive: true, force: true });
}
