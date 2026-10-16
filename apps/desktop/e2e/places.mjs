// End-to-end test of places: This PC, the Recycle Bin page, the sidebar's folder tree and the
// address bar's completion, against the real app window.
//
// File changes happen only inside a scratch folder this script creates under the temp
// directory and removes afterwards. Two small files from it pass through the real Recycle Bin:
// one is restored, the other deleted permanently from the bin's own page, after its
// confirmation. Only rows whose original location is the scratch folder are ever selected,
// and the bin's other items (those not deleted from the temp folder, where other test runs
// work) are read before and after and must be unchanged. Emptying the bin is never touched.
// Drops go through the dev build's test hook, as in dragdrop.mjs, and the Windows menu runs its
// helper in `verbs` mode, as in shellmenu.mjs, so this needs a dev build. "Show hidden items"
// is switched on and off again, and the window's view options are put back at the end; the
// Places section is folded and unfolded. Screenshots, dark and light, go to e2e/out/places.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/places.mjs`.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "places");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-places-${process.pid}`);
const outside = join(sandbox, "outside");
rmSync(sandbox, { recursive: true, force: true });
for (const folder of ["Alpha/Inner/Deep", "Beta", "Gamma", "outside"]) mkdirSync(join(sandbox, folder), { recursive: true });
writeFileSync(join(sandbox, "notes.txt"), "notes");
mkdirSync(join(sandbox, ".secret"));
execFileSync("attrib", ["+h", join(sandbox, ".secret")]);
writeFileSync(join(outside, "carried.txt"), "carried");
const keepName = `kestrel-test-${process.pid}-keep.txt`;
const goneName = `kestrel-test-${process.pid}-gone.txt`;
writeFileSync(join(sandbox, keepName), "kept through the bin");
writeFileSync(join(sandbox, goneName), "gone for good");
const onDisk = (...parts) => existsSync(join(...parts));
const comparable = (path) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
const temp = comparable(tmpdir());

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
// The view options this suite changes ("Show hidden items"), put back at the end.
const STORAGE_KEYS = ["kestrel.view-options"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const crumbs = () => page.locator(".crumb").allTextContents();
const lastCrumb = async () => (await crumbs()).at(-1);
const tabTitle = () => page.locator(".tab.active .tab-title").textContent();
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const hook = {
  keys: (keys) => page.evaluate((keys) => window.__kestrelDragDrop.keys(keys ?? undefined), keys ?? null),
  enter: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [paths, at]),
  drop: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [paths, at]),
  leave: () => page.evaluate(() => window.__kestrelDragDrop.leave()),
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

/** Types an address and presses Enter. */
async function go(address) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(address);
  await page.keyboard.press("Enter");
}

/** Waits until the active tab shows `path`. */
const arrivedAt = (path) =>
  page.waitForFunction(
    (wanted) => document.querySelector(".tab.active")?.getAttribute("title")?.toLowerCase() === wanted.toLowerCase(),
    path,
    { timeout: 10_000 },
  );

/** The bin's items, read through the backend. */
const binItems = () => invoke("recycle_bin_items");
/** The bin items no test can have touched: those deleted from anywhere but the temp folder. */
const ownersItems = async () =>
  (await binItems())
    .filter((item) => !comparable(item.original).startsWith(`${temp}\\`))
    .map((item) => item.id)
    .sort();
const ours = async () => (await binItems()).filter((item) => comparable(item.folder) === comparable(sandbox));

const tree = page.locator(".tree");
const treeRow = (path) => page.locator(`.tree-row[data-tree-path="${path.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"]`);
const treeRowNamed = (name) => page.locator(".tree-row").filter({ has: page.locator(".tree-label", { hasText: exactly(name) }) });
const focusedTreeLabel = () => page.locator(".tree-row.focused .tree-label").textContent();
const currentTreePath = () => page.locator(".tree-row.current").getAttribute("data-tree-path");

const menu = page.locator(".menu");
const menuLabels = () => menu.locator(".menu-label").allTextContents();

/** Waits for the app to start the Windows menu helper and returns what it recorded. */
async function helperRun() {
  for (let attempt = 0; attempt < 150; attempt++) {
    const record = await invoke("shell_probe_take");
    if (record) return record;
    await page.waitForTimeout(100);
  }
  throw new Error("the helper was not started");
}

const binRow = (name) =>
  page.locator(".bin-row").filter({ has: page.locator(".bin-cell.name .bin-label", { hasText: exactly(name) }) }).filter({
    has: page.locator(".bin-cell.folder", { hasText: exactly(sandbox) }),
  });

let ownersBefore = null;

try {
  await step("This PC lists the drives, each with its usage bar", async () => {
    await panel.locator(".row").first().waitFor();
    await go("This PC");
    await page.locator(".pc-page .pc-tile.drive").first().waitFor();
    assert.equal(await tabTitle(), "This PC");
    assert.deepEqual(await crumbs(), ["This PC"]);
    const system = (process.env.SystemDrive ?? "C:").toUpperCase();
    const tile = page.locator(".pc-tile.drive", { hasText: `(${system})` });
    await tile.locator(".pc-meter span").waitFor();
    const width = await tile.locator(".pc-meter span").evaluate((bar) => parseFloat(bar.style.width));
    assert(width > 0 && width <= 100, `the bar shows how full the drive is (${width} %)`);
    assert.match(await tile.locator(".pc-free").innerText(), /free of/);
    assert.match(await tile.locator(".pc-fs").innerText(), /^(NTFS|ReFS|FAT32|exFAT)$/);
    // Every drive that answered has a bar; a bar of a drive 90 % full or more is red.
    const tiles = await page.locator(".pc-tile.drive").evaluateAll((all) =>
      all.map((element) => ({ bar: element.querySelector(".pc-meter span")?.style.width ?? null, full: element.querySelector(".pc-meter")?.classList.contains("full") ?? false, text: element.innerText })),
    );
    for (const each of tiles.filter((one) => /free of/.test(one.text))) {
      assert(each.bar !== null, `${each.text} has a bar`);
      assert.equal(each.full, parseFloat(each.bar) >= 90, `${each.text}: red from 90 % full`);
    }
    for (const name of ["Desktop", "Documents", "Downloads"]) await page.locator(".pc-tile:not(.drive) .pc-tile-name", { hasText: exactly(name) }).waitFor();
    assert.match(await page.locator(".statusbar").innerText(), /\d+ items/);
    await page.locator(".tree-row.current", { hasText: "This PC" }).waitFor();
    await shotBoth("1-this-pc");
  });

  await step("a drive opens with Enter or a double-click, and Back and Up return to This PC", async () => {
    const system = (process.env.SystemDrive ?? "C:").toUpperCase();
    const tile = page.locator(".pc-tile.drive", { hasText: `(${system})` });
    await tile.click();
    assert.equal(await tile.getAttribute("aria-selected"), "true");
    await page.keyboard.press("Enter");
    await arrivedAt(`${system}\\`);
    await panel.locator(".row").first().waitFor();
    assert.deepEqual(await crumbs(), ["This PC", system]);
    await page.keyboard.press("Alt+ArrowLeft");
    await page.locator(".pc-page").waitFor();
    await tile.dblclick();
    await arrivedAt(`${system}\\`);
    await page.keyboard.press("Alt+ArrowUp");
    await page.locator(".pc-page").waitFor();
    assert.equal(await tabTitle(), "This PC");
    // Up went somewhere new; Back and Forward step through it as through folders.
    await page.keyboard.press("Alt+ArrowLeft");
    await arrivedAt(`${system}\\`);
    await page.keyboard.press("Alt+ArrowRight");
    await page.locator(".pc-page").waitFor();
    assert.equal(await page.locator(".nav button[aria-label='Up']").isDisabled(), true, "This PC has nothing above it");
  });

  await step("a drive's menu opens it, shows its disk usage in the hub, and the Windows menu", async () => {
    const system = (process.env.SystemDrive ?? "C:").toUpperCase();
    const tile = page.locator(".pc-tile.drive", { hasText: `(${system})` });
    await tile.click({ button: "right" });
    await menu.waitFor();
    assert.deepEqual(await menuLabels(), ["Open", "Open in new tab", "Show disk usage", "Properties", "Show more options"]);
    // The disk usage hub opens on the drive, over This PC, and Escape closes it again.
    await menu.locator(".menu-label", { hasText: "Show disk usage" }).click();
    const hub = page.locator(".disk-hub");
    await hub.waitFor();
    assert.equal(await hub.locator('select[aria-label="Drive"]').inputValue(), `${system}\\`);
    await hub.locator("h1").click();
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });
    await page.locator(".tab-panel.active .pc-tile").first().waitFor();
    // The Windows menu, listed by the helper instead of shown (as shellmenu.mjs does).
    await invoke("shell_probe", { mode: "verbs" });
    try {
      await tile.click({ button: "right" });
      await menu.locator(".menu-label", { hasText: "Show more options" }).click();
      const record = await helperRun();
      assert.equal(record.kind, "menu");
      assert.deepEqual(record.paths, [`${system}\\`]);
    } finally {
      await invoke("shell_probe", { mode: "off" });
    }
    await page.locator(".pc-page").focus();
  });

  await step("Explorer's names for This PC and the Recycle Bin are addresses too", async () => {
    await go("shell:RecycleBinFolder");
    await page.locator(".bin-page").waitFor();
    assert.equal(await tabTitle(), "Recycle Bin");
    await go("shell:MyComputerFolder");
    await page.locator(".pc-page").waitFor();
    await go("Recycle Bin");
    await page.locator(".bin-page").waitFor();
    assert.deepEqual(await crumbs(), ["Recycle Bin"]);
    // A new tab opens where the tab is; the palette knows both places.
    await page.keyboard.press("Control+k");
    await page.keyboard.type("this pc");
    await page.locator(".palette-item", { hasText: "This PC" }).first().waitFor();
    await page.keyboard.press("Enter");
    await page.locator(".pc-page").waitFor();
  });

  await step("two scratch files go to the Recycle Bin, and the bin page lists them", async () => {
    ownersBefore = await ownersItems();
    await go(sandbox);
    await row(keepName).click();
    await row(goneName).click({ modifiers: ["Control"] });
    assert.match(await page.locator(".statusbar").innerText(), /2 selected/);
    await list.focus();
    await page.keyboard.press("Delete");
    await row(goneName).waitFor({ state: "detached", timeout: 15_000 });
    await row(keepName).waitFor({ state: "detached" });
    assert(!onDisk(sandbox, keepName) && !onDisk(sandbox, goneName));
    await page.locator(".sidebar .place", { hasText: "Recycle Bin" }).click();
    await page.locator(".bin-page").waitFor();
    await binRow(keepName).waitFor({ timeout: 10_000 });
    await binRow(goneName).waitFor();
    assert.equal((await ours()).length, 2);
    await page.locator(".sidebar .place.current", { hasText: "Recycle Bin" }).waitFor();
  });

  await step("the bin's columns sort", async () => {
    const header = page.locator(".bin-column.name");
    await header.click();
    await page.waitForFunction(() => document.querySelector(".bin-column.name")?.getAttribute("aria-sort") === "ascending");
    const names = await page.locator(".bin-row .bin-label").allTextContents();
    const sorted = [...names].sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" }));
    assert.deepEqual(names, sorted);
    await page.locator(".bin-column.deleted").click();
    await page.waitForFunction(() => document.querySelector(".bin-column.deleted")?.getAttribute("aria-sort") === "descending");
  });

  await step("Restore puts one item back where it was deleted from, as an undoable operation", async () => {
    await binRow(keepName).click();
    assert.equal(await page.locator(".bin-row.selected").count(), 1, "only our item is selected");
    assert.equal(await page.locator(".bin-row.selected .bin-cell.folder").innerText(), sandbox);
    await shotBoth("2-recycle-bin");
    await page.locator(".bin-action", { hasText: /^Restore$/ }).click();
    await binRow(keepName).waitFor({ state: "detached", timeout: 15_000 });
    assert.equal(readFileSync(join(sandbox, keepName), "utf8"), "kept through the bin");
    await page.locator(".notice span", { hasText: `Restored ${keepName}` }).waitFor();
    // Undo sends it back to the bin; redo restores it again.
    await page.locator(".bin-viewport").focus();
    await page.keyboard.press("Control+z");
    await binRow(keepName).waitFor({ timeout: 15_000 });
    assert(!onDisk(sandbox, keepName));
    await page.keyboard.press("Control+y");
    await binRow(keepName).waitFor({ state: "detached", timeout: 15_000 });
    assert(onDisk(sandbox, keepName));
  });

  await step("Delete permanently asks, naming how many, then deletes the other for good", async () => {
    await binRow(goneName).click();
    assert.equal(await page.locator(".bin-row.selected").count(), 1, "only our item is selected");
    assert.equal(await page.locator(".bin-row.selected .bin-cell.folder").innerText(), sandbox);
    await page.keyboard.press("Delete");
    const dialog = page.locator(".confirm-dialog");
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").innerText(), "Delete 1 item permanently?");
    assert.match(await dialog.locator(".confirm-message").innerText(), new RegExp(escape(goneName)));
    await shotBoth("3-delete-permanently");
    // Cancel first: nothing happens.
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    await binRow(goneName).waitFor();
    await page.locator(".bin-action", { hasText: "Delete permanently" }).click();
    await dialog.waitFor();
    await dialog.locator(".dialog-button", { hasText: "Delete" }).click();
    await binRow(goneName).waitFor({ state: "detached", timeout: 15_000 });
    assert(!onDisk(sandbox, goneName), "deleting from the bin does not put it back");
    assert.equal((await ours()).length, 0, "nothing of ours is left in the bin");
    const ownersAfter = await ownersItems();
    assert.deepEqual(ownersAfter, ownersBefore, "the bin's other items are untouched");
    console.log(`    the bin's other items: ${ownersAfter.length} before and after`);
  });

  await step("the folder tree reveals and highlights the open folder", async () => {
    await go(sandbox);
    await arrivedAt(sandbox);
    // The index learns of the scratch folder from the drive's change journal within moments.
    await page.waitForFunction((path) => document.querySelector(".tree-row.current")?.getAttribute("data-tree-path")?.toLowerCase() === path.toLowerCase(), sandbox, { timeout: 15_000 });
    // Every folder above it is expanded (some may be drawn out of view: the tree only draws
    // the rows in view); it is not, until asked.
    const rows = await page.evaluate(() => window.__kestrelTree.rows());
    for (let folder = dirname(sandbox); folder !== dirname(folder); folder = dirname(folder)) {
      assert(rows.some((each) => comparable(each.path) === comparable(folder) && each.expanded), `${folder} is expanded`);
    }
    assert.equal(await treeRow(sandbox).getAttribute("aria-expanded"), "false");
  });

  await step("a chevron expands a folder, with its folders in name order", async () => {
    await treeRow(sandbox).locator(".tree-chevron").click();
    for (const name of ["Alpha", "Beta", "Gamma", "outside"]) await treeRow(join(sandbox, name)).waitFor();
    assert.equal(await treeRow(sandbox).getAttribute("aria-expanded"), "true");
    const labels = await page.locator(".tree-row").evaluateAll((rows, parent) =>
      rows
        .filter((row) => row.getAttribute("data-tree-path")?.toLowerCase().startsWith(`${parent.toLowerCase()}\\`))
        .sort((a, b) => Number(a.dataset.row) - Number(b.dataset.row))
        .map((row) => row.querySelector(".tree-label")?.textContent),
      sandbox,
    );
    assert.deepEqual(labels, ["Alpha", "Beta", "Gamma", "outside"]);
    assert.equal(await treeRow(join(sandbox, "Alpha")).getAttribute("aria-expanded"), "false", "Alpha has a chevron");
    assert.equal(await treeRow(join(sandbox, "Beta")).getAttribute("aria-expanded"), null, "Beta holds no folders");
    await shotBoth("4-tree");
  });

  await step("the tree answers the keys as Explorer's does", async () => {
    // On the name: deep in the tree, the middle of a row can fall on its chevron.
    await treeRow(sandbox).locator(".tree-label").click();
    await tree.focus();
    assert.equal(await focusedTreeLabel(), basename(sandbox));
    await page.keyboard.press("ArrowDown");
    assert.equal(await focusedTreeLabel(), "Alpha");
    await page.keyboard.press("ArrowRight");
    await treeRow(join(sandbox, "Alpha", "Inner")).waitFor();
    assert.equal(await treeRow(join(sandbox, "Alpha")).getAttribute("aria-expanded"), "true");
    await page.keyboard.press("ArrowRight");
    assert.equal(await focusedTreeLabel(), "Inner");
    await page.keyboard.press("ArrowLeft");
    assert.equal(await focusedTreeLabel(), "Alpha", "Left goes to the parent");
    await page.keyboard.press("ArrowLeft");
    await treeRow(join(sandbox, "Alpha", "Inner")).waitFor({ state: "detached" });
    assert.equal(await focusedTreeLabel(), "Alpha", "Left collapses");
    await page.keyboard.type("ga");
    assert.equal(await focusedTreeLabel(), "Gamma", "typing jumps by name");
  });

  await step("the highlight follows navigation, both ways", async () => {
    // The keys resting on a folder open it, as in Explorer.
    await arrivedAt(join(sandbox, "Gamma"));
    assert.equal(comparable(await currentTreePath()), comparable(join(sandbox, "Gamma")));
    await go(join(sandbox, "Beta"));
    await arrivedAt(join(sandbox, "Beta"));
    await page.waitForFunction((path) => document.querySelector(".tree-row.current")?.getAttribute("data-tree-path")?.toLowerCase() === path.toLowerCase(), join(sandbox, "Beta"));
    await treeRow(join(sandbox, "Alpha")).locator(".tree-label").click();
    await arrivedAt(join(sandbox, "Alpha"));
    assert.equal(await lastCrumb(), "Alpha");
  });

  await step("a tree folder's menu opens it and offers the Windows menu", async () => {
    await treeRow(join(sandbox, "Gamma")).locator(".tree-label").click({ button: "right" });
    await menu.waitFor();
    assert.deepEqual(await menuLabels(), [
      "Open",
      "Open in new tab",
      "Pin to sidebar",
      "Properties",
      "Show more options",
      "Expand to open folder",
    ]);
    assert.equal(await menu.locator(".menu-item", { hasText: "Expand to open folder" }).getAttribute("aria-checked"), "true");
    await shotBoth("6-tree-menu");
    await invoke("shell_probe", { mode: "verbs" });
    try {
      await menu.locator(".menu-label", { hasText: "Show more options" }).click();
      const record = await helperRun();
      assert.deepEqual(record.paths.map(comparable), [comparable(join(sandbox, "Gamma"))]);
    } finally {
      await invoke("shell_probe", { mode: "off" });
    }
    await treeRow(join(sandbox, "Alpha")).locator(".tree-label").click({ button: "right" });
    await menu.locator(".menu-label", { hasText: /^Expand$/ }).click();
    await treeRow(join(sandbox, "Alpha", "Inner")).waitFor();
    await treeRow(join(sandbox, "Alpha")).locator(".tree-label").click({ button: "right" });
    await menu.locator(".menu-label", { hasText: /^Collapse$/ }).click();
    await treeRow(join(sandbox, "Alpha", "Inner")).waitFor({ state: "detached" });
  });

  await step("folders made, renamed and deleted elsewhere show in the tree", async () => {
    mkdirSync(join(sandbox, "Delta"));
    await treeRow(join(sandbox, "Delta")).waitFor({ timeout: 15_000 });
    renameSync(join(sandbox, "Delta"), join(sandbox, "Epsilon"));
    await treeRow(join(sandbox, "Epsilon")).waitFor({ timeout: 15_000 });
    await treeRow(join(sandbox, "Delta")).waitFor({ state: "detached" });
    rmSync(join(sandbox, "Epsilon"), { recursive: true });
    await treeRow(join(sandbox, "Epsilon")).waitFor({ state: "detached", timeout: 15_000 });
  });

  await step("hidden folders show in the tree only with Show hidden items", async () => {
    await treeRow(join(sandbox, "Alpha")).waitFor();
    assert.equal(await treeRow(join(sandbox, ".secret")).count(), 0);
    const toggle = async () => {
      await page.locator(".view-button").click();
      await menu.locator(".menu-item", { hasText: "Show hidden items" }).click();
      await menu.waitFor({ state: "detached" });
    };
    await toggle();
    await page.locator(`.tree-row.dimmed[data-tree-path="${join(sandbox, ".secret").replace(/\\/g, "\\\\")}"]`).waitFor({ timeout: 10_000 });
    await toggle();
    await treeRow(join(sandbox, ".secret")).waitFor({ state: "detached", timeout: 10_000 });
    await treeRow(join(sandbox, "Alpha")).waitFor();
  });

  await step("a tree folder takes a drop", async () => {
    const carried = join(outside, "carried.txt");
    await hook.keys({ ctrl: true, shift: false });
    await hook.enter([carried], await centre(treeRow(join(sandbox, "Beta"))));
    await page.locator(".tree-row.drop-target .tree-label", { hasText: exactly("Beta") }).waitFor();
    const target = await page.evaluate(() => window.__kestrelDragDrop.target());
    assert.equal(target?.kind, "place");
    assert.equal(comparable(target.path), comparable(join(sandbox, "Beta")));
    const result = await hook.drop([carried], await centre(treeRow(join(sandbox, "Beta"))));
    assert.equal(result.effect, "copy");
    assert(onDisk(sandbox, "Beta", "carried.txt") && onDisk(outside, "carried.txt"), "copied, as Ctrl asks");
    await hook.keys(null);
  });

  await step("sidebar sections fold away from their headings", async () => {
    const places = page.locator('.sidebar section[aria-label="Places"]');
    await places.locator("h2").click();
    await page.waitForFunction(() => document.querySelector('.sidebar section[aria-label="Places"]')?.classList.contains("collapsed"));
    assert.equal(await places.locator(".place").first().isVisible(), false);
    assert.deepEqual(await page.evaluate(() => window.__kestrelPlaces.sidebar()), ["Places"]);
    await places.locator("h2").click();
    await places.locator(".place").first().waitFor({ state: "visible" });
  });

  await step("the address bar completes folders from an environment variable", async () => {
    const typed = `%TEMP%\\${basename(sandbox)}\\Al`;
    await page.keyboard.press("Control+l");
    const input = page.locator(".address-input");
    await input.fill("");
    await page.keyboard.type(typed);
    const first = page.locator(".address-suggestion").first();
    await first.waitFor();
    assert.equal(comparable(await first.locator(".address-suggestion-name").innerText()), comparable(join(sandbox, "Alpha")));
    await shotBoth("5-completion");
    await page.keyboard.press("Tab");
    assert.equal(comparable(await input.inputValue()), comparable(join(sandbox, "Alpha")));
    assert((await input.inputValue()).endsWith("\\"), "Tab completes into the folder");
    await page.locator(".address-suggestion .address-suggestion-name", { hasText: /Inner$/ }).waitFor();
    await page.keyboard.press("ArrowDown");
    assert.equal(comparable(await input.inputValue()), comparable(join(sandbox, "Alpha", "Inner")));
    await page.keyboard.press("Enter");
    await arrivedAt(join(sandbox, "Alpha", "Inner"));
  });

  await step("shell: names are suggested and lead to their folders", async () => {
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill("");
    await page.keyboard.type("shell:downl");
    await page.locator(".address-suggestion-name", { hasText: /^shell:Downloads$/ }).first().waitFor();
    assert.equal(await page.locator(".address-suggestion-name").first().innerText(), "shell:Downloads", "names starting with it come first");
    await page.keyboard.press("ArrowDown");
    assert.equal(await page.locator(".address-input").inputValue(), "shell:Downloads");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => /downloads$/i.test(document.querySelector(".tab.active")?.getAttribute("title") ?? ""));
    assert.equal((await lastCrumb()).toLowerCase(), "downloads");
    // Typed in full, in any case.
    await go("SHELL:sendto");
    await page.waitForFunction(() => /\\sendto$/i.test(document.querySelector(".tab.active")?.getAttribute("title") ?? ""));
  });

  await step("a typed file path opens its folder with the file selected", async () => {
    await go(`"${join(sandbox, "notes.txt").replace(/\\/g, "/")}"`);
    await arrivedAt(sandbox);
    await panel.locator(".row.selected .label", { hasText: exactly("notes.txt") }).waitFor();
    assert.deepEqual(await panel.locator(".row.selected .label").allTextContents(), ["notes.txt"]);
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  await hook.keys(null).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  // Other suites click the places, so Places is left unfolded whatever happened.
  await invoke("set_sidebar_section", { section: "Places", collapsed: false }).catch(() => {});
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  await page.reload().catch(() => {});
  await panel.locator(".row").first().waitFor().catch(() => {});
  // Should a step have failed with our own files still in the bin, they are put back into the
  // scratch folder (chosen by where they were deleted from), which then goes as a whole.
  const left = await ours().catch(() => []);
  if (left.length > 0) await invoke("recycle_bin_restore", { ids: left.map((item) => item.id) }).catch(() => {});
  await go("C:\\").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
