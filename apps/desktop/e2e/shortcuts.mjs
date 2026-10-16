// End-to-end test of the shortcuts: pinned places and their groups, frequent folders and Go to
// folder, saved searches, tags and workspaces, against the real app window.
//
// File changes happen only inside a scratch folder this script creates under the temp directory
// and removes afterwards. Everything the script adds to the slot's database (pins, groups, tags,
// saved searches, workspaces, and visits to two folders under C:\Windows that it only reads) is
// removed again at the end, and the window's view settings are put back, so the slot is left as
// it was found.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/shortcuts.mjs`. Screenshots go to e2e/out/shortcuts, dark and light.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "shortcuts");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-shortcuts-${process.pid}`);
const at = (...parts) => join(sandbox, ...parts);
rmSync(sandbox, { recursive: true, force: true });
for (const folder of ["Alpha", "Beta", "Gamma", "Epsilon", join("Delta", `quokka-${process.pid}`)]) mkdirSync(at(folder), { recursive: true });
writeFileSync(at("report.txt"), "quarterly report");
writeFileSync(at("notes.txt"), "notes");
writeFileSync(at("budget.txt"), "x".repeat(4000));
writeFileSync(at("Beta", "big.bin"), "x".repeat(50_000));
writeFileSync(at("Beta", "small.txt"), "x");
const QUOKKA = `quokka-${process.pid}`;
/** Folders outside the sandbox the suite visits (only reads) for the Frequent list and Go to folder. */
const WINDOWS = process.env.SystemRoot ?? "C:\\Windows";
const VISITED = [join(WINDOWS, "Help"), join(WINDOWS, "Web")];

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
// Start from the defaults: details everywhere, the preview pane closed, searching everywhere.
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const sidebar = page.locator(".sidebar");
const notice = page.locator(".notice");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const item = (name) => panel.locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const pin = (label) => sidebar.locator(".shelf-item.pin").filter({ has: page.locator(".shelf-label", { hasText: exactly(label) }) });
const group = (name) => sidebar.locator(".pin-group").filter({ has: page.locator(".pin-group-name", { hasText: exactly(name) }) });
const groupHead = (name) => group(name).locator(".pin-group-head");
const shelf = (section, label) =>
  sidebar.locator(`section[aria-label="${section}"] .shelf-item`).filter({ has: page.locator(".shelf-label", { hasText: exactly(label) }) });
const menu = page.locator(".menu");
const menuItem = (label) => menu.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) });
const dialog = page.locator(".shortcut-dialog");
const crumb = () => page.locator(".crumb").last().innerText();
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** What the sidebar is built from, read through the dev build's hook. */
const hook = {
  read: (what) => page.evaluate((what) => window.__kestrelShortcuts[what](), what),
  call: (method, ...args) => page.evaluate(([method, args]) => window.__kestrelShortcuts.api[method](...args), [method, args]),
  refresh: () => page.evaluate(() => window.__kestrelShortcuts.refresh()),
};
const drops = {
  enter: (paths, point) => page.evaluate(([paths, point]) => window.__kestrelDragDrop.enter(paths, point.x, point.y), [paths, point]),
  drop: (paths, point) => page.evaluate(([paths, point]) => window.__kestrelDragDrop.drop(paths, point.x, point.y), [paths, point]),
  leave: () => page.evaluate(() => window.__kestrelDragDrop.leave()),
};

async function centre(locator) {
  const box = await locator.boundingBox();
  assert(box, "element is on screen");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Opens a folder through the address bar. */
async function open(path, expectName) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, basename(path));
  if (expectName) await item(expectName).waitFor();
}

/** Presses on `source` and drags it, with the mouse, to the top, middle or bottom of `target`. */
async function dragTo(source, target, where = "middle", beforeRelease = async () => {}) {
  const from = await centre(source);
  const box = await target.boundingBox();
  const y = where === "top" ? box.y + 3 : where === "bottom" ? box.y + box.height - 3 : box.y + box.height / 2;
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x, from.y + 6, { steps: 2 });
  await page.mouse.move(box.x + box.width / 2, y, { steps: 6 });
  await beforeRelease();
  await page.mouse.up();
}

/** Labels of the pins, in sidebar order, as the backend has them. */
const pinOrder = async () => (await hook.read("board")).pins.map((p) => p.alias ?? basename(p.path));

/** Captured through the debugging protocol, which unlike Playwright's own does not resize the window. */
async function shot(name) {
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(data, "base64"));
}
async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    await shot(`${name}-${scheme}`);
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Waits for `check` to hold, retrying for up to `ms`, for what the index learns from the journal. */
async function eventually(check, ms = 10_000) {
  const until = Date.now() + ms;
  for (;;) {
    try {
      return await check();
    } catch (error) {
      if (Date.now() > until) throw error;
      await page.waitForTimeout(250);
    }
  }
}

// What the slot's database held before, so exactly what this suite adds is removed afterwards.
await page.waitForFunction(() => typeof window.__kestrelShortcuts === "object");
await hook.refresh();
const before = {
  pins: new Set((await hook.read("board")).pins.map((p) => p.id)),
  groups: new Set((await hook.read("board")).groups.map((g) => g.id)),
  tags: new Set((await hook.read("tags")).tags.map((t) => t.id)),
  searches: new Set((await hook.read("searches")).map((s) => s.id)),
  workspaces: new Set((await hook.read("workspaces")).map((w) => w.id)),
  // Visits to these are forgotten afterwards only if the suite made them.
  visited: new Set((await hook.call("goto", "", 60)).map((hit) => hit.path.toLowerCase())),
};
for (const name of ["Work stuff", "Home stuff"]) {
  assert(!(await hook.read("board")).groups.some((g) => g.name === name), `a group named ${name} already exists; remove it first`);
}
assert(!(await hook.read("tags")).tags.some((t) => t.name.toLowerCase() === "urgent"), "a tag named Urgent already exists; remove it first");

let passed = false;
try {
  await step("the sidebar has pinned places above Places", async () => {
    await panel.locator(".row").first().waitFor();
    const headings = await sidebar.locator("h2").allTextContents();
    assert(headings.indexOf("Pinned") === 0 && headings.indexOf("Places") === 1, headings.join(", "));
    await open(sandbox, "report.txt");
  });

  await step("Pin to sidebar in an item's menu pins a folder", async () => {
    await row("Alpha").click({ button: "right" });
    await menuItem("Pin to sidebar").click();
    await pin("Alpha").waitFor();
    await notice.filter({ hasText: "Pinned 1 item to the sidebar" }).waitFor();
    assert.equal(await pin("Alpha").getAttribute("data-place"), at("Alpha"), "a pinned folder takes drops like a place");
  });

  await step("Ctrl+D pins the selection, or the folder itself when nothing is selected", async () => {
    await row("report.txt").click();
    await page.keyboard.press("Control+d");
    await pin("report.txt").waitFor();
    assert.equal(await pin("report.txt").getAttribute("data-place"), null, "a pinned file is not a drop target");
    await page.keyboard.press("Escape");
    await page.keyboard.press("Control+d");
    await pin(basename(sandbox)).waitFor();
    // Pinning again does not pin twice.
    await page.keyboard.press("Control+d");
    await notice.filter({ hasText: "Already pinned" }).waitFor();
    assert.equal((await pinOrder()).filter((label) => label === basename(sandbox)).length, 1);
  });

  await step("dragging items onto the pinned places pins them", async () => {
    const zone = await centre(sidebar.locator(".pinned .shelf-head"));
    await drops.enter([at("Beta")], zone);
    await page.locator(".pinned.drop-target").waitFor();
    await page.locator(".drop-hint", { hasText: "Pin to Pinned" }).waitFor();
    await shotBoth("1-drop-to-pin");
    const result = await drops.drop([at("Beta")], zone);
    assert.equal(result.effect, "link");
    await pin("Beta").waitFor();
    assert(existsSync(at("Beta")), "pinning moves nothing");
    assert.deepEqual((await pinOrder()).slice(-4), ["Alpha", "report.txt", basename(sandbox), "Beta"]);
  });

  await step("groups are created, renamed, coloured and collapsed", async () => {
    await sidebar.locator(".pinned .shelf-add").click();
    await dialog.waitFor();
    await page.keyboard.type("Work");
    await dialog.locator('.swatch[data-color="blue"]').click();
    await shotBoth("2-new-group-dialog");
    await dialog.locator(".dialog-button.primary").click();
    await group("Work").waitFor();
    assert.equal(await group("Work").getAttribute("data-color"), "blue");

    await groupHead("Work").click({ button: "right" });
    await menuItem("Rename and colour…").click();
    await dialog.waitFor();
    await page.keyboard.press("Control+a");
    await page.keyboard.type("Work stuff");
    await dialog.locator('.swatch[data-color="green"]').click();
    await dialog.locator(".dialog-button.primary").click();
    await group("Work stuff").waitFor();
    assert.equal(await group("Work stuff").getAttribute("data-color"), "green");

    await groupHead("Work stuff").click();
    await page.waitForFunction(() => document.querySelector(".pin-group-head")?.getAttribute("aria-expanded") === "false");
    assert.equal(await group("Work stuff").locator(".pin-group-body").count(), 0);
    await groupHead("Work stuff").click();
    await group("Work stuff").locator(".pin-group-body").waitFor();
  });

  await step("pins move into a group and reorder by dragging", async () => {
    await dragTo(pin("Alpha"), groupHead("Work stuff"));
    await group("Work stuff").locator(".shelf-item.pin", { hasText: "Alpha" }).waitFor();
    // Beta, dragged to the top half of report.txt, goes before it.
    await dragTo(pin("Beta"), pin("report.txt"), "top", async () => {
      await pin("report.txt").and(page.locator(".insert-before")).waitFor();
      await shotBoth("3-reorder-pin");
    });
    await eventually(async () => assert.deepEqual((await pinOrder()).slice(-4), ["Beta", "report.txt", basename(sandbox), "Alpha"]));
    const shown = await sidebar.locator(".shelf-item.pin .shelf-label").allTextContents();
    assert.deepEqual(shown.slice(-4), ["Beta", "report.txt", basename(sandbox), "Alpha"]);
  });

  await step("groups reorder by dragging their heading", async () => {
    await hook.call("createGroup", "Home stuff", "orange");
    await hook.refresh();
    await group("Home stuff").waitFor();
    await dragTo(groupHead("Home stuff"), groupHead("Work stuff"), "top");
    await eventually(async () => {
      const names = await sidebar.locator(".pin-group-name").allTextContents();
      assert(names.indexOf("Home stuff") >= 0 && names.indexOf("Home stuff") === names.indexOf("Work stuff") - 1, names.join(", "));
    });
  });

  await step("files dropped on a group are pinned into it", async () => {
    const head = await centre(groupHead("Home stuff"));
    await drops.enter([at("Gamma"), at("Epsilon")], head);
    await page.locator(".drop-hint", { hasText: "Pin to Home stuff" }).waitFor();
    await group("Home stuff").and(page.locator(".drop-target")).waitFor();
    await drops.drop([at("Gamma"), at("Epsilon")], head);
    await group("Home stuff").locator(".shelf-item.pin", { hasText: "Gamma" }).waitFor();
    await group("Home stuff").locator(".shelf-item.pin", { hasText: "Epsilon" }).waitFor();
  });

  await step("an alias renames a pin in the sidebar only", async () => {
    await pin("Beta").click({ button: "right" });
    await menuItem("Edit…").click();
    await dialog.waitFor();
    const name = dialog.locator("input").first();
    assert.equal(await name.getAttribute("placeholder"), "Beta");
    await name.fill("Beta notes");
    await shotBoth("4-edit-pin");
    await dialog.locator(".dialog-button.primary").click();
    await pin("Beta notes").waitFor();
    assert(existsSync(at("Beta")) && !existsSync(at("Beta notes")), "the folder keeps its name");
  });

  await step("Ctrl+1 opens the first pin, and a pin can claim a number of its own", async () => {
    const first = (await hook.read("board")).pins.find((p) => p.key === 1);
    assert(first, "the first pin has Ctrl+1");
    // Pins the slot had before this suite come first; Ctrl+1 is pressed only on one of ours,
    // so it can never open somebody's file in its application.
    if (first.dir && first.path.startsWith(sandbox)) {
      await list.focus();
      await page.keyboard.press("Control+1");
      await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, basename(first.path));
    }

    await pin("Gamma").click({ button: "right" });
    await menuItem("Edit…").click();
    await dialog.waitFor();
    await dialog.locator("select").nth(1).selectOption("1");
    await dialog.locator(".dialog-button.primary").click();
    await pin("Gamma").locator(".shelf-key", { hasText: "1" }).waitFor();
    await list.focus();
    await page.keyboard.press("Control+1");
    await page.waitForFunction(() => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === "Gamma");
    assert.equal((await hook.read("board")).pins.filter((p) => p.key === 1).length, 1, "one pin per number");
  });

  await step("a pin whose folder was renamed shows as missing and is found again by its file id", async () => {
    await open(sandbox, "Alpha");
    renameSync(at("Alpha"), at("Alpha renamed"));
    await row("Alpha renamed").waitFor();
    await pin("Alpha").and(page.locator(".missing")).waitFor();
    await pin("Alpha").locator(".shelf-note", { hasText: "Missing" }).waitFor();
    await shotBoth("5-missing-pin");
    // Clicking looks for it by its file id; the index learns of the rename from the journal,
    // so a click made too soon is told it can't be found, and is made again.
    const missing = sidebar.locator(`.shelf-item.pin[data-pin-id="${await pin("Alpha").getAttribute("data-pin-id")}"]`);
    await eventually(async () => {
      if (await missing.evaluate((element) => element.classList.contains("missing"))) await missing.click();
      await notice.filter({ hasText: "Found Alpha at" }).waitFor({ timeout: 1000 });
    });
    await page.waitForFunction(() => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === "Alpha renamed");
    // The pin now points at the folder's new name, and is no longer missing.
    await pin("Alpha renamed").and(page.locator(":not(.missing)")).waitFor();
    const found = (await hook.read("board")).pins.find((p) => p.path === at("Alpha renamed"));
    assert(found && !found.missing && found.group !== null, JSON.stringify(found));
  });

  await step("a pin whose folder is gone can't be found, and can be removed", async () => {
    await open(sandbox, "Epsilon");
    rmSync(at("Epsilon"), { recursive: true });
    await row("Epsilon").waitFor({ state: "detached" });
    await pin("Epsilon").and(page.locator(".missing")).waitFor();
    await pin("Epsilon").click({ button: "right" });
    await menuItem("Find again").click();
    await notice.filter({ hasText: "Couldn't find Epsilon" }).waitFor();
    await pin("Epsilon").click({ button: "right" });
    await menuItem("Remove from sidebar").click();
    await pin("Epsilon").waitFor({ state: "detached" });
  });

  await step("visits are ranked on the Frequent list, but not those to scratch folders", async () => {
    await open(VISITED[0]);
    await open(VISITED[1]);
    await open(VISITED[0]);
    await shelf("Frequent", "Help").waitFor();
    await shelf("Frequent", "Web").waitFor();
    // The sandbox was visited several times, but it is in the temp folder.
    assert.equal(await hook.call("visit", at("Gamma")), false);
    const frequent = (await hook.read("frequent")).map((folder) => folder.path.toLowerCase());
    assert(!frequent.some((path) => path.startsWith(sandbox.toLowerCase())), frequent.join(", "));
    const [help, web] = VISITED.map((path) => frequent.indexOf(path.toLowerCase()));
    assert(help >= 0 && web >= 0 && help < web, `visited twice ranks higher: ${frequent.join(", ")}`);
  });

  await step("Go to folder (Ctrl+G) offers frecent folders first, then any folder in the index", async () => {
    await list.focus();
    await page.keyboard.press("Control+g");
    const finder = page.locator(".goto");
    await finder.waitFor();
    await page.keyboard.type("help");
    // The frecent folder comes first, ahead of the index's many other folders with "help" in them.
    await page.waitForFunction(
      (path) => document.querySelector(".goto-hit")?.getAttribute("title")?.toLowerCase() === path,
      VISITED[0].toLowerCase(),
    );
    assert.equal((await finder.locator(".goto-hit.active").getAttribute("title")).toLowerCase(), VISITED[0].toLowerCase());
    assert((await finder.locator(".goto-hit").count()) > 1, "the index offers more folders after it");
    await shotBoth("6-go-to-folder");
    await page.keyboard.press("Escape");
    await finder.waitFor({ state: "detached" });

    await open(sandbox, "Gamma");
    await page.keyboard.press("Control+g");
    await finder.waitFor();
    // The folder was made when this suite began; the index learned of it from the journal.
    await eventually(async () => {
      await page.locator(".goto-field input").fill("");
      await page.locator(".goto-field input").fill(QUOKKA);
      await finder.locator(".goto-hit", { hasText: QUOKKA }).first().waitFor({ timeout: 1000 });
    });
    await page.keyboard.press("Enter");
    await finder.waitFor({ state: "detached" });
    await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, QUOKKA);
  });

  await step("a search is saved with its scope and runs from the sidebar", async () => {
    await open(sandbox, "report.txt");
    const scope = page.locator(".searchbox .scope");
    if ((await scope.getAttribute("aria-pressed")) === "true") await scope.click();
    await page.keyboard.press("Control+f");
    await page.keyboard.type("report");
    await row("report.txt").waitFor();
    await page.locator(".save-search").click();
    await dialog.waitFor();
    assert.equal(await dialog.locator("input").inputValue(), "report");
    await page.keyboard.press("Control+a");
    await page.keyboard.type("Reports here");
    await page.keyboard.press("Enter");
    await shelf("Saved searches", "Reports here").waitFor();

    // Elsewhere, searching everywhere: the saved search brings back both its text and its folder.
    await page.locator(".searchbox input").press("Escape");
    await open(VISITED[0]);
    await scope.click();
    assert.equal(await scope.getAttribute("aria-pressed"), "true");
    await shelf("Saved searches", "Reports here").click();
    await row("report.txt").waitFor();
    assert.equal(await page.locator(".searchbox input").inputValue(), "report");
    assert.equal(await scope.getAttribute("aria-pressed"), "false", "searching the saved folder again");
    assert.equal(await panel.locator(".row").count(), 1);

    await shelf("Saved searches", "Reports here").click({ button: "right" });
    await menuItem("Rename…").click();
    await dialog.waitFor();
    await page.keyboard.press("Control+a");
    await page.keyboard.type("Report files");
    await page.keyboard.press("Enter");
    await shelf("Saved searches", "Report files").waitFor();
    await page.locator(".searchbox input").press("Escape");
  });

  await step("tags are put on items from their menu and shown as dots in every view", async () => {
    await open(sandbox, "report.txt");
    await row("report.txt").click({ button: "right" });
    await menuItem("Tags…").click();
    const picker = page.locator(".tag-picker");
    await picker.waitFor();
    await picker.locator(".tag-new input").fill("Urgent");
    await picker.locator('.swatch[data-color="red"]').click();
    await picker.locator(".dialog-button", { hasText: "Add tag" }).click();
    await picker.locator(".tag-choice", { hasText: "Urgent" }).and(page.locator('[aria-checked="true"]')).waitFor();
    await shotBoth("7-tag-picker");
    await picker.locator(".dialog-button.primary").click();
    await row("report.txt").locator('.tag-dot[data-tag="Urgent"]').waitFor();

    // A second item, through the picker's existing tag.
    await row("notes.txt").click({ button: "right" });
    await menuItem("Tags…").click();
    await picker.locator(".tag-choice", { hasText: "Urgent" }).click();
    await picker.locator(".tag-choice", { hasText: "Urgent" }).and(page.locator('[aria-checked="true"]')).waitFor();
    await page.keyboard.press("Escape");
    await row("notes.txt").locator(".tag-dot").waitFor();
    await shelf("Tags", "Urgent").waitFor();
    await shotBoth("8-tag-dots-details");

    for (const [digit, mode] of [[2, "large"], [5, "list"], [7, "tiles"], [4, "small"]]) {
      await list.focus();
      await page.keyboard.press(`Control+Shift+Digit${digit}`);
      await panel.locator(`.filelist[data-view="${mode}"]`).waitFor();
      await item("report.txt").locator('.tag-dot[data-tag="Urgent"]').waitFor();
      if (mode === "tiles" || mode === "large") await shotBoth(`9-tag-dots-${mode}`);
    }
    await page.keyboard.press("Control+Shift+Digit6");
    await panel.locator('.filelist[data-view="details"]').waitFor();
  });

  await step("tags follow their items through renames and moves, by file id", async () => {
    await row("report.txt").click();
    await page.keyboard.press("F2");
    await panel.locator("input.rename").waitFor();
    await page.keyboard.type("report final");
    await page.keyboard.press("Enter");
    await row("report final.txt").locator(".tag-dot").waitFor();
    // Moved by another program into another folder.
    renameSync(at("notes.txt"), at("Beta", "notes.txt"));
    await row("notes.txt").waitFor({ state: "detached" });
    await open(at("Beta"), "notes.txt");
    await eventually(() => row("notes.txt").locator(".tag-dot").waitFor({ timeout: 1000 }));
  });

  await step("tag: finds tagged items, from the search box and from the sidebar", async () => {
    const scope = page.locator(".searchbox .scope");
    if ((await scope.getAttribute("aria-pressed")) !== "true") await scope.click();
    await page.keyboard.press("Control+f");
    await page.keyboard.type("tag:urgent");
    await eventually(async () => {
      const names = (await panel.locator(".row .label").allTextContents()).sort();
      assert.deepEqual(names, ["notes.txt", "report final.txt"]);
    });
    await shotBoth("10-tag-search");
    await page.locator(".searchbox input").fill("tag:urgent ext:txt final");
    await eventually(async () => assert.deepEqual(await panel.locator(".row .label").allTextContents(), ["report final.txt"]));
    await page.locator(".searchbox input").fill("tag:nonesuch");
    await panel.locator(".list-error", { hasText: 'No tag is named "nonesuch"' }).waitFor();
    await page.locator(".searchbox input").press("Escape");

    await shelf("Tags", "Urgent").click();
    await eventually(async () => assert.equal(await panel.locator(".row").count(), 2));
    assert.equal(await page.locator(".searchbox input").inputValue(), "tag:Urgent");
    await page.locator(".searchbox input").press("Escape");
  });

  await step("a tag comes off through the picker", async () => {
    await open(at("Beta"), "notes.txt");
    await row("notes.txt").click({ button: "right" });
    await menuItem("Tags…").click();
    const picker = page.locator(".tag-picker");
    await picker.locator(".tag-choice", { hasText: "Urgent" }).click();
    await picker.locator(".tag-choice", { hasText: "Urgent" }).and(page.locator('[aria-checked="false"]')).waitFor();
    await picker.locator(".dialog-button.primary").click();
    await row("notes.txt").locator(".tag-dot").waitFor({ state: "detached" });
  });

  await step("a workspace saves the tabs with their views and sorts, and restores them", async () => {
    // Beta: sorted by size, in tiles. Gamma, in a second tab: details by name.
    await open(at("Beta"), "big.bin");
    await panel.locator(".column-header.size").click();
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .column-header.size")?.getAttribute("aria-sort") === "descending");
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit7");
    await panel.locator('.filelist[data-view="tiles"]').waitFor();
    await page.keyboard.press("Control+t");
    await page.locator(".tab").nth(1).waitFor();
    await open(at("Gamma"));
    await sidebar.locator('section[aria-label="Workspaces"] .shelf-add').click();
    await dialog.waitFor();
    await page.keyboard.type("Shortcuts test");
    await page.keyboard.press("Enter");
    await shelf("Workspaces", "Shortcuts test").locator(".shelf-count", { hasText: "2" }).waitFor();
    await shotBoth("11-workspace-saved");

    // Change everything: one tab, somewhere else, and Beta back in details by name.
    await page.keyboard.press("Control+w");
    assert.equal(await page.locator(".tab").count(), 1);
    await open(at("Beta"), "big.bin");
    await list.focus();
    await page.keyboard.press("Control+Shift+Digit6");
    await panel.locator('.filelist[data-view="details"]').waitFor();
    await panel.locator(".column-header.name").click();
    await open(VISITED[0]);

    await shelf("Workspaces", "Shortcuts test").click();
    await page.locator(".tab").nth(1).waitFor();
    assert.equal(await page.locator(".tab").count(), 2);
    assert.deepEqual(await page.locator(".tab .tab-title").allTextContents(), ["Beta", "Gamma"]);
    await page.locator(".tab.active .tab-title", { hasText: "Gamma" }).waitFor();
    await page.locator(".tab").first().click();
    await panel.locator('.filelist[data-view="tiles"]').waitFor();
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .view-item .label")[0]?.textContent === "big.bin");
    await page.locator(".tab").nth(1).click();
    await panel.locator('.filelist[data-view="details"]').waitFor();
  });

  await step("the sidebar shows everything together", async () => {
    await open(sandbox, "report final.txt");
    await row("report final.txt").click();
    await page.waitForTimeout(300);
    await sidebar.evaluate((element) => (element.scrollTop = 0));
    await shotBoth("12-sidebar-top");
    await sidebar.evaluate((element) => (element.scrollTop = element.scrollHeight));
    await shotBoth("13-sidebar-bottom");
    await sidebar.evaluate((element) => (element.scrollTop = 0));
  });

  await step("groups, saved searches, tags and workspaces are deleted from their menus", async () => {
    await groupHead("Home stuff").click({ button: "right" });
    await menuItem("Delete group").click();
    await group("Home stuff").waitFor({ state: "detached" });
    // Its pins are kept, above every group.
    await pin("Gamma").waitFor();
    assert.equal((await hook.read("board")).pins.find((p) => p.path === at("Gamma"))?.group, null);

    await shelf("Saved searches", "Report files").click({ button: "right" });
    await menuItem("Delete").click();
    await shelf("Saved searches", "Report files").waitFor({ state: "detached" });

    await shelf("Tags", "Urgent").click({ button: "right" });
    await menuItem("Delete tag").click();
    await shelf("Tags", "Urgent").waitFor({ state: "detached" });
    await row("report final.txt").locator(".tag-dot").waitFor({ state: "detached" });

    await shelf("Workspaces", "Shortcuts test").click({ button: "right" });
    await menuItem("Delete").click();
    await shelf("Workspaces", "Shortcuts test").waitFor({ state: "detached" });
  });

  console.log("\nall checks passed; screenshots are in", out);
  passed = true;
} finally {
  if (!passed) await shot("failure").catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  // Put the window's settings back, then leave it somewhere that still exists, in one tab.
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  await page.reload().catch(() => {});
  await page.locator(".tab-panel.active .row").first().waitFor().catch(() => {});
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  // Long enough for that visit to be recorded, so it is forgotten below like the others.
  await page.waitForTimeout(800);
  // Remove whatever this suite added to the database, whether or not it got that far.
  await page
    .evaluate(
      async ({ before, visited }) => {
        const { api } = window.__kestrelShortcuts;
        const board = await api.board();
        for (const pin of board.pins) if (!before.pins.includes(pin.id)) await api.unpin(pin.id).catch(() => {});
        for (const group of board.groups) if (!before.groups.includes(group.id)) await api.deleteGroup(group.id).catch(() => {});
        for (const tag of (await api.tags()).tags) if (!before.tags.includes(tag.id)) await api.deleteTag(tag.id).catch(() => {});
        for (const search of await api.searches()) if (!before.searches.includes(search.id)) await api.deleteSearch(search.id).catch(() => {});
        for (const workspace of await api.workspaces()) if (!before.workspaces.includes(workspace.id)) await api.deleteWorkspace(workspace.id).catch(() => {});
        for (const path of visited) if (!before.visited.includes(path.toLowerCase())) await api.forgetFolder(path).catch(() => {});
        await window.__kestrelShortcuts.refresh();
      },
      {
        before: Object.fromEntries(Object.entries(before).map(([key, ids]) => [key, [...ids]])),
        visited: [...VISITED, "C:\\"],
      },
    )
    .catch((error) => console.error("cleaning up the database failed:", error));
  await session.detach().catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
