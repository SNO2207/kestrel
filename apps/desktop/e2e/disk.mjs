// End-to-end test of the disk usage hub, against a scratch folder of known composition.
//
// The suite makes a folder under %TEMP% named kestrel-test-e2e-disk-*, with sixteen files of
// known sizes, types and modification times, opens the hub on it with Ctrl+Shift+U, and checks
// the totals, the breakdowns by type and by age, the largest files, the treemap (areas, the
// tooltip, zooming in by a click and out with Back and the breadcrumb), and sending one file to
// the Recycle Bin from the treemap's menu, with confirmation, then undoing that. The file passes
// through the Recycle Bin and comes back; the suite checks the Recycle Bin ends as it began. It
// also opens the hub on the whole drive from the sidebar and from the palette.
//
// The treemap is a canvas, so the suite finds its tiles through the development hook a dev build
// exposes (`window.__kestrelDiskMap`); it needs a dev build.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/disk.mjs`. Screenshots go to e2e/out/disk, dark and light.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync, mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { png } from "./fixtures.mjs";
import { webviews } from "./windows.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "disk");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

const KB = 1024;
const GB = 1024 ** 3;
const DAY = 86_400_000;
const sandboxName = `kestrel-test-e2e-disk-${process.pid}`;
const sandbox = join(tmpdir(), sandboxName);
const RECYCLED = "kestrel-test-recycle.bin";

/**
 * Path, size, days since it was last changed, and type. Every size is a whole number of 4 KB
 * clusters and differs from every other, so sizes on disk are known and the order of the
 * largest files is too.
 */
const FILES = [
  ["media\\movie.mp4", 6144 * KB, 400, "video"],
  ["images\\installer.iso", 3072 * KB, 60, "disk_images"],
  ["archive\\backup.zip", 2048 * KB, 900, "archives"],
  [RECYCLED, 1792 * KB, 20, "other"],
  ["app\\tool.exe", 1536 * KB, 100, "programs"],
  ["media\\song.mp3", 1024 * KB, 40, "audio"],
  ["app\\helper.dll", 768 * KB, 100, "programs"],
  ["misc\\data.bin", 640 * KB, 700, "other"],
  ["photos\\beach.jpg", 512 * KB, 3, "images"],
  ["docs\\report.pdf", 384 * KB, 10, "documents"],
  ["photos\\map.png", 256 * KB, 200, "images"],
  ["docs\\sheet.xlsx", 128 * KB, 2000, "documents"],
  ["web\\node_modules\\dep\\index.js", 64 * KB, 5, "build_output"],
  ["code\\app.ts", 12 * KB, 1, "code"],
  ["code\\main.rs", 8 * KB, 1, "code"],
  ["web\\package.json", 4 * KB, 5, "code"],
];
/** media, images, archive, app, misc, photos, docs, web, web\node_modules, its dep, code. */
const FOLDERS = 11;
/** The upper bound of each age group, in days, as the backend groups them. */
const AGE_BOUNDS = [7, 30, 182, 365, 730, 1826, Infinity];

const total = FILES.reduce((sum, [, size]) => sum + size, 0);
const byType = {};
for (const [, size, , type] of FILES) byType[type] = (byType[type] ?? 0) + size;
const byAge = AGE_BOUNDS.map(() => 0);
for (const [, size, days] of FILES) byAge[AGE_BOUNDS.findIndex((bound) => days < bound)] += size;

function makeFixture() {
  rmSync(sandbox, { recursive: true, force: true });
  const now = Date.now();
  for (const [path, size, days] of FILES) {
    const file = join(sandbox, ...path.split("\\"));
    mkdirSync(dirname(file), { recursive: true });
    // Random bytes: nothing about the contents could make the file smaller on disk. The map is
    // a real picture, padded after its end, for the preview pane.
    const picture = path.endsWith(".png") ? png(64, 48, (x, y) => [x * 4, y * 5, 160]) : Buffer.alloc(0);
    writeFileSync(file, Buffer.concat([picture, randomBytes(size - picture.length)]));
    const when = new Date(now - days * DAY - 3_600_000);
    utimesSync(file, when, when);
  }
}

makeFixture();

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
const STORAGE_KEYS = ["kestrel.disk", "preview-open"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate(() => localStorage.removeItem("kestrel.disk"));
await page.reload();

const hub = page.locator(".disk-hub");
const panel = page.locator(".tab-panel.active");
const bigNumber = hub.locator(".disk-totals .disk-big-number");
const currentCrumb = hub.locator(".disk-crumb[aria-current]");
const step = (await import("./steps.mjs")).stepper("disk", () => page, { timed: true });

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

/** Waits until `read` returns `want`, polling; the index takes a moment to follow the disk. */
async function until(read, want, what, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    last = await read();
    if (JSON.stringify(last) === JSON.stringify(want)) return;
    if (Date.now() > deadline) throw new Error(`${what}: expected ${JSON.stringify(want)}, last saw ${JSON.stringify(last)}`);
    await page.waitForTimeout(200);
  }
}

const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const numberOf = async (locator, attribute = "data-bytes") => Number(await locator.getAttribute(attribute));
const tiles = () => page.evaluate(() => window.__kestrelDiskMap?.tiles() ?? []);
const mapPath = () => page.evaluate(() => window.__kestrelDiskMap?.path() ?? null);
const tile = async (name) => (await tiles()).find((each) => each.name === name);
const centre = (t) => ({ x: t.x + t.w / 2, y: t.y + t.h / 2 });

/** Waits for the preview's own webview to be as `wanted` says, asking Windows (windows.mjs). */
async function previewView(wanted, what, timeout = 10_000) {
  const deadline = Date.now() + timeout;
  let seen;
  do {
    seen = webviews()[1] ?? null;
    if (seen && wanted(seen)) return seen;
    await page.waitForTimeout(100);
  } while (Date.now() < deadline);
  throw new Error(`the preview's webview is not ${what}: ${JSON.stringify(seen)}`);
}

/** Items in the Recycle Bin that were deleted from somewhere inside the scratch folder. */
function recycledFromSandbox() {
  // The pattern is copied out of $args first: inside Where-Object, $args is that block's own.
  const script =
    "$pattern = $args[0]; $bin = (New-Object -ComObject Shell.Application).NameSpace(10); " +
    "@($bin.Items() | Where-Object { [string]$_.ExtendedProperty('System.Recycle.DeletedFrom') -like $pattern }).Count";
  const output = execFileSync("powershell", ["-NoProfile", "-Command", `& { ${script} } '${sandbox.replace(/'/g, "''")}*'`], {
    encoding: "utf8",
  });
  return Number(output.trim());
}

async function openFolder(path) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await page.waitForFunction((path) => document.querySelector(".address .crumb:last-of-type")?.textContent === path.split("\\").pop(), path);
}

const recycledBefore = recycledFromSandbox();

try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });

  await step("Ctrl+Shift+U opens the hub on the folder on screen", async () => {
    await openFolder(sandbox);
    await page.keyboard.press("Control+Shift+U");
    await hub.waitFor();
    assert.equal(await currentCrumb.innerText(), sandboxName);
    assert.equal(await page.locator(".disk-entry.current").count(), 1, "the sidebar entry shows the hub is open");
    assert.equal(await page.locator(".sidebar .place.current").count(), 0, "no folder is current while the hub covers the list");
    assert.equal(await hub.locator(".disk-tab[aria-selected=true]").innerText(), "Overview");
  });

  await step("the totals add up to the fixture's files", async () => {
    // The index follows the disk through the change journal; the hub follows the index.
    await until(() => numberOf(bigNumber), total, "size on disk", 60_000);
    const facts = hub.locator(".disk-facts");
    assert.equal(await numberOf(facts.locator("dd").nth(0)), total, "logical size: whole clusters, so the same");
    await until(() => numberOf(facts.locator("dd").nth(1), "data-count"), FILES.length, "files");
    assert.equal(await numberOf(facts.locator("dd").nth(2), "data-count"), FOLDERS, "folders");
    assert.equal(await hub.locator(".disk-cloud").count(), 0, "nothing in the cloud");
    // A folder's overview sets it beside its drive.
    assert.match(await hub.locator(".disk-drive-numbers").innerText(), /used of .* free$/);
    assert.match(await hub.locator(".disk-legend").innerText(), /This folder/);
  });

  await step("the breakdown by type puts each file where it belongs", async () => {
    const rows = await hub.locator(".disk-types tbody tr").evaluateAll((trs) =>
      trs.map((tr) => [tr.dataset.category, Number(tr.dataset.bytes), Number(tr.dataset.files)]),
    );
    const want = Object.entries(byType)
      .map(([type, bytes]) => [type, bytes, FILES.filter((file) => file[3] === type).length])
      .sort((a, b) => b[1] - a[1]);
    assert.deepEqual(rows, want, "largest type first, each with its bytes and files");
    // node_modules is build output by its location, though index.js is code by its name.
    assert.equal(await hub.locator(".disk-types tr[data-category=build_output] .disk-type-label").innerText(), "Developer build output");
    assert.equal(await hub.locator(".disk-stack-part").count(), rows.length);
  });

  await step("the breakdown by age groups files by when they last changed", async () => {
    await until(
      () => hub.locator(".disk-bar-row").evaluateAll((rows) => rows.map((row) => Number(row.dataset.bytes))),
      byAge,
      "bytes by age",
    );
    const labels = await hub.locator(".disk-bar-label").allInnerTexts();
    assert.deepEqual(labels, ["Last 7 days", "7 to 30 days", "1 to 6 months", "6 to 12 months", "1 to 2 years", "2 to 5 years", "Over 5 years"]);
  });

  await step("the largest files are listed largest first, and the largest folders", async () => {
    const files = await hub.locator('.disk-list-card[aria-label="Largest files"] .disk-item').evaluateAll((items) =>
      items.map((item) => [item.dataset.path, Number(item.dataset.bytes)]),
    );
    assert.deepEqual(
      files,
      FILES.map(([path, size]) => [join(sandbox, ...path.split("\\")), size]),
      "every file, in order of size",
    );
    const folders = await hub.locator('.disk-list-card[aria-label="Largest folders"] .disk-item').evaluateAll((items) =>
      items.map((item) => item.dataset.path),
    );
    assert.equal(folders[0], join(sandbox, "media"), "media holds the most");
    // web and web\node_modules only pass their one subfolder on; dep stands for all three.
    assert(folders.includes(join(sandbox, "web", "node_modules", "dep")) && !folders.includes(join(sandbox, "web")), folders.join(", "));
    // Until a day of samples exists, what changed lately stands in for what grew.
    const week = hub.locator('.disk-tile[data-days="7"]');
    assert.equal(await numberOf(week), byAge[0]);
    await shotBoth("1-overview");
    await hub.locator(".disk-scroll").evaluate((el) => (el.scrollTop = el.scrollHeight));
    await shotBoth("2-overview-end");
  });

  await step("the treemap gives each item an area in proportion to its size", async () => {
    await hub.locator(".disk-tab[data-tab=map]").click();
    await until(async () => (await tiles()).filter((t) => t.kind === "file").length, FILES.length, "file tiles");
    assert.equal(await mapPath(), sandbox);
    const all = await tiles();
    const frame = await hub.locator(".disk-map-frame").boundingBox();
    const area = (t) => t.w * t.h;
    const topLevel = all.filter((t) => t.parent === -1);
    assert.equal(topLevel.length, 10, "the root's nine folders and one file");
    const summed = topLevel.reduce((sum, t) => sum + area(t), 0);
    assert(Math.abs(summed - frame.width * frame.height) < frame.width + frame.height, `tiles cover the map: ${summed}`);
    for (const t of topLevel) {
      const share = area(t) / summed;
      assert(Math.abs(share - t.bytes / total) < 0.005, `${t.name}: area ${share.toFixed(4)} for ${(t.bytes / total).toFixed(4)} of the bytes`);
    }
    // Every tile lies inside the folder it belongs to, and no two in a folder overlap.
    for (const t of all.filter((t) => t.parent >= 0)) assert(inside(t, all[t.parent]), `${t.name} lies inside its folder`);
    for (const a of all) {
      for (const b of all) {
        if (a !== b && a.parent === b.parent) assert(overlap(a, b) === 0, `${a.name} and ${b.name} overlap: ${JSON.stringify([a, b])}`);
      }
    }
    const movie = await tile("movie.mp4");
    assert.equal(movie.category, 0, "video");
    await shotBoth("3-map");
  });

  await step("hovering a tile shows what it is", async () => {
    const movie = await tile("movie.mp4");
    const { x, y } = centre(movie);
    await page.mouse.move(x, y);
    const tooltip = hub.locator(".disk-tooltip");
    await tooltip.waitFor();
    assert.equal(await tooltip.locator(".disk-tooltip-name").innerText(), "movie.mp4");
    assert.equal(await tooltip.locator(".disk-tooltip-path").innerText(), join(sandbox, "media", "movie.mp4"));
    assert.equal(await tooltip.locator(".disk-tooltip-size").innerText(), "6.0 MB on disk");
    assert.equal(await tooltip.locator(".disk-tooltip-kind").innerText(), "Video");
    await shotBoth("4-tooltip", () => page.mouse.move(x + 1, y + 1));
  });

  await step("a click looks inside the folder, Back and the breadcrumb step out again", async () => {
    const back = hub.locator('.disk-icon-button[aria-label="Back"]');
    assert(await back.isDisabled(), "nothing to go back to yet");
    const movie = await tile("movie.mp4");
    await page.mouse.click(centre(movie).x, centre(movie).y);
    await until(mapPath, join(sandbox, "media"), "zoomed into media");
    assert.equal(await currentCrumb.innerText(), "media");
    await until(async () => (await tiles()).map((t) => t.name).sort(), ["movie.mp4", "song.mp3"], "media's tiles");
    await shotBoth("5-zoomed");
    await back.click();
    await until(mapPath, sandbox, "back out");
    assert(await back.isDisabled());

    // Into docs by a click on it, out by the breadcrumb.
    const docs = await tile("docs");
    await page.mouse.click(docs.x + 6, docs.y + 6);
    await until(mapPath, join(sandbox, "docs"), "zoomed into docs");
    await hub.locator(".disk-crumb", { hasText: sandboxName }).click();
    await until(mapPath, sandbox, "out by the breadcrumb");
    // Alt+Left is Back too.
    await page.mouse.click(docs.x + 6, docs.y + 6);
    await until(mapPath, join(sandbox, "docs"), "zoomed into docs again");
    await page.keyboard.press("Alt+ArrowLeft");
    await until(mapPath, sandbox, "out with Alt+Left");
  });

  await step("a file goes to the Recycle Bin from the treemap's menu, after confirmation", async () => {
    const victim = await tile(RECYCLED);
    await page.mouse.click(centre(victim).x, centre(victim).y, { button: "right" });
    const menu = page.locator(".menu");
    await menu.waitFor();
    assert.deepEqual(await menu.locator(".menu-label").allInnerTexts(), ["Open", "Show in folder", "Move to the Recycle Bin…"]);
    await menu.locator(".menu-item", { hasText: "Move to the Recycle Bin…" }).click();
    const dialog = page.locator(".disk-confirm");
    await dialog.waitFor();
    assert.match(await dialog.innerText(), new RegExp(`${RECYCLED}, 1\\.8 MB on disk`));
    await shotBoth("6-confirm");
    // Cancel first: nothing happens.
    await dialog.locator(".dialog-button", { hasText: "Cancel" }).click();
    await dialog.waitFor({ state: "detached" });
    assert(existsSync(join(sandbox, RECYCLED)));

    await page.mouse.click(centre(victim).x, centre(victim).y, { button: "right" });
    await menu.locator(".menu-item", { hasText: "Move to the Recycle Bin…" }).click();
    await dialog.locator(".dialog-button", { hasText: "Move to the Recycle Bin" }).click();
    await until(() => existsSync(join(sandbox, RECYCLED)), false, "the file left the folder");
    assert.equal(recycledFromSandbox(), recycledBefore + 1, "it is in the Recycle Bin, not erased");
    await hub.locator(".disk-status", { hasText: `Moved ${RECYCLED} to the Recycle Bin.` }).waitFor();
    // The hub hears of it from the index.
    await until(async () => !!(await tile(RECYCLED)), false, "its tile went");
    await hub.locator(".disk-tab[data-tab=overview]").click();
    await until(() => numberOf(bigNumber), total - 1792 * KB, "the total without it");
    await shotBoth("7-recycled");
  });

  await step("Undo brings it back", async () => {
    await hub.locator(".disk-status-action", { hasText: "Undo" }).click();
    await page.locator(".notice span", { hasText: `Undid delete of ${RECYCLED}` }).waitFor({ timeout: 15_000 });
    await until(() => existsSync(join(sandbox, RECYCLED)), true, "the file is back");
    await until(() => numberOf(bigNumber), total, "the total with it again");
    assert.equal(recycledFromSandbox(), recycledBefore, "the Recycle Bin ends as it began");
  });

  await step("what grew compares the index with an earlier sample", async () => {
    await hub.focus();
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });
    // Yesterday's sample, made up from today's with one folder deep under the profiles (and
    // its parents) a gigabyte smaller; the development hook keeps it in the slot's database.
    const current = await invoke("disk_history_probe", { action: "current", daysAgo: 1, rows: null });
    const folders = current.filter((row) => !row.file);
    const key = (path) => path.replace(/\\$/, "").toLowerCase();
    const under = (path, folder) => key(path).startsWith(`${key(folder)}\\`);
    const users = folders.find((row) => key(row.path) === "c:\\users");
    const target = folders.find(
      (row) => under(row.path, "C:\\Users") && row.bytes > 2 * GB && key(row.path).split("\\").length >= 4 && !folders.some((other) => under(other.path, row.path)),
    );
    if (!users || !target) {
      console.log("--  no folder of 2 GB four levels under C:\\Users to make history with; skipped");
      return;
    }
    const baseline = current.map((row) => (row === target || (!row.file && under(target.path, row.path)) ? { ...row, bytes: row.bytes - GB } : row));
    await invoke("disk_history_probe", { action: "save", daysAgo: 1, rows: baseline });
    try {
      await openFolder("C:\\Users");
      await page.keyboard.press("Control+Shift+U");
      await hub.waitFor();
      // C:\Users is a frequent folder by now, but no place is current while the hub covers it.
      assert.deepEqual(await page.locator(".sidebar .current").evaluateAll((items) => items.map((item) => item.className)), [
        "disk-entry current",
      ]);
      const growth = hub.locator(".disk-growth");
      await growth.locator(".disk-growth-summary").waitFor({ timeout: 30_000 });
      assert.equal(await growth.locator('.segmented button[aria-pressed="true"]').innerText(), "Last day");
      assert.match(await growth.locator(".disk-growth-summary").innerText(), /^Since .*: up /);
      const listed = await growth.locator(".disk-item").evaluateAll((items) => items.map((item) => [item.dataset.path, item.innerText]));
      const row = listed.find(([path]) => key(path) === key(target.path));
      assert(row, `${target.path} is among what grew: ${listed.map(([path]) => path).join(", ")}`);
      // Its parents grew by what it did, so it stands for them.
      assert(!listed.some(([path]) => key(path) !== key(target.path) && under(target.path, path)), "its parents are left to it");
      await hub.locator(".disk-scroll").evaluate((el) => (el.scrollTop = el.scrollHeight));
      await shotBoth("8-what-grew");
    } finally {
      await invoke("disk_history_probe", { action: "forget", daysAgo: 1, rows: null });
    }
  });

  await step("Escape closes the hub, the sidebar opens it on the whole drive", async () => {
    await hub.focus();
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });
    await page.locator(".disk-entry").click();
    await hub.waitFor();
    await until(async () => (await currentCrumb.innerText()).endsWith("(C:)"), true, "the drive");
    await hub.locator(".disk-explain").waitFor({ timeout: 60_000 });
    const drive = await hub.locator(".disk-drive-numbers").evaluate((el) => [Number(el.dataset.used), Number(el.dataset.free)]);
    assert(drive[0] > 0 && drive[1] > 0, `used and free: ${drive}`);
    assert.match(await hub.locator(".disk-legend").innerText(), /In files the index holds/);
    await shotBoth("9-drive");
    await page.locator(".disk-entry").click();
    await hub.waitFor({ state: "detached" });
  });

  await step("the palette and the folder's menu open it too", async () => {
    await openFolder(sandbox);
    await page.keyboard.press("Control+k");
    const palette = page.locator(".palette");
    await palette.locator("input").fill(">disk usage");
    await palette.locator('.palette-list[aria-busy="false"]').waitFor();
    const titles = await palette.locator(".palette-title").allTextContents();
    assert(titles.includes("Show disk usage of this folder") && titles.includes("Show disk usage of this drive"), titles.join(" | "));
    await palette.locator(".palette-item", { hasText: "Show disk usage of this folder" }).click();
    await hub.waitFor();
    await until(() => currentCrumb.innerText(), sandboxName, "the folder on screen");
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });

    const list = panel.locator(".list-viewport");
    const box = await list.boundingBox();
    await page.mouse.click(box.x + 60, box.y + box.height - 30, { button: "right" });
    await page.locator(".menu .menu-item", { hasText: "Show disk usage" }).click();
    await hub.waitFor();
    await until(() => currentCrumb.innerText(), sandboxName, "the folder from its menu");
    await page.keyboard.press("Control+Shift+U");
    await hub.waitFor({ state: "detached" });
  });

  await step("the preview's webview steps aside while the hub covers it", async () => {
    await openFolder(join(sandbox, "photos"));
    await panel.locator(".list-viewport").focus();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor();
    await panel.locator(".row").filter({ has: page.locator(".label", { hasText: /^map\.png$/ }) }).click();
    await page.locator(".preview-pane .preview-surface[data-kind='image']").waitFor();
    await previewView((view) => view.visible, "shown over the pane");
    await page.keyboard.press("Control+Shift+U");
    await hub.waitFor();
    await previewView((view) => !view.visible, "hidden under the hub");
    await page.keyboard.press("Control+Shift+U");
    await hub.waitFor({ state: "detached" });
    await previewView((view) => view.visible, "shown again once the hub closes");
    await panel.locator(".list-viewport").focus();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor({ state: "detached" });
  });

  assert.deepEqual(errors, [], "no errors in the page");
  console.log("\nall checks passed");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  if (recycledFromSandbox() > recycledBefore) {
    console.error(`!!  ${RECYCLED} from ${sandbox} is still in the Recycle Bin; restore it from there`);
  }
  rmSync(sandbox, { recursive: true, force: true });
}

/** Whether tile `a` lies within tile `b`. */
function inside(a, b) {
  return a.x >= b.x - 0.01 && a.y >= b.y - 0.01 && a.x + a.w <= b.x + b.w + 0.01 && a.y + a.h <= b.y + b.h + 0.01;
}

/**
 * The area two tiles share. Positions arrive rounded to a hundredth of a pixel (`disk_treemap`),
 * so tiles that meet can seem to share a sliver that thin; such a sliver is no overlap.
 */
function overlap(a, b) {
  const ROUNDING = 0.02;
  const w = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
  return w > ROUNDING && h > ROUNDING ? w * h : 0;
}
