// End-to-end test of the media library: the background pass, the media clauses of a search, the
// details view's media columns, the details pane's recognised text, the Gallery (by month, its
// filters, Quick Look, the menu, the keyboard, the timeline, tens of thousands of items), pausing
// and resuming, and the library's settings page.
//
// Everything happens in a scratch folder under %TEMP% named kestrel-test-e2e-media-*, which a
// development hook makes the media library's only folder (test slots start with none:
// KESTREL_MEDIA_SCOPES=none in dev.mjs). Its files are written by another development hook,
// since photos with EXIF, a video and a picture of text can't be made from here: two photos from
// July 2024 (one with a GPS position, one turned upright by its orientation), a receipt last
// changed in March 2025, and a three-second video. Nothing is deleted through the app, and the
// clipboard is never used: Copy in the details pane goes to a development hook.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/media.mjs`. Screenshots go to e2e/out/media, dark and light.

import assert from "node:assert/strict";
import { copyFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "media");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-test-e2e-media-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(sandbox, { recursive: true });

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
const STORAGE_KEYS = ["kestrel.views", "preview-open", "details-open", "search-scope"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
// Searches look in the folder on screen, the scratch folder, whatever else the library holds.
await page.evaluate(() => localStorage.setItem("search-scope", "folder"));
await page.reload();

const panel = page.locator(".tab-panel.active");
const gallery = panel.locator(".gallery");
const tiles = gallery.locator(".gallery-tile[data-path]");
const menu = page.locator(".menu");
const exactly = (text) => new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`);
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const menuItem = (label) => menu.locator(".menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) });
const names = async () => (await panel.locator(".row .label").allTextContents()).sort();
const step = (await import("./steps.mjs")).stepper("media", () => page, { timed: true });

/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}) => page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);

/** Captured through the debugging protocol, in both themes. */
async function shotBoth(name, prepare = async () => {}) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(250);
    await prepare();
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Retries `check` until it stops throwing. */
async function eventually(check, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      return await check();
    } catch (error) {
      if (Date.now() > deadline) throw error;
      await page.waitForTimeout(200);
    }
  }
}

/** Opens a folder or a location through the address bar. */
async function open(address) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(address);
  await page.keyboard.press("Enter");
}

/** Waits for the pass to have read `total` items with nothing left, asking for a plan each time. */
async function readAll(total) {
  return eventually(async () => {
    const progress = await invoke("media_dev_wait", { timeoutMs: 20_000 });
    assert.equal(progress.total, total, JSON.stringify(progress));
    assert.equal(progress.queued, 0, JSON.stringify(progress));
    return progress;
  }, 90_000);
}

async function search(query) {
  await page.locator(".searchbox input").fill(query);
}

/**
 * Clears the search and waits for the folder to be listed again: typing a new search before then
 * loses it, as the folder's listing, arriving after, takes the tab back to no search.
 */
async function clearSearch() {
  await page.locator(".searchbox input").press("Escape");
  await eventually(async () => {
    assert.equal(await page.locator(".searchbox input").inputValue(), "");
    assert(!/results?/.test(await page.locator(".statusbar").innerText()), "the folder is listed again");
  });
}

/** The month each Gallery tile is drawn under: the last heading above it. */
const tileMonths = () =>
  page.evaluate(() => {
    const months = [...document.querySelectorAll(".tab-panel.active .gallery-month")].map((m) => ({
      month: m.dataset.month,
      top: m.getBoundingClientRect().top,
    }));
    return [...document.querySelectorAll(".tab-panel.active .gallery-tile[data-path]")].map((tile) => {
      const top = tile.getBoundingClientRect().top;
      const above = months.filter((m) => m.top <= top).sort((a, b) => b.top - a.top)[0];
      return { name: tile.dataset.path.split("\\").pop(), month: above?.month };
    });
  });

const monthOf = (ms) => {
  const at = new Date(ms);
  return `${at.getFullYear()}-${String(at.getMonth() + 1).padStart(2, "0")}`;
};

const fixtures = {};
let columnsDoc;

try {
  await step("the index is ready", async () => {
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 300_000 });
  });

  await step("the fixtures are written and the library reads them in the background", async () => {
    const written = await invoke("media_dev_fixtures", { folder: sandbox });
    assert.equal(written.length, 4);
    for (const path of written) fixtures[basename(path)] = path;
    await invoke("media_dev_sandbox", { folder: sandbox });
    const progress = await readAll(4);
    assert.deepEqual([progress.photos, progress.videos, progress.done, progress.skipped.length], [3, 1, 4, 0], JSON.stringify(progress));
    const stages = Object.fromEntries(progress.stages.map((stage) => [stage.name, stage]));
    assert.equal(stages.metadata.done, 4);
    assert.equal(stages.thumbnail.done, 4);
    assert.equal(stages.text.done, 3, "text is read in the photos");
  });

  await step("searches find the fixtures by their facts and their words", async () => {
    await open(sandbox);
    await page.locator(".crumb").last().filter({ hasText: basename(sandbox) }).waitFor();
    await row("trip").waitFor();
    const cases = [
      ["width:>1000", ["beach.jpg", "receipt.png"]],
      ["height:1200", ["tower.jpg"]],
      ["camera:pixel", ["tower.jpg"]],
      ["taken:2024-07", ["beach.jpg", "tower.jpg"]],
      ["geo:yes", ["beach.jpg"]],
      ["kind:video", ["walk.mp4"]],
      ["duration:>2s", ["walk.mp4"]],
      ["kind:photo !geo:yes", ["receipt.png", "tower.jpg"]],
      ['text:"order 42.17"', ["receipt.png"]],
      ["text:costco", ["receipt.png"]],
    ];
    for (const [query, expected] of cases) {
      await search(query);
      await eventually(async () => assert.deepEqual(await names(), expected, query));
    }
    await shotBoth("1-text-search");
  });

  await step("the details pane shows the text in an image, and Copy copies it", async () => {
    await row("receipt.png").click();
    await page.keyboard.press("Alt+Shift+P");
    const pane = page.locator(".details-pane");
    await pane.locator(".details-name", { hasText: "receipt.png" }).waitFor();
    const text = pane.locator(".media-text-body");
    await eventually(async () => {
      const shown = await text.innerText();
      assert(/COSTCO/.test(shown) && /42\.17/.test(shown), shown);
    });
    await shotBoth("2-details-text");
    await page.evaluate(() => window.__kestrelMediaText.capture());
    await pane.locator(".media-text-copy").click();
    await pane.locator(".media-text-copy", { hasText: "Copied" }).waitFor();
    const copied = await page.evaluate(() => window.__kestrelMediaText.capture());
    assert.equal(copied.length, 1);
    assert.match(copied[0], /COSTCO[\s\S]*42\.17/);
    await page.evaluate(() => window.__kestrelMediaText.release());
    await page.keyboard.press("Alt+Shift+P");
    await pane.waitFor({ state: "detached" });
    await clearSearch();
  });

  await step("the details view shows Date taken, Dimensions and Duration", async () => {
    columnsDoc = new Map(await invoke("layout_load")).get("columns:search") ?? "";
    await search("kind:photo");
    await eventually(async () => assert.deepEqual(await names(), ["beach.jpg", "receipt.png", "tower.jpg"]));
    await search("width:>1 ");
    await eventually(async () => assert.equal((await names()).length, 4));
    for (const label of ["Date taken", "Dimensions", "Duration"]) {
      await panel.locator(".list-header").click({ button: "right" });
      await menuItem(label).click();
    }
    await eventually(async () => {
      assert.equal(await row("tower.jpg").locator(".cell.dimensions").innerText(), "900 × 1200");
      assert.match(await row("beach.jpg").locator(".cell.taken").innerText(), /2024/);
      assert.equal(await row("walk.mp4").locator(".cell.duration").innerText(), "0:03");
      assert.equal(await row("receipt.png").locator(".cell.taken").innerText(), "", "a screenshot has no date taken");
    });
    // The engine sorts by what the index holds; these columns don't sort, and say nothing changed.
    await panel.locator(".column-header.dimensions").click();
    await shotBoth("3-media-columns");
    await invoke("layout_save", { name: "columns:search", value: columnsDoc });
    await clearSearch();
  });

  await step("the Gallery shows the fixtures by month, newest first", async () => {
    const facts = await invoke("media_facts", { paths: [fixtures["walk.mp4"]] });
    // The video's own date is when it was written; a camera's would be when it was recorded.
    const videoMonth = monthOf(facts[0]?.taken ?? Date.parse("2023-12-24T12:00:00Z"));
    await page.locator(".gallery-entry").click();
    await gallery.waitFor();
    await eventually(async () => assert.equal(await tiles.count(), 4));
    const months = await gallery.locator(".gallery-month").evaluateAll((all) => all.map((m) => m.dataset.month));
    assert.deepEqual(months, [...new Set([videoMonth, "2025-03", "2024-07"])].sort().reverse());
    const placed = Object.fromEntries((await tileMonths()).map((tile) => [tile.name, tile.month]));
    assert.deepEqual(placed, { "walk.mp4": videoMonth, "receipt.png": "2025-03", "tower.jpg": "2024-07", "beach.jpg": "2024-07" });
    await gallery.locator(".gallery-tile[data-path$='walk.mp4'] .gallery-video-badge", { hasText: "0:03" }).waitFor();
    // Thumbnails come from the cache the pass filled.
    await eventually(async () => {
      const loaded = await gallery.locator(".gallery-tile img").evaluateAll((images) => images.filter((img) => img.complete && img.naturalWidth > 0).length);
      assert.equal(loaded, 4);
    });
    const tower = await gallery.locator(".gallery-tile[data-path$='tower.jpg']").boundingBox();
    assert(tower && tower.height > tower.width, "the tower photo stands upright");
    assert.match(await page.locator(".statusbar").innerText(), /4 items/);
    await shotBoth("4-gallery");
  });

  await step("the Gallery's filters narrow it to photos, videos or a folder", async () => {
    await gallery.locator('.gallery-kind[data-kind="video"]').click();
    await eventually(async () => assert.deepEqual(await tiles.evaluateAll((all) => all.map((t) => t.dataset.path.split("\\").pop())), ["walk.mp4"]));
    await gallery.locator('.gallery-kind[data-kind="photo"]').click();
    await eventually(async () => assert.equal(await tiles.count(), 3));
    await gallery.locator('.gallery-kind[data-kind="all"]').click();
    await eventually(async () => assert.equal(await tiles.count(), 4));
    await gallery.locator(".gallery-folder").click();
    await menuItem("trip (2)").click();
    await eventually(async () => assert.deepEqual((await tiles.evaluateAll((all) => all.map((t) => t.dataset.path.split("\\").pop()))).sort(), ["beach.jpg", "tower.jpg"]));
    await gallery.locator(".gallery-folder", { hasText: "trip" }).waitFor();
    await shotBoth("5-gallery-folder");
    await gallery.locator(".gallery-folder").click();
    await menuItem("All folders").click();
    await eventually(async () => assert.equal(await tiles.count(), 4));
  });

  await step("Quick Look, the keyboard and the menu work in the Gallery", async () => {
    await gallery.locator(".gallery-tile[data-path$='tower.jpg']").click();
    assert.match(await page.locator(".statusbar").innerText(), /1 selected/);
    await page.keyboard.press("Space");
    const look = page.locator(".quicklook");
    await look.waitFor();
    await look.getByText("tower.jpg").first().waitFor();
    // Newest first: the beach photo, taken the day before, comes after the tower.
    await page.keyboard.press("ArrowRight");
    await look.getByText("beach.jpg").first().waitFor();
    await page.keyboard.press("Escape");
    await look.waitFor({ state: "detached" });
    await gallery.locator(".gallery-tile.focused[data-path$='beach.jpg']").waitFor();
    await page.keyboard.press("Home");
    await gallery.locator(".gallery-tile.focused[data-path$='walk.mp4']").waitFor();
    await page.keyboard.press("End");
    await gallery.locator(".gallery-tile.focused[data-path$='beach.jpg']").waitFor();
    await page.keyboard.press("Shift+ArrowLeft");
    assert.match(await page.locator(".statusbar").innerText(), /2 selected/);
    await gallery.locator(".gallery-tile[data-path$='beach.jpg']").click();
    assert.match(await page.locator(".statusbar").innerText(), /1 selected/);
    await gallery.locator(".gallery-tile[data-path$='beach.jpg']").click({ button: "right" });
    await menu.waitFor();
    for (const label of ["Open", "Quick Look", "Open file location", "Copy", "Copy as path", "Delete", "Properties", "Show more options"]) {
      await menuItem(label).waitFor();
    }
    await shotBoth("6-gallery-menu");
    await menuItem("Open file location").click();
    await page.locator(".crumb").last().filter({ hasText: "trip" }).waitFor();
    await row("beach.jpg").and(panel.locator(".row.selected")).waitFor();
    await page.keyboard.press("Alt+ArrowLeft");
    await gallery.waitFor();
    await eventually(async () => assert.equal(await tiles.count(), 4));
  });

  await step("pausing stops the pass, and resuming finishes what came meanwhile", async () => {
    await gallery.locator(".gallery-settings").click();
    const settings = panel.locator(".media-library");
    await settings.waitFor();
    await settings.locator(".media-scopes li .media-scope-path", { hasText: exactly(sandbox) }).waitFor();
    for (const stage of ["metadata", "thumbnail", "text"]) await settings.locator(`.media-stages li[data-stage="${stage}"]`).waitFor();
    await shotBoth("7-settings");
    await settings.locator('.media-status-action[data-action="pause"]').click();
    await settings.locator('.media-status[data-state="paused"]').waitFor();
    assert.equal((await invoke("media_status")).progress.state, "paused");
    // A new photo while paused is found, but not read.
    copyFileSync(fixtures["receipt.png"], join(sandbox, "screens", "receipt again.png"));
    await eventually(async () => {
      const progress = await invoke("media_dev_wait", { timeoutMs: 5_000 });
      assert.deepEqual([progress.total, progress.queued, progress.state], [5, 1, "paused"], JSON.stringify(progress));
    });
    await page.waitForTimeout(1500);
    const still = (await invoke("media_status")).progress;
    assert.deepEqual([still.queued, still.done], [1, 4], "nothing is read while paused");
    await settings.locator(".media-status-text", { hasText: "Paused at 4 of 5" }).waitFor();
    await shotBoth("8-settings-paused");
    // The Gallery says so too, and resumes from there.
    await open("Gallery");
    await gallery.locator('.media-status[data-state="paused"]').waitFor();
    await gallery.locator(".gallery-tile[data-path$='tower.jpg']").click();
    await shotBoth("9-gallery-paused");
    await gallery.locator('.media-status-action[data-action="resume"]').click();
    await readAll(5);
    await gallery.locator('.media-status[data-state="idle"]').waitFor();
    await eventually(async () => assert.equal(await tiles.count(), 5), 15_000);
    // The Gallery reloaded with the new photo, and kept the selection.
    await gallery.locator(".gallery-tile.selected.focused[data-path$='tower.jpg']").waitFor();
    assert.equal(await gallery.locator(".gallery-tile.selected").count(), 1);
    await open(sandbox);
    await row("trip").waitFor();
    await search("text:costco");
    await eventually(async () => assert.deepEqual(await names(), ["receipt again.png", "receipt.png"]));
    await clearSearch();
  });

  await step("the Gallery lays out and scrolls fifty thousand items smoothly", async () => {
    await open("Gallery");
    await gallery.waitFor();
    const started = Date.now();
    await page.evaluate(() => window.__kestrelGallery.synthetic(50_000));
    await page.waitForFunction(() => document.querySelectorAll(".tab-panel.active .gallery-tile[data-path^='C:\\\\Synthetic']").length > 10);
    const shown = Date.now() - started;
    assert(shown < 3000, `laid out in ${shown} ms`);
    const timing = await page.evaluate(async () => {
      const scroller = document.querySelector(".tab-panel.active .gallery-scroller");
      const frames = [];
      let most = 0;
      let last = performance.now();
      for (let i = 0; i < 150; i++) {
        scroller.scrollTop += 1400;
        await new Promise((resolve) => requestAnimationFrame(resolve));
        const now = performance.now();
        frames.push(now - last);
        last = now;
        most = Math.max(most, document.querySelectorAll(".tab-panel.active .gallery-tile").length);
      }
      frames.sort((a, b) => a - b);
      return {
        mean: frames.reduce((a, b) => a + b, 0) / frames.length,
        p95: frames[Math.floor(frames.length * 0.95)],
        most,
        height: scroller.scrollHeight,
      };
    });
    console.log(`    laid out in ${shown} ms; ${JSON.stringify(timing)}`);
    assert(timing.most < 600, `only what is in view is drawn: ${timing.most} tiles`);
    assert(timing.mean < 25, `scrolling frames average ${timing.mean.toFixed(1)} ms`);
    // Ten years of items: the timeline names most of them.
    const years = await gallery.locator(".gallery-timeline-mark.year").allTextContents();
    assert(years.length >= 6 && years.every((year) => /^20\d\d$/.test(year)), `the timeline's years: ${years}`);
    // The timeline jumps to a month: the middle of the track lands in the middle of the years.
    const track = gallery.locator(".gallery-timeline");
    const box = await track.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await gallery.locator(".gallery-timeline-bubble").waitFor();
    await page.mouse.down();
    await page.mouse.up();
    const fraction = await page.evaluate(() => {
      const scroller = document.querySelector(".tab-panel.active .gallery-scroller");
      return scroller.scrollTop / (scroller.scrollHeight - scroller.clientHeight);
    });
    assert(fraction > 0.4 && fraction < 0.6, `the timeline took the view to ${fraction}`);
    await shotBoth("10-fifty-thousand", async () => {
      await page.mouse.move(box.x + box.width / 2, box.y + box.height * 0.3);
      await page.waitForTimeout(100);
    });
    await page.mouse.move(10, 10);
    await page.evaluate(() => window.__kestrelGallery.synthetic(null));
    await eventually(async () => assert.equal(await tiles.count(), 5));
  });

  assert.deepEqual(errors, [], "no errors in the page");
  console.log("\nall media checks passed; screenshots are in", out);
} finally {
  await page.evaluate(() => window.__kestrelGallery?.synthetic(null)).catch(() => {});
  await invoke("media_set_paused", { paused: false }).catch(() => {});
  await invoke("media_dev_sandbox", { folder: null }).catch(() => {});
  if (columnsDoc !== undefined) await invoke("layout_save", { name: "columns:search", value: columnsDoc }).catch(() => {});
  await page.evaluate((saved) => saved.forEach(([key, value]) => (value === null ? localStorage.removeItem(key) : localStorage.setItem(key, value))), savedStorage).catch(() => {});
  await page.reload().catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
