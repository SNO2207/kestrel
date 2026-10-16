// End-to-end test of the window's layout features against the real app window: the second
// pane (its tabs, the keyboard, the splitter, copying and moving to the other pane with undo,
// drops between the panes, and the layout coming back when the app starts again), grouping in
// every view and in search results, the details view's columns, the filter box, the selection
// rectangle and the Content view.
//
// File changes happen only inside a scratch folder this script creates under the temp
// directory and removes afterwards. Undoing a copy sends the copy to the Recycle Bin, so that
// step runs only with KESTREL_E2E_RECYCLE=1; moves are undone by moving back. The clipboard is
// never touched. The suite restarts the slot's app once (`e2e/dev.mjs stop`, then `start`), as
// undo-restart.mjs does, so run it with the same KESTREL_SLOT as the running instance. The
// window's view settings (local storage) and its saved layout and columns (the app's database)
// are saved first and put back at the end. Drops go through the dev build's test hook, as in
// dragdrop.mjs. Screenshots go to e2e/out/layout, in dark and light.
//
//   KESTREL_SLOT=<n> node e2e/dev.mjs start
//   KESTREL_SLOT=<n> node e2e/layout.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { png } from "./fixtures.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "out", "layout");
mkdirSync(out, { recursive: true });
const recycle = process.env.KESTREL_E2E_RECYCLE === "1";

// ---------- the scratch folder ----------

const sandbox = join(tmpdir(), `kestrel-e2e-layout-${process.pid}`);
const left = join(sandbox, "left");
const right = join(sandbox, "right");
const many = join(sandbox, "many");
const dated = join(sandbox, "dated");
const sized = join(sandbox, "sized");
const gone = join(sandbox, "gone");
rmSync(sandbox, { recursive: true, force: true });
for (const folder of [join(left, "Sub"), right, many, dated, join(sized, "Folder"), gone]) mkdirSync(folder, { recursive: true });
writeFileSync(join(gone, "doomed.txt"), "doomed");
writeFileSync(join(left, "a.txt"), "0123456789");
writeFileSync(join(left, "b.txt"), "b".repeat(20_000));
writeFileSync(join(left, "c.log"), "");
writeFileSync(join(left, "photo.png"), png(64, 32, (x, y) => [x * 4, y * 8, 120]));
writeFileSync(join(left, "Sub", "inner.txt"), "inner");
const NOTES = Array.from({ length: 12 }, (_, i) => `notes-${String(i + 1).padStart(2, "0")}.txt`);
NOTES.forEach((name, i) => writeFileSync(join(left, name), "n".repeat(10 * (i + 1))));
/** Everything `left` shows: the folder, four files and the notes. */
const LEFT_ITEMS = 1 + 4 + NOTES.length;
for (let i = 0; i < 240; i++) writeFileSync(join(many, `item-${String(i).padStart(3, "0")}.txt`), "x");
// Dates in local time: now, yesterday at noon, and two years ago.
const now = new Date();
const stamp = (name, when) => {
  writeFileSync(join(dated, name), name);
  utimesSync(join(dated, name), when, when);
};
stamp("now.txt", now);
stamp("yesterday.txt", new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 12));
stamp("ancient.txt", new Date(now.getFullYear() - 2, 0, 15, 12));
writeFileSync(join(sized, "empty.txt"), "");
writeFileSync(join(sized, "tiny.txt"), "t".repeat(100));
writeFileSync(join(sized, "small.bin"), Buffer.alloc(20_000));
writeFileSync(join(sized, "medium.bin"), Buffer.alloc(2 * 1024 * 1024));
const onDisk = (...parts) => existsSync(join(...parts));

// ---------- the window ----------

let browser;
let page;
let session;
({ browser, page } = await connectToKestrel());
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open", "preview-width"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const LAYOUT_DOCS = ["panes", "columns:folder", "columns:search"];
const savedLayout = new Map(await invoke("layout_load"));
// Start from the defaults: one pane, every folder in details, default columns.
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
for (const name of LAYOUT_DOCS) await invoke("layout_save", { name, value: "" });
await page.reload();

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const active = () => page.locator(".tab-panel.active");
const pane = (n) => page.locator(`.tab-panel.current[data-pane="${n}"]`);
const list = (scope = active()) => scope.locator(".list-viewport");
/** An item, in whatever view is showing, by the name it displays. */
const item = (name, scope = active()) => scope.locator(".view-item, .row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const labels = (scope = active()) => scope.locator(".label").allTextContents();
const status = () => page.locator(".statusbar").innerText();
const viewIs = (mode) => active().locator(`.filelist[data-view="${mode}"]`).waitFor();
const groupLabels = (scope = active()) => scope.locator(".group-label").allTextContents();
const notice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 15_000 });
/** The open menu; a function, since the page changes when the app restarts. */
const menu = () => page.locator(".menu").first();
const menuItem = (label, within = page) => within.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) }).first();
const step = (await import("./steps.mjs")).stepper("layout", () => page);

const hook = {
  keys: (keys) => page.evaluate((keys) => window.__kestrelDragDrop.keys(keys ?? undefined), keys ?? null),
  enter: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [paths, at]),
  leave: () => page.evaluate(() => window.__kestrelDragDrop.leave()),
  drop: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [paths, at]),
};

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

/** Opens a folder in the active tab through the address bar and waits for one of its items. */
async function open(path, expectName) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  // The folder's name ends the breadcrumb once the tab is there: both folders hold an a.txt.
  await page.waitForFunction((name) => {
    const crumbs = document.querySelectorAll(".crumbs .crumb");
    return crumbs.length > 0 && crumbs[crumbs.length - 1].textContent === name;
  }, basename(path));
  // By full path: the list keeps the previous folder's rows drawn until the new ones are in,
  // and the previous folder may hold a file of the same name.
  if (expectName) {
    await page.waitForFunction(
      (wanted) =>
        [...document.querySelectorAll(".tab-panel.active [data-path]")].some((element) => element.getAttribute("data-path").toLowerCase() === wanted),
      join(path, expectName).toLowerCase(),
    );
  } else await active().locator(".list-empty").waitFor();
}

/** Chooses an entry of the View menu, or of its Group by submenu. */
async function viewMenu(label, sub) {
  await page.locator(".view-button").click();
  await menu().waitFor();
  if (sub) {
    await menuItem(sub).hover();
    await menuItem(label, page.locator(".menu .menu")).click();
  } else {
    await menuItem(label).click();
  }
  await menu().waitFor({ state: "detached" });
}

/** The layout as it would be saved now. */
const snapshot = () => page.evaluate(() => window.__kestrelLayout.snapshot());

/** Stops the slot's app, runs `meanwhile`, starts the app again and connects to the new window. */
async function restart(meanwhile = () => {}) {
  await session.detach().catch(() => {});
  await browser.close();
  const dev = join(here, "dev.mjs");
  execFileSync(process.execPath, [dev, "stop"], { stdio: ["ignore", "inherit", "inherit"], env: process.env });
  meanwhile();
  execFileSync(process.execPath, [dev, "start"], { stdio: ["ignore", "inherit", "inherit"], env: process.env });
  ({ browser, page } = await connectToKestrel());
  session = await page.context().newCDPSession(page);
}

/** Every header sits above (or, in the list view, over) the items of its group, never on them. */
async function headersClearOfItems() {
  const overlaps = await active().evaluate((panel) => {
    const boxes = (selector) => [...panel.querySelectorAll(selector)].map((element) => element.getBoundingClientRect());
    const headers = boxes(".group-header");
    const items = boxes(".row, .view-item");
    const meet = (a, b) => a.left < b.right - 1 && b.left < a.right - 1 && a.top < b.bottom - 1 && b.top < a.bottom - 1;
    return headers.filter((header) => items.some((each) => meet(header, each))).length;
  });
  assert.equal(overlaps, 0, "no group header overlaps an item");
}

session = await page.context().newCDPSession(page);

try {
  await step("the second pane opens beside the first, each with tabs and history of its own", async () => {
    await active().locator(".row").first().waitFor();
    await open(left, "a.txt");
    await list().focus();
    await page.keyboard.press("Control+Shift+D");
    await page.locator(".pane-bar").nth(1).waitFor();
    assert.equal(await page.locator(".titlebar .tab").count(), 0, "with two panes, the tabs are above the panes");
    await item("a.txt", pane(1)).waitFor();
    await page.locator('.pane-bar[data-pane="0"].active').waitFor();
    // Tab moves the keyboard to the other pane, which shows that it has it.
    await page.keyboard.press("Tab");
    await page.locator('.pane-bar[data-pane="1"].active').waitFor();
    assert.equal(await page.evaluate(() => document.activeElement?.closest(".tab-panel")?.dataset.pane), "1");
    await open(right, null);
    await item("a.txt", pane(0)).waitFor();
    // Tabs open in the pane with the keyboard.
    await page.keyboard.press("Control+t");
    await page.locator('.pane-bar[data-pane="1"] .pane-tab').nth(1).waitFor();
    assert.equal(await page.locator('.pane-bar[data-pane="0"] .pane-tab').count(), 1);
    await page.keyboard.press("Control+w");
    assert.equal(await page.locator('.pane-bar[data-pane="1"] .pane-tab').count(), 1);
    // Each tab keeps its own history.
    await list().focus();
    await page.keyboard.press("Alt+ArrowLeft");
    await item("a.txt", pane(1)).waitFor();
    await page.keyboard.press("Alt+ArrowRight");
    await pane(1).locator(".list-empty").waitFor();
    await item("a.txt", pane(0)).waitFor();
    await shotBoth("1-two-panes");
  });

  await step("the splitter shares the room between the panes, and a click gives a pane the keyboard", async () => {
    const before = (await pane(0).boundingBox()).width;
    const handle = await centre(page.locator(".pane-splitter"));
    await page.mouse.move(handle.x, handle.y);
    await page.mouse.down();
    await page.mouse.move(handle.x - 120, handle.y, { steps: 6 });
    await page.mouse.up();
    const after = (await pane(0).boundingBox()).width;
    assert(Math.abs(before - 120 - after) < 6, `the first pane is ${after}px wide, was ${before}px`);
    assert((await snapshot()).split < 0.5);
    const box = await list(pane(0)).boundingBox();
    await page.mouse.click(box.x + box.width / 2, box.y + box.height - 20);
    await page.locator('.pane-bar[data-pane="0"].active').waitFor();
  });

  await step("Copy to other pane and Move to other pane go through the undo history", async () => {
    await item("a.txt", pane(0)).click();
    await page.keyboard.press("Alt+c");
    await item("a.txt", pane(1)).waitFor();
    assert(onDisk(right, "a.txt") && onDisk(left, "a.txt"), "copied, and the original stays");
    await pane(1).locator(".row.selected .label", { hasText: exactly("a.txt") }).waitFor();
    // From the item's menu.
    await item("b.txt", pane(0)).click({ button: "right" });
    await menuItem("Move to other pane").click();
    await item("b.txt", pane(1)).waitFor();
    assert(onDisk(right, "b.txt") && !onDisk(left, "b.txt"));
    await list(pane(0)).focus();
    await page.keyboard.press("Control+z");
    await notice(/Undid move of b\.txt/);
    await item("b.txt", pane(0)).waitFor();
    assert(onDisk(left, "b.txt") && !onDisk(right, "b.txt"));
    await item("c.log", pane(0)).click();
    await page.keyboard.press("Alt+x");
    await item("c.log", pane(1)).waitFor();
    assert(!onDisk(left, "c.log"));
    await page.keyboard.press("Control+z");
    await notice(/Undid move of c\.log/);
    await item("c.log", pane(0)).waitFor();
    assert(onDisk(left, "c.log") && !onDisk(right, "c.log"));
    await page.keyboard.press("Control+y");
    await notice(/Redid move of c\.log/);
    await item("c.log", pane(1)).waitFor();
    await page.keyboard.press("Control+z");
    await notice(/Undid move of c\.log/);
    if (recycle) {
      // Undoing the copy sends the copy to the Recycle Bin.
      await page.keyboard.press("Control+z");
      await notice(/Undid copy of a\.txt/);
      await item("a.txt", pane(1)).waitFor({ state: "detached" });
      assert(!onDisk(right, "a.txt") && onDisk(left, "a.txt"));
    }
    await shotBoth("2-copied-to-other-pane");
  });

  await step("files dragged from one pane drop into the folder the other shows", async () => {
    const dragged = join(left, "notes-01.txt");
    await hook.keys({ ctrl: false, shift: false });
    const target = await centre(list(pane(1)));
    await hook.enter([dragged], target);
    await page.locator(".drop-hint", { hasText: "Move to right" }).waitFor();
    assert(await pane(1).evaluate((panel) => panel.classList.contains("drop-target")), "the other pane's folder is the target");
    const result = await hook.drop([dragged], target);
    assert.deepEqual(result, { effect: "move", names: ["notes-01.txt"] });
    assert(onDisk(right, "notes-01.txt") && !onDisk(dragged));
    await pane(1).locator(".row.selected .label", { hasText: exactly("notes-01.txt") }).waitFor();
    await list(pane(0)).focus();
    await page.keyboard.press("Control+z");
    await notice(/Undid move of notes-01\.txt/);
    assert(onDisk(dragged));
    await hook.keys(undefined);
  });

  await step("the layout (two panes, the split, each pane's tabs) comes back when the app starts", async () => {
    // The first pane gets a second tab; the second pane keeps the keyboard.
    await list(pane(0)).focus();
    await page.keyboard.press("Control+t");
    await page.locator('.pane-bar[data-pane="0"] .pane-tab').nth(1).waitFor();
    await open(many, "item-000.txt");
    // The second pane gets a tab on a folder that is deleted while the app is closed.
    await list(pane(1)).click({ position: { x: 20, y: 200 } });
    await page.locator('.pane-bar[data-pane="1"].active').waitFor();
    await page.keyboard.press("Control+t");
    await page.locator('.pane-bar[data-pane="1"] .pane-tab').nth(1).waitFor();
    await open(gone, "doomed.txt");
    await page.locator('.pane-bar[data-pane="1"] .pane-tab').first().click();
    await page.locator('.pane-bar[data-pane="1"] .pane-tab.shown .tab-title', { hasText: exactly("right") }).waitFor();
    const saved = await snapshot();
    assert.equal(saved.panes[0].tabs.length, 2);
    assert.equal(saved.panes[1].tabs.length, 2);
    await page.evaluate(() => window.__kestrelLayout.flush());
    await restart(() => rmSync(gone, { recursive: true, force: true }));
    await page.locator(".pane-bar").nth(1).waitFor({ timeout: 60_000 });
    await page.locator('.pane-bar[data-pane="1"].active').waitFor();
    const titles = await page.locator('.pane-bar[data-pane="0"] .tab-title').allTextContents();
    assert.deepEqual(titles, ["left", "many"]);
    // The tab on the deleted folder is left out.
    assert.deepEqual(await page.locator('.pane-bar[data-pane="1"] .tab-title').allTextContents(), ["right"]);
    await item("item-000.txt", pane(0)).waitFor();
    const restored = await snapshot();
    assert(Math.abs(restored.split - saved.split) < 0.01, `split ${restored.split} vs ${saved.split}`);
    await shotBoth("3-restored-after-restart");
  });

  await step("hiding the second pane keeps its tabs for the next time it opens", async () => {
    await list(pane(1)).focus();
    await page.keyboard.press("Control+Shift+D");
    await page.locator(".pane-bar").first().waitFor({ state: "detached" });
    assert.equal(await page.locator(".titlebar .tab").count(), 2, "the first pane's two tabs");
    await list().focus();
    await page.keyboard.press("Control+Shift+D");
    await pane(1).locator(".list-empty, .row").first().waitFor();
    assert.deepEqual(await page.locator('.pane-bar[data-pane="1"] .tab-title').allTextContents(), ["right"]);
    await page.keyboard.press("Control+Shift+D");
    await page.locator(".pane-bar").first().waitFor({ state: "detached" });
    // One tab again, as the remaining steps expect; each close is awaited, since closing the
    // last tab closes the window.
    for (let count = await page.locator(".titlebar .tab").count(); count > 1; count--) {
      await page.keyboard.press("Control+w");
      await page.waitForFunction((left) => document.querySelectorAll(".titlebar .tab").length === left, count - 1);
    }
  });

  await step("grouping by date modified uses Explorer's names, in local time, newest first", async () => {
    await open(dated, "now.txt");
    await viewMenu("Date modified", "Group by");
    await active().locator(".group-header").first().waitFor();
    assert.deepEqual(await groupLabels(), ["Today", "Yesterday", "A long time ago"]);
    assert.deepEqual(await active().locator(".group-count").allTextContents(), ["1", "1", "1"]);
    await headersClearOfItems();
    await shotBoth("4-grouped-by-date");
    await viewMenu("Ascending", "Group by");
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .group-label")?.textContent === "A long time ago");
    assert.deepEqual(await groupLabels(), ["A long time ago", "Yesterday", "Today"]);
    assert.deepEqual(await labels(), ["ancient.txt", "yesterday.txt", "now.txt"]);
  });

  await step("grouping by size puts folders apart and files in Explorer's size ranges", async () => {
    await open(sized, "tiny.txt");
    assert.equal(await active().locator(".group-header").count(), 0, "grouping belongs to the folder it was chosen in");
    await viewMenu("Size", "Group by");
    await active().locator(".group-header").first().waitFor();
    assert.deepEqual(await groupLabels(), [
      "Medium (1 - 128 MB)",
      "Small (16 KB - 1 MB)",
      "Tiny (0 - 16 KB)",
      "Empty (0 KB)",
      "Unspecified",
    ]);
    assert.deepEqual(await labels(), ["medium.bin", "small.bin", "tiny.txt", "empty.txt", "Folder"]);
    // The folder's own menu offers Group by too, with the grouping ticked.
    const box = await list().boundingBox();
    await page.mouse.click(box.x + box.width / 2, box.y + box.height - 30, { button: "right" });
    await menuItem("Group by").hover();
    assert.equal(await menuItem("Size", page.locator(".menu .menu")).getAttribute("aria-checked"), "true");
    await page.keyboard.press("Escape");
    await page.keyboard.press("Escape");
    await menu().waitFor({ state: "detached" });
  });

  await step("groups collapse, the keyboard crosses them, and Ctrl+A takes every group", async () => {
    await open(left, "a.txt");
    await viewMenu("Type", "Group by");
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .group-header").length === 4);
    assert.deepEqual(await groupLabels(), ["Folder", "LOG file", "PNG file", "TXT file"]);
    await viewMenu("Name", "Group by");
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .group-label")?.textContent === "A - H");
    assert.deepEqual(await groupLabels(), ["A - H", "I - P", "Q - Z"]);
    // Collapse "I - P": its items go, its header stays.
    await active().locator('.group-header[aria-label="I - P"] .group-toggle').click();
    await item("notes-01.txt").waitFor({ state: "detached" });
    await active().locator('.group-header.collapsed[aria-label="I - P"]').waitFor();
    await headersClearOfItems();
    await shotBoth("5-collapsed-group");
    // Down from the last item of "A - H" skips the collapsed group to "Q - Z".
    await item("c.log").click();
    await page.keyboard.press("ArrowDown");
    await active().locator(".row.focused .label", { hasText: exactly("Sub") }).waitFor();
    await page.keyboard.press("Home");
    await active().locator(".row.focused .label", { hasText: exactly("a.txt") }).waitFor();
    await page.keyboard.press("Control+a");
    assert.match(await status(), new RegExp(`${LEFT_ITEMS} selected`));
    await active().locator('.group-header[aria-label="I - P"] .group-toggle').click();
    await item("notes-01.txt").waitFor();
    // Clicking a header selects its group.
    await active().locator('.group-header[aria-label="I - P"] .group-label').click();
    assert.match(await status(), new RegExp(`${NOTES.length + 1} selected`));
  });

  await step("groups lay out in every view, and the keyboard moves between them in a grid", async () => {
    for (const [digit, mode] of [[3, "medium"], [5, "list"], [7, "tiles"], [8, "content"], [1, "xl"], [6, "details"]]) {
      await list().focus();
      await page.keyboard.press(`Control+Shift+Digit${digit}`);
      await viewIs(mode);
      await active().locator(".group-header").first().waitFor();
      await item("a.txt").waitFor();
      await headersClearOfItems();
      if (mode !== "details" && mode !== "xl") await shotBoth(`6-grouped-${mode}`);
      if (mode === "medium") {
        // From the last line of "A - H", Down lands in "I - P", in the same column.
        await item("a.txt").click();
        await page.keyboard.press("ArrowDown");
        await active().locator(".view-item.focused .label", { hasText: exactly("notes-01.txt") }).waitFor();
      }
    }
  });

  await step("grouping is remembered per folder, and search results group too", async () => {
    await open(right, "a.txt");
    assert.equal(await active().locator(".group-header").count(), 0);
    await open(left, "a.txt");
    await active().locator(".group-header").first().waitFor();
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    const scope = page.locator(".searchbox .scope");
    if ((await scope.getAttribute("aria-pressed")) === "true") await scope.click();
    await page.keyboard.press("Control+f");
    await page.keyboard.type("notes-");
    await page.waitForFunction((count) => new RegExp(`^${count} results`).test(document.querySelector(".statusbar")?.textContent ?? ""), NOTES.length, {
      timeout: 30_000,
    });
    // Search results have a grouping of their own: none yet.
    await item("notes-02.txt").waitFor();
    assert.equal(await active().locator(".group-header").count(), 0);
    await viewMenu("Size", "Group by");
    await active().locator(".group-header").first().waitFor();
    assert.deepEqual(await groupLabels(), ["Tiny (0 - 16 KB)"]);
    await shotBoth("7-grouped-search");
    await viewMenu("None", "Group by");
    await page.locator(".searchbox input").press("Escape");
    await item("a.txt").waitFor();
    await viewMenu("None", "Group by");
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .group-header").length === 0);
  });

  await step("the column menu shows and hides columns; headers move and resize, and stay so after a reload", async () => {
    const header = (id) => active().locator(`.column-header.${id}`);
    const order = () => active().locator(".column-cell").evaluateAll((cells) => cells.map((cell) => cell.dataset.column));
    const width = async (id) => (await active().locator(`.column-cell[data-column="${id}"]`).boundingBox()).width;
    await header("name").click({ button: "right" });
    await menuItem("Size all columns to fit").waitFor();
    await menuItem("Allocated size").waitFor();
    await menuItem("Attributes").click();
    await header("attributes").waitFor();
    await active().locator(".row .cell.attributes", { hasText: "A" }).first().waitFor();
    await header("name").click({ button: "right" });
    await menuItem("Type").click();
    await header("type").waitFor({ state: "detached" });
    assert.deepEqual(await order(), ["name", "modified", "size", "attributes"]);
    // Drag Size before Date modified.
    const from = await centre(header("size"));
    const to = await active().locator('.column-cell[data-column="modified"]').boundingBox();
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(to.x + 10, from.y, { steps: 8 });
    await page.mouse.up();
    assert.deepEqual(await order(), ["name", "size", "modified", "attributes"]);
    // Drag Date modified's right edge 60px further.
    const before = await width("modified");
    const edge = await active().locator('.column-cell[data-column="modified"] .column-resizer').boundingBox();
    await page.mouse.move(edge.x + edge.width / 2, edge.y + edge.height / 2);
    await page.mouse.down();
    await page.mouse.move(edge.x + edge.width / 2 + 60, edge.y + edge.height / 2, { steps: 6 });
    await page.mouse.up();
    assert(Math.abs((await width("modified")) - before - 60) < 3, `${await width("modified")} vs ${before} + 60`);
    // Double-clicking the attributes edge fits the column to its letters.
    await active().locator('.column-cell[data-column="attributes"] .column-resizer').dblclick();
    const fitted = await width("attributes");
    assert(fitted < 140 && fitted >= 48, `attributes fitted to ${fitted}px`);
    // Sorting by an extra column.
    await header("attributes").click();
    assert.equal(await header("attributes").getAttribute("aria-sort"), "ascending");
    await shotBoth("8-columns");
    const widths = { modified: await width("modified"), attributes: await width("attributes") };
    await page.waitForTimeout(400);
    await page.reload();
    await active().locator(".row").first().waitFor();
    await open(left, "a.txt");
    assert.deepEqual(await order(), ["name", "size", "modified", "attributes"]);
    assert(Math.abs((await width("modified")) - widths.modified) < 2);
    assert(Math.abs((await width("attributes")) - widths.attributes) < 2);
    // Search results keep columns of their own.
    await page.keyboard.press("Control+f");
    await page.keyboard.type("notes-0");
    await active().locator(".filelist.results .row .label", { hasText: exactly("notes-02.txt") }).waitFor();
    assert.deepEqual(await order(), ["name", "location", "modified", "size"]);
    await page.locator(".searchbox input").press("Escape");
    await item("a.txt").waitFor();
    await header("name").click({ button: "right" });
    await menuItem("Size all columns to fit").click();
    await header("name").click({ button: "right" });
    await menuItem("Reset columns").click();
    await header("type").waitFor();
    assert.deepEqual(await order(), ["name", "modified", "type", "size"]);
    await header("name").click();
  });

  await step("the filter box narrows the folder as you type, keeps the selection and clears with Esc", async () => {
    await page.keyboard.press("Control+Shift+L");
    const input = active().locator(".filter-bar input");
    await input.waitFor();
    assert(await input.evaluate((element) => element === document.activeElement), "the box takes the keyboard");
    await page.keyboard.type("notes");
    await page.waitForFunction((count) => document.querySelectorAll(".tab-panel.active .row").length === count, NOTES.length);
    assert.match(await status(), new RegExp(`^${NOTES.length} of ${LEFT_ITEMS} items`));
    await item("notes-11.txt").click();
    await input.focus();
    await page.keyboard.type("-1");
    // notes-10, 11 and 12; notes-11 stays selected.
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .row").length === 3);
    await active().locator(".row.selected .label", { hasText: exactly("notes-11.txt") }).waitFor();
    assert.match(await status(), new RegExp(`^3 of ${LEFT_ITEMS} items`));
    await shotBoth("9-filter");
    await input.fill("*.png");
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .row").length === 1);
    await item("photo.png").waitFor();
    await input.fill("no such name");
    await active().locator(".list-empty", { hasText: "No names here contain" }).waitFor();
    // An item the filter leaves out drops out of the selection; what stays in view stays selected.
    await input.fill("notes-1");
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .row").length === 3);
    await item("notes-11.txt").click();
    await input.press("Escape");
    await active().locator(".filter-bar").waitFor({ state: "detached" });
    await page.waitForFunction((count) => document.querySelectorAll(".tab-panel.active .row").length === count, LEFT_ITEMS);
    assert.match(await status(), new RegExp(`^${LEFT_ITEMS} items`));
    await active().locator(".row.selected .label", { hasText: exactly("notes-11.txt") }).waitFor();
    // With the box closed, typing jumps to a name as before.
    await list().focus();
    await page.keyboard.type("pho");
    await active().locator(".row.focused .label", { hasText: exactly("photo.png") }).waitFor();
    // The toolbar's button opens it too.
    await page.locator(".filter-toggle").click();
    await input.waitFor();
    await input.press("Escape");
    await active().locator(".filter-bar").waitFor({ state: "detached" });
  });

  await step("a rectangle drawn from empty space selects what it touches, in every view; Ctrl adds", async () => {
    // Details: from below the last row up to the first.
    const box = await list().boundingBox();
    const firstRow = await item("Sub").boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height - 20);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 40, firstRow.y + 5, { steps: 10 });
    await page.locator(".tab-panel.active .marquee").waitFor();
    await shotBoth("10-marquee-details");
    await page.mouse.up();
    assert.match(await status(), new RegExp(`${LEFT_ITEMS} selected`));
    // Ctrl adds the last two rows to a single selection.
    await item("a.txt").click();
    const lastTwo = await item("notes-12.txt").boundingBox();
    await page.keyboard.down("Control");
    await page.mouse.move(box.x + box.width / 2, box.y + box.height - 20);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 30, lastTwo.y + 5, { steps: 6 });
    await page.mouse.up();
    await page.keyboard.up("Control");
    assert.match(await status(), /3 selected/);
    // The other views, from the empty space at the bottom left: past the list view's column and
    // below the tiles; beside the Content view's rows, which fill the view, so it takes those in view.
    for (const [digit, mode] of [[5, "list"], [7, "tiles"], [8, "content"]]) {
      await list().focus();
      await page.keyboard.press(`Control+Shift+Digit${digit}`);
      await viewIs(mode);
      await item("a.txt").waitFor();
      const view = await list().boundingBox();
      const start = { x: view.x + 3, y: view.y + view.height - 18 };
      await page.mouse.click(start.x, start.y);
      assert.doesNotMatch(await status(), /selected/, `a click on empty space clears the selection in the ${mode} view`);
      const first = await item("Sub").boundingBox();
      await page.mouse.move(start.x, start.y);
      await page.mouse.down();
      await page.mouse.move(view.x + view.width - 24, first.y + 4, { steps: 10 });
      await page.mouse.up();
      const selected = Number((await status()).match(/(\d+) selected/)?.[1] ?? 0);
      if (mode === "content") assert(selected >= 5 && selected < LEFT_ITEMS, `${selected} selected in the content view`);
      else assert.equal(selected, LEFT_ITEMS, `in the ${mode} view`);
    }
    await list().focus();
    await page.keyboard.press("Control+Shift+Digit6");
    await viewIs("details");
  });

  await step("the rectangle scrolls the list at its edge and selects items that were never on screen", async () => {
    await open(many, "item-000.txt");
    await list().focus();
    await page.keyboard.press("Control+Shift+Digit3");
    await viewIs("medium");
    await item("item-000.txt").waitFor();
    const view = await list().boundingBox();
    const onScreen = await active().locator(".view-item").count();
    // From the grid's left padding, down past the bottom edge, where the list scrolls.
    await page.mouse.move(view.x + 3, view.y + 6);
    await page.mouse.down();
    await page.mouse.move(view.x + view.width - 30, view.y + view.height + 20, { steps: 12 });
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .list-viewport").scrollTop > 1500, null, { timeout: 15_000 });
    await page.mouse.up();
    const selected = Number((await status()).match(/([\d,]+) selected/)?.[1].replace(/,/g, "") ?? 0);
    assert(selected > onScreen, `${selected} selected, more than the ${onScreen} rendered at first`);
    // The first items have left the DOM by now, and are still selected when they come back.
    assert.equal(await item("item-000.txt").count(), 0, "the first item is no longer rendered");
    await list().evaluate((viewport) => (viewport.scrollTop = 0));
    await active().locator(".view-item.selected .label", { hasText: exactly("item-000.txt") }).waitFor();
    await list().focus();
    await page.keyboard.press("Control+Shift+Digit6");
    await viewIs("details");
  });

  await step("the Content view shows each item on a wide row with its details, at Explorer's Ctrl+Shift+8", async () => {
    await open(left, "a.txt");
    await page.locator(".view-button").click();
    await menu().waitFor();
    assert.match(await menuItem("Content").innerText(), /Ctrl\+Shift\+8/);
    assert.match(await menuItem("Tiles").innerText(), /Ctrl\+Shift\+7/);
    await page.keyboard.press("Escape");
    await menu().waitFor({ state: "detached" });
    await list().focus();
    await page.keyboard.press("Control+Shift+Digit8");
    await viewIs("content");
    // Typing its name brings the picture, last in the folder, into view.
    await page.keyboard.type("photo");
    const photo = item("photo.png");
    await photo.locator(".content-detail", { hasText: "Dimensions: 64 x 32" }).waitFor();
    await photo.locator(".content-detail", { hasText: /Date modified:/ }).waitFor();
    await photo.locator(".content-detail", { hasText: /Size:/ }).waitFor();
    assert.equal(await photo.locator(".view-item-detail").innerText(), "PNG file");
    const row = await photo.boundingBox();
    const view = await list().boundingBox();
    assert(row.width > view.width * 0.8, "one wide row per item");
    await page.locator(".view-item-thumb.shown").first().waitFor({ timeout: 30_000 });
    await shotBoth("11-content");
    await page.keyboard.press("Control+Shift+Digit7");
    await viewIs("tiles");
    await page.keyboard.press("Control+Shift+Digit6");
    await viewIs("details");
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.mouse.up().catch(() => {});
  await page.keyboard.up("Control").catch(() => {});
  await page.evaluate(() => window.__kestrelDragDrop?.keys(undefined)).catch(() => {});
  // Put the window's settings and layout back as they were, then leave it somewhere that still exists.
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  for (const name of LAYOUT_DOCS) await invoke("layout_save", { name, value: savedLayout.get(name) ?? "" }).catch(() => {});
  await page.reload().catch(() => {});
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await session.detach().catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
