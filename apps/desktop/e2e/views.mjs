// End-to-end test of the view modes, thumbnails and view options against the real app window.
//
// File changes happen only inside a scratch folder this script creates under the temp
// directory and removes afterwards: a few PNG images it draws itself, text files, a hidden
// file and a subfolder. The window's view settings (kept in its local storage) are saved first
// and put back at the end, so the other suites find the window as it was.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/views.mjs`. Screenshots go to e2e/out/views, in dark and light.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { crc32, deflateSync } from "node:zlib";

import { connectToKestrel } from "./connect.mjs";
import { dataDir } from "./slot.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "views");
mkdirSync(out, { recursive: true });

/** Encodes an RGB image as PNG, so the test carries no image files of its own. */
function png(width, height, pixel) {
  const chunk = (type, data) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([length, body, crc]);
  };
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, 2, 0, 0, 0], 8); // 8 bits per channel, RGB, no interlace
  const raw = Buffer.alloc((width * 3 + 1) * height);
  for (let y = 0; y < height; y++) {
    const line = y * (width * 3 + 1); // each line starts with filter type 0
    for (let x = 0; x < width; x++) raw.set(pixel(x / width, y / height), line + 1 + x * 3);
  }
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  return Buffer.concat([signature, chunk("IHDR", header), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

const mix = (a, b, t) => a.map((value, i) => Math.round(value + (b[i] - value) * t));
/** Name, width, height and drawing of each image. Every one has a different shape. */
const IMAGES = [
  ["aurora.png", 320, 200, (x, y) => mix([20, 40, 90], [60, 220, 160], Math.abs(Math.sin(x * 6 + y * 3)))],
  ["beach.png", 300, 300, (x, y) => (y < 0.55 ? mix([90, 170, 240], [200, 230, 255], y * 1.8) : mix([240, 220, 160], [210, 180, 120], y))],
  ["canyon.png", 200, 320, (x, y) => mix([190, 80, 40], [250, 180, 90], (x + y) / 2)],
  ["dunes.png", 360, 180, (x, y) => (y > 0.5 + 0.15 * Math.sin(x * 9) ? [225, 170, 90] : [250, 225, 170])],
  ["evening.png", 256, 160, (x, y) => mix([250, 120, 60], [60, 30, 110], y)],
  ["forest.png", 240, 240, (x, y) => ((Math.floor(x * 8) + Math.floor(y * 8)) % 2 ? [30, 110, 50] : [90, 170, 80])],
];
const FILLERS = Array.from({ length: 12 }, (_, i) => `notes-${String(i + 1).padStart(2, "0")}.txt`);
const HIDDEN = "secret.txt";

const sandbox = join(tmpdir(), `kestrel-e2e-views-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "Subfolder"), { recursive: true });
for (const [name, width, height, draw] of IMAGES) writeFileSync(join(sandbox, name), png(width, height, draw));
// The largest file, by far, for the sort-by-size check.
writeFileSync(join(sandbox, "notes.txt"), "A plain text file has an icon but no thumbnail.\r\n".repeat(10_000));
FILLERS.forEach((name, i) => writeFileSync(join(sandbox, name), "x".repeat(100 * (i + 1))));
writeFileSync(join(sandbox, "Subfolder", "inside.txt"), "inside");
writeFileSync(join(sandbox, HIDDEN), "hidden");
execFileSync("attrib", ["+h", join(sandbox, HIDDEN)]);
/** Every item the folder shows with hidden items left out. */
const VISIBLE = 1 + IMAGES.length + 1 + FILLERS.length;

const VIEWS = [
  { mode: "xl", label: "Extra large icons", digit: 1 },
  { mode: "large", label: "Large icons", digit: 2 },
  { mode: "medium", label: "Medium icons", digit: 3 },
  { mode: "small", label: "Small icons", digit: 4 },
  { mode: "list", label: "List", digit: 5 },
  { mode: "details", label: "Details", digit: 6 },
  { mode: "tiles", label: "Tiles", digit: 8 },
];
const THUMBNAIL_VIEWS = new Set(["xl", "large", "medium", "small", "tiles"]);

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
// Start from the defaults: every folder in details, hidden items hidden, extensions shown.
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
/** An item, in whatever view is showing, by the name it displays. */
const item = (name) => panel.locator(".view-item, .row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const labels = () => panel.locator(".label").allTextContents();
const status = () => page.locator(".statusbar").innerText();
const viewIs = (mode) => panel.locator(`.filelist[data-view="${mode}"]`).waitFor();
const focusedIndex = async () => Number(await panel.locator(".view-item.focused, .row.focused").getAttribute("data-index"));

/** Captured through the debugging protocol, which unlike Playwright's own capture does not resize the window. */
const shot = async (name) => {
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(data, "base64"));
};
/** Screenshots in both themes. */
const shotBoth = async (name) => {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    await shot(`${name}-${scheme}`);
  }
  await page.emulateMedia({ colorScheme: null });
};

const openViewMenu = async () => {
  await page.locator(".view-button").click();
  const menu = page.locator(".menu");
  await menu.waitFor();
  return menu;
};
/** A menu item by its whole label, so "Open" does not also find "Open with…". */
const menuItem = (menu, label) => menu.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) });
const choose = async (label) => {
  const menu = await openViewMenu();
  await menuItem(menu, label).click();
  await menu.waitFor({ state: "detached" });
};
const isChecked = async (label) => {
  const menu = await openViewMenu();
  const checked = (await menuItem(menu, label).getAttribute("aria-checked")) === "true";
  await page.keyboard.press("Escape");
  await menu.waitFor({ state: "detached" });
  return checked;
};

/** Waits until every named image shows a thumbnail served by the thumb protocol. */
const thumbnailsLoaded = (names) =>
  page.waitForFunction(
    (names) =>
      names.every((name) => {
        const items = [...document.querySelectorAll(".tab-panel.active .view-item")];
        const img = items.find((el) => el.querySelector(".label")?.textContent === name)?.querySelector("img.view-item-thumb.shown");
        return img?.complete && img.naturalWidth > 0 && img.src.startsWith("http://thumb.localhost/");
      }),
    names,
    { timeout: 30_000 },
  );

const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

try {
  await step("the scratch folder opens in details, without its hidden file", async () => {
    await panel.locator(".row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await item("notes.txt").waitFor();
    await viewIs("details");
    assert.match(await status(), new RegExp(`^${VISIBLE} items`));
    assert(!(await labels()).includes(HIDDEN), "hidden files are left out by default");
  });

  await step("the View menu lists every view, the sort order and both options", async () => {
    const menu = await openViewMenu();
    for (const { label, digit } of VIEWS) {
      assert.match(await menuItem(menu, label).innerText(), new RegExp(`Ctrl\\+Shift\\+${digit}`));
    }
    assert.equal(await menuItem(menu, "Details").getAttribute("aria-checked"), "true");
    assert.equal(await menuItem(menu, "Sort by name").getAttribute("aria-checked"), "true");
    assert.equal(await menuItem(menu, "Show hidden items").getAttribute("aria-checked"), "false");
    assert.equal(await menuItem(menu, "Show file extensions").getAttribute("aria-checked"), "true");
    await shotBoth("menu");
    // The button closes the menu it opened, and Escape closes it too.
    await page.locator(".view-button").click();
    await menu.waitFor({ state: "detached" });
    await openViewMenu();
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });
  });

  for (const { mode, label } of VIEWS) {
    await step(`${label} shows the folder${THUMBNAIL_VIEWS.has(mode) ? ", with thumbnails" : ""}`, async () => {
      await choose(label);
      await viewIs(mode);
      // The first items; the largest views render only the rows in sight.
      await item("Subfolder").waitFor();
      await item("aurora.png").waitFor();
      if (THUMBNAIL_VIEWS.has(mode)) await thumbnailsLoaded(IMAGES.map(([name]) => name));
      // Clicking still selects, in every view.
      await item("beach.png").click();
      assert.match(await status(), /1 selected/);
      await shotBoth(`view-${mode}`);
    });
  }

  await step("thumbnails are the images themselves, cached on disk", async () => {
    await choose("Large icons");
    await thumbnailsLoaded(IMAGES.map(([name]) => name));
    const shapes = await page.evaluate(() =>
      [...document.querySelectorAll(".tab-panel.active .view-item")].map((el) => {
        const img = el.querySelector("img.view-item-thumb.shown");
        return [el.querySelector(".label")?.textContent, img ? img.naturalWidth / img.naturalHeight : null];
      }),
    );
    const shape = Object.fromEntries(shapes);
    for (const [name, width, height] of IMAGES) {
      // An icon would be square; a thumbnail keeps the picture's shape.
      assert(Math.abs(shape[name] - width / height) < 0.05, `${name}: ${shape[name]} vs ${width / height}`);
    }
    const cache = join(dataDir, "thumbs");
    assert(existsSync(cache) && readdirSync(cache).length > 0, `thumbnails are cached in ${cache}`);
  });

  await step("Ctrl+Shift+digit switches views as in Explorer, and Ctrl+wheel steps through sizes", async () => {
    await list.focus();
    for (const { mode, digit } of [...VIEWS].reverse()) {
      await page.keyboard.press(`Control+Shift+Digit${digit}`);
      await viewIs(mode);
    }
    // Now in extra large icons. Wheel down makes items smaller, wheel up larger.
    const box = await list.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.keyboard.down("Control");
    await page.mouse.wheel(0, 120);
    await viewIs("large");
    await page.mouse.wheel(0, 120);
    await viewIs("medium");
    await page.mouse.wheel(0, -120);
    await viewIs("large");
    await page.keyboard.up("Control");
  });

  await step("arrow keys move by cell and by row in an icon grid", async () => {
    await page.keyboard.press("Control+Shift+Digit2");
    await viewIs("large");
    await item("Subfolder").click();
    assert.equal(await focusedIndex(), 0);
    const perRow = await page.evaluate(() => {
      const tops = [...document.querySelectorAll(".tab-panel.active .view-item")].map((el) => el.getBoundingClientRect().top);
      return tops.filter((top) => Math.abs(top - tops[0]) < 1).length;
    });
    assert(perRow > 1 && perRow < VISIBLE, `${perRow} items per row`);
    await page.keyboard.press("ArrowRight");
    assert.equal(await focusedIndex(), 1);
    await page.keyboard.press("ArrowDown");
    assert.equal(await focusedIndex(), 1 + perRow);
    await page.keyboard.press("ArrowLeft");
    assert.equal(await focusedIndex(), perRow);
    await page.keyboard.press("ArrowUp");
    assert.equal(await focusedIndex(), 0);
    await page.keyboard.press("Shift+ArrowRight");
    await page.keyboard.press("Shift+ArrowRight");
    assert.match(await status(), /3 selected/);
    await page.keyboard.press("End");
    assert.equal(await focusedIndex(), VISIBLE - 1);
    await page.keyboard.press("ArrowRight");
    assert.equal(await focusedIndex(), VISIBLE - 1, "Right stops at the last item");
    // Down into a last row too short to have an item below lands on its last item.
    const lastRowStart = Math.floor((VISIBLE - 1) / perRow) * perRow;
    if (VISIBLE - lastRowStart < perRow && lastRowStart > 0) {
      await panel.locator(`.view-item[data-index="${lastRowStart - 1}"]`).click();
      await page.keyboard.press("ArrowDown");
      assert.equal(await focusedIndex(), VISIBLE - 1);
    }
    await page.keyboard.press("Home");
    assert.equal(await focusedIndex(), 0);
    await page.keyboard.press("Control+a");
    assert.match(await status(), new RegExp(`${VISIBLE} selected`));
    await page.keyboard.type("note", { delay: 20 });
    await panel.locator(".view-item.focused .label", { hasText: /^notes/ }).waitFor();
  });

  await step("in the list view, columns run across: Down steps an item, Right a column", async () => {
    await page.keyboard.press("Control+Shift+Digit5");
    await viewIs("list");
    await item("Subfolder").click();
    const perColumn = await page.evaluate(() => {
      const lefts = [...document.querySelectorAll(".tab-panel.active .view-item")].map((el) => el.getBoundingClientRect().left);
      return lefts.filter((left) => Math.abs(left - lefts[0]) < 1).length;
    });
    await page.keyboard.press("ArrowDown");
    assert.equal(await focusedIndex(), 1);
    if (perColumn < VISIBLE) {
      await page.keyboard.press("ArrowRight");
      assert.equal(await focusedIndex(), 1 + perColumn);
    }
  });

  await step("rename in place, the context menu and live refresh work in an icon view", async () => {
    await page.keyboard.press("Control+Shift+Digit3");
    await viewIs("medium");
    await item("notes-01.txt").click();
    await page.keyboard.press("F2");
    const input = panel.locator("input.rename");
    await input.waitFor();
    assert.equal(await input.inputValue(), "notes-01.txt");
    await page.keyboard.type("renamed");
    await page.keyboard.press("Enter");
    await item("renamed.txt").waitFor();
    assert(existsSync(join(sandbox, "renamed.txt")) && !existsSync(join(sandbox, "notes-01.txt")));

    await item("forest.png").click({ button: "right" });
    const menu = page.locator(".menu");
    await menuItem(menu, "Open").waitFor();
    await menuItem(menu, "Rename").waitFor();
    await shot("context-menu-in-icons");
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });

    // Another program adds a file; the view refreshes itself and keeps the selection.
    await item("canyon.png").click();
    writeFileSync(join(sandbox, "added-later.txt"), "new");
    await item("added-later.txt").waitFor({ timeout: 5000 });
    assert.deepEqual(await panel.locator(".view-item.selected .label").allTextContents(), ["canyon.png"]);
  });

  await step("Show hidden items lists hidden files, dimmed, and leaves them out again", async () => {
    await choose("Show hidden items");
    await item(HIDDEN).waitFor();
    assert(await item(HIDDEN).evaluate((el) => el.classList.contains("dimmed")), "hidden files are dimmed");
    await shotBoth("hidden-shown");
    assert(await isChecked("Show hidden items"));
    await choose("Show hidden items");
    await item(HIDDEN).waitFor({ state: "detached" });
  });

  await step("Show file extensions off hides extensions, but rename edits the full name", async () => {
    await choose("Show file extensions");
    await item("notes").waitFor();
    await item("aurora").waitFor();
    await item("Subfolder").waitFor();
    assert(!(await labels()).some((name) => name.endsWith(".png")));
    await shotBoth("extensions-hidden");
    await item("notes").click();
    await page.keyboard.press("F2");
    const input = panel.locator("input.rename");
    await input.waitFor();
    assert.equal(await input.inputValue(), "notes.txt");
    await page.keyboard.press("Escape");
    await input.waitFor({ state: "detached" });
    assert(existsSync(join(sandbox, "notes.txt")));
    // Options are kept across restarts.
    await page.reload();
    await panel.locator(".view-item, .row").first().waitFor();
    assert.equal(await isChecked("Show file extensions"), false);
    await choose("Show file extensions");
    assert.equal(await isChecked("Show file extensions"), true);
  });

  await step("each folder keeps its own view and sort after leaving and returning", async () => {
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await item("notes.txt").waitFor();
    await choose("Tiles");
    await viewIs("tiles");
    await choose("Sort by size");
    // Folders first, then the largest file.
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .view-item .label")[1]?.textContent === "notes.txt");

    await item("Subfolder").dblclick();
    await page.locator(".crumb", { hasText: "Subfolder" }).waitFor();
    await item("inside.txt").waitFor();
    await viewIs("details");
    assert(await isChecked("Sort by name"), "a folder never sorted is sorted by name");
    await page.keyboard.press("Control+Shift+Digit5");
    await viewIs("list");

    await page.keyboard.press("Alt+ArrowUp");
    await item("aurora.png").waitFor();
    await viewIs("tiles");
    assert(await isChecked("Sort by size"));
    await item("Subfolder").dblclick();
    await item("inside.txt").waitFor();
    await viewIs("list");
    // And across restarts.
    await page.reload();
    await panel.locator(".view-item, .row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await item("aurora.png").waitFor();
    await viewIs("tiles");
  });

  await step("a search for \"e\" in an icon view scrolls to its last item", async () => {
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    const scope = page.locator(".searchbox .scope");
    if ((await scope.getAttribute("aria-pressed")) !== "true") await scope.click();
    await page.keyboard.press("Control+f");
    await page.keyboard.type("e");
    await page.waitForFunction(() => /[\d,.\s]{7,} results/.test(document.querySelector(".statusbar")?.textContent ?? ""), null, {
      timeout: 60_000,
    });
    // Search results have a view of their own, separate from the folder's tiles.
    await viewIs("details");
    await page.keyboard.press("Control+Shift+Digit3");
    await viewIs("medium");
    await list.focus();
    await page.keyboard.press("End");
    const total = Number((await page.locator(".statusbar span").first().innerText()).replace(/\D/g, ""));
    const last = panel.locator(`.view-item[data-index="${total - 1}"]`);
    await last.locator(".label").waitFor();
    assert.equal(await focusedIndex(), total - 1);
    const [itemBox, viewBox] = await Promise.all([last.boundingBox(), list.boundingBox()]);
    assert(itemBox.y >= viewBox.y - 1 && itemBox.y + itemBox.height <= viewBox.y + viewBox.height + 1, "the last item is in view");
    await page.waitForTimeout(500);
    await shotBoth("search-end-of-millions");
    // The list view scrolls sideways, through as many columns as millions of results need.
    await page.keyboard.press("Control+Shift+Digit5");
    await viewIs("list");
    await page.keyboard.press("Home");
    await panel.locator('.view-item[data-index="0"] .label').waitFor();
    await page.keyboard.press("End");
    await last.locator(".label").waitFor();
    assert.equal(await focusedIndex(), total - 1);
    const [listItemBox, listViewBox] = await Promise.all([last.boundingBox(), list.boundingBox()]);
    assert(
      listItemBox.x >= listViewBox.x - 1 && listItemBox.x + listItemBox.width <= listViewBox.x + listViewBox.width + 1,
      "the last item is in view",
    );
    await shot("search-end-of-millions-list");
    // Back at the top, then out of the search to the folder, which is still in tiles.
    await page.keyboard.press("Home");
    await panel.locator('.view-item[data-index="0"] .label').waitFor();
    await page.locator(".searchbox input").press("Escape");
    await viewIs("tiles");
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
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
