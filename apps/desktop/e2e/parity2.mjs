// End-to-end test of Explorer parity's second round: rows that never go blank while a folder is
// refreshed or re-sorted (sampled on every animation frame, and measured), compact view, item
// check boxes, Ctrl+Space and Ctrl+arrows, compressed files in colour, icons instead of
// thumbnails, the navigation pane hidden, Num Lock+plus and minus in the folder tree, tab
// commands (Duplicate tab, Close other tabs) and a tab moved out to a new window and back, the
// Libraries location (on libraries the suite writes itself), the Network location (on computers
// a development hook gives it) and the network's dialogs (recorded by a development hook, not
// shown), Folder Options' privacy switches and Clear history, and files dropped on a zip.
//
// File changes happen only in a scratch folder under the temp directory, removed afterwards; a
// zip of the suite's own passes through the Recycle Bin (the original, then the new one after an
// undo) and is deleted from the bin at the end, and the bin's other items are compared before
// and after. Clear history empties the slot's own database's history, nothing else: Windows'
// Recent items folder is only read, before and after, to show it is untouched. The window's view
// options are saved first and put back at the end. Needs a dev build.
//
// Checked by hand, not here: dragging a tab out of its strip with the real mouse (the suite lets
// it go through the same code with a development hook standing in for the pointer, which also
// skips the hit test of `WindowFromPoint`), Windows' Map Network Drive, Disconnect Network Drives
// and Add Network Location dialogs (recorded by a development hook, never shown), the Network
// location on a network whose browser answers, pasting into a zip (the clipboard is left alone;
// the backend's adding is the same as a drop's and is unit tested).
//
//   KESTREL_SLOT=<n> node e2e/dev.mjs start
//   KESTREL_SLOT=<n> node e2e/parity2.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel, waitForKestrelWindow } from "./connect.mjs";
import { png } from "./fixtures.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

// ---------- the scratch folder ----------

const sandbox = join(tmpdir(), `kestrel-e2e-parity2-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
const MANY = join(sandbox, "many");
const BIG = join(sandbox, "big");
const LIBS = join(sandbox, "libraries");
const SHELF_A = join(sandbox, "shelf-a");
const SHELF_B = join(sandbox, "shelf-b");
for (const folder of [MANY, BIG, LIBS, SHELF_A, SHELF_B, join(sandbox, "sub", "inner")]) mkdirSync(folder, { recursive: true });
writeFileSync(join(sandbox, "alpha.txt"), "alpha");
writeFileSync(join(sandbox, "beta.txt"), "beta beta");
writeFileSync(join(sandbox, "gamma.txt"), "gamma gamma gamma");
writeFileSync(join(sandbox, "squeezed.txt"), "squeeze me ".repeat(4000));
writeFileSync(join(sandbox, "picture.png"), png(96, 64, (x, y) => [x * 2, y * 3, 180]));
writeFileSync(join(sandbox, "dropped.txt"), "dropped on the zip");
writeFileSync(join(sandbox, "inside.txt"), "kept inside");
// Files of many sizes, so a sort by size moves them about.
for (let i = 0; i < 400; i++) writeFileSync(join(MANY, `item ${String(i).padStart(3, "0")}.txt`), "x".repeat((i * 7919) % 4096));
// A folder of ten thousand files, for timing a navigation.
for (let i = 0; i < 10_000; i++) writeFileSync(join(BIG, `file ${String(i).padStart(5, "0")}.dat`), "");
writeFileSync(join(SHELF_A, "from-a.txt"), "a");
writeFileSync(join(SHELF_B, "from-b.txt"), "bb");
mkdirSync(join(SHELF_B, "b-folder"));
// A library of two folders, as Windows writes one; the second is where new items go.
const library = (name, folders) =>
  `<?xml version="1.0" encoding="UTF-8"?>\r\n<libraryDescription xmlns="http://schemas.microsoft.com/windows/2009/library">\r\n  <name>${name}</name>\r\n  <version>6</version>\r\n  <isLibraryPinned>true</isLibraryPinned>\r\n  <iconReference>imageres.dll,-1002</iconReference>\r\n  <searchConnectorDescriptionList>\r\n${folders
    .map(
      ([path, save]) =>
        `    <searchConnectorDescription>\r\n      ${save ? "<isDefaultSaveLocation>true</isDefaultSaveLocation>\r\n      " : ""}<simpleLocation>\r\n        <url>${path}</url>\r\n      </simpleLocation>\r\n    </searchConnectorDescription>\r\n`,
    )
    .join("")}  </searchConnectorDescriptionList>\r\n</libraryDescription>`;
writeFileSync(join(LIBS, "Shelves.library-ms"), library("Shelves", [[SHELF_A, false], [SHELF_B, true]]));
// A zip to drop files on, made by Windows' own tar.
const ZIP_NAME = `kestrel-test-parity2-${process.pid}.zip`;
const ZIP = join(sandbox, ZIP_NAME);
execFileSync("C:\\Windows\\System32\\tar.exe", ["--format", "zip", "-cf", ZIP, "-C", sandbox, "inside.txt"]);
const zipEntries = () =>
  execFileSync("C:\\Windows\\System32\\tar.exe", ["-tf", ZIP], { encoding: "utf8" })
    .split(/\r?\n/)
    .filter(Boolean)
    .sort();

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.parity2", "kestrel.folder-options", "kestrel.view-options", "kestrel.views", "preview-open", "details-open", "search-scope"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
// Start from the defaults: details everywhere, no panes, Explorer's options.
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();
await page.waitForFunction(() => typeof window.__kestrelParity2 === "object" && typeof window.__kestrelDragDrop === "object");

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const menu = page.locator(".menu");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const item = (name) => panel.locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const menuItem = (label) => page.locator(".menu .menu-item").filter({ has: page.locator(".menu-label", { hasText: exactly(label) }) });
const comparable = (path) => path.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};
/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}, target = page) =>
  target.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const hook = (method, ...args) => page.evaluate(([method, args]) => window.__kestrelParity2[method](...args), [method, args]);
const setOptions = (change) => page.evaluate((change) => window.__kestrelParity2.setOptions(change), change);

/** Captured through the debugging protocol, which unlike Playwright's own capture does not resize the window. */
async function shotBoth(name, target = page, targetSession = session) {
  for (const scheme of ["dark", "light"]) {
    await target.emulateMedia({ colorScheme: scheme });
    await target.waitForTimeout(200);
    const { data } = await targetSession.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `parity2-${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await target.emulateMedia({ colorScheme: null });
}

/** Opens a folder or location through the address bar and waits for its last crumb. */
async function open(path, crumb, target = page) {
  await target.keyboard.press("Control+l");
  await target.locator(".address-input").fill(path);
  await target.keyboard.press("Enter");
  await target.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, crumb);
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

const chooseView = async (label) => {
  await page.locator(".view-button").click();
  await menuItem(label).click();
  await menu.first().waitFor({ state: "detached" });
};
/** Opens the View menu's Show submenu and chooses `label` there. */
const chooseShow = async (label) => {
  await page.locator(".view-button").click();
  await menuItem("Show").click();
  await menuItem(label).click();
  await menu.first().waitFor({ state: "detached" });
};
const swapCount = () => page.evaluate(() => window.__kestrelSwaps.length);
const lastSwaps = (since) => page.evaluate((since) => window.__kestrelSwaps.slice(since), since);
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)] ?? 0;
};

// ---------- the bin, which the zip passes through ----------

const binItems = () => invoke("recycle_bin_items");
const sandboxKey = comparable(sandbox);
/** The bin's items the suite didn't put there: those not deleted from its scratch folder. */
const othersInBin = async () =>
  (await binItems())
    .filter((entry) => comparable(entry.folder) !== sandboxKey)
    .map((entry) => entry.id)
    .sort();
const oursInBin = async () => (await binItems()).filter((entry) => comparable(entry.folder) === sandboxKey);
const othersBefore = await othersInBin();

// Windows' own Recent items, read only, to show Clear leaves them alone.
const recentFolder = join(process.env.APPDATA ?? "", "Microsoft", "Windows", "Recent");
const recentBefore = existsSync(recentFolder) ? readdirSync(recentFolder) : [];

let opened = null;
const timings = {};

try {
  await step("the scratch folder opens", async () => {
    await list.waitFor();
    await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
    await item("alpha.txt").waitFor();
  });

  // ---------- 7. rows never go blank while the list is refreshed or re-sorted ----------

  await step("refreshing and re-sorting never draws a row without its item, in details, list and icon views", async () => {
    await open(MANY, "many");
    await item("item 000.txt").waitFor();
    // Watch every animation frame, and every change to the rows in between, for a row or item
    // drawn without its item (no path), and for a list drawn with no rows at all.
    // Paused while the suite changes the view or scrolls, which bring rows into view that have
    // never been fetched: only refreshes and re-sorts are watched.
    await page.evaluate(() => {
      const watch = { frames: 0, emptyFrames: 0, emptyRows: 0, blankFrames: 0, mutations: 0, running: true, paused: true, phase: "", seen: [] };
      const items = () => document.querySelectorAll(".tab-panel.active .row[data-index], .tab-panel.active .view-item[data-index]");
      const note = (what) => {
        if (watch.seen.length < 10) watch.seen.push(`${watch.phase}: ${what}`);
      };
      const tick = () => {
        if (!watch.running) return;
        if (!watch.paused) {
          watch.frames++;
          const all = items();
          let empty = 0;
          for (const element of all) if (!element.hasAttribute("data-path")) empty++;
          if (empty > 0) {
            watch.emptyFrames++;
            note(`${empty} empty in a frame`);
          }
          watch.emptyRows += empty;
          if (all.length === 0) watch.blankFrames++;
        }
        requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      // Between frames too: a row that loses its path, or arrives without one, even for a moment.
      const observer = new MutationObserver((records) => {
        if (watch.paused) return;
        for (const record of records) {
          const target = record.target;
          if (record.type === "attributes" && target instanceof HTMLElement && target.matches("[data-index]") && !target.hasAttribute("data-path")) {
            watch.mutations++;
            note(`row ${target.dataset.index} lost its path`);
          }
          for (const node of record.addedNodes) {
            if (node instanceof HTMLElement && node.matches?.(".row[data-index], .view-item[data-index]") && !node.hasAttribute("data-path")) {
              watch.mutations++;
              note(`row ${node.dataset.index} came without a path`);
            }
          }
        }
      });
      observer.observe(document.querySelector(".content"), { subtree: true, childList: true, attributes: true, attributeFilter: ["data-path"] });
      watch.stop = () => {
        watch.running = false;
        observer.disconnect();
      };
      window.__flicker = watch;
    });
    const before = await swapCount();
    const views = [
      ["Details", "details"],
      ["List", "list"],
      ["Medium icons", "medium"],
      ["Tiles", "tiles"],
    ];
    let n = 0;
    for (const [label, mode] of views) {
      // From the top, which a new view keeps in sight.
      await list.focus();
      await page.keyboard.press("Home");
      await chooseView(label);
      await panel.locator(`.filelist[data-view="${mode}"]`).waitFor();
      await panel.locator(".row[data-path], .view-item[data-path]").first().waitFor();
      // Somewhere in the middle, so a refresh has a place to keep.
      await list.evaluate((viewport) => {
        if (viewport.scrollHeight > viewport.clientHeight) viewport.scrollTop = viewport.scrollHeight / 3;
        else viewport.scrollLeft = viewport.scrollWidth / 3;
      });
      // Every row in view loaded before watching.
      await page.waitForFunction(() => [...document.querySelectorAll(".tab-panel.active [data-index]")].every((element) => element.hasAttribute("data-path")));
      await page.waitForTimeout(200);
      for (let round = 0; round < 3; round++) {
        // F5, a change made by another program, and a re-sort, one after another.
        await page.evaluate((phase) => Object.assign(window.__flicker, { paused: false, phase }), `${mode} ${round}`);
        let count = await swapCount();
        await list.focus();
        await page.keyboard.press("F5");
        await page.waitForFunction((count) => window.__kestrelSwaps.length > count, count);
        count = await swapCount();
        writeFileSync(join(MANY, `added ${mode} ${round}.txt`), "y".repeat(round * 100));
        await page.waitForFunction((count) => window.__kestrelSwaps.length > count, count, { timeout: 15_000 });
        count = await swapCount();
        await page.locator(".view-button").click();
        await menuItem(round % 2 === 0 ? "Sort by size" : "Sort by name").click();
        await page.waitForFunction((count) => window.__kestrelSwaps.length > count, count);
        n += 3;
        await page.waitForTimeout(60);
        await page.evaluate(() => (window.__flicker.paused = true));
      }
    }
    const watch = await page.evaluate(() => {
      window.__flicker.stop();
      const { stop, ...counts } = window.__flicker;
      return counts;
    });
    const swaps = await lastSwaps(before);
    console.log(`    ${watch.frames} frames sampled over ${n} refreshes and re-sorts: ${watch.emptyFrames} with an empty row, ${watch.blankFrames} blank, ${watch.mutations} rows drawn empty between frames`);
    if (watch.seen.length > 0) console.log(`    first seen: ${watch.seen.join("; ")}`);
    assert(watch.frames > 100, `enough frames were sampled (${watch.frames})`);
    assert.equal(watch.emptyRows, 0, "no row or item was drawn without its item");
    assert.equal(watch.mutations, 0, "no row lost its item between frames either");
    assert.equal(watch.blankFrames, 0, "the list never went blank");
    const waits = swaps.filter((swap) => !swap.kind.startsWith("new")).map((swap) => swap.waited);
    timings.swapMedian = median(waits);
    timings.swapMax = Math.max(...waits);
    console.log(`    the new rows waited ${timings.swapMedian.toFixed(1)} ms (median), ${timings.swapMax.toFixed(1)} ms at most, over ${waits.length} swaps`);
    assert(swaps.every((swap) => !swap.kind.includes("late")), `every swap had its rows in time: ${JSON.stringify(swaps.filter((swap) => swap.kind.includes("late")))}`);
    assert(timings.swapMedian < 50, `a swap waits well within the 50 ms keystroke budget (${timings.swapMedian} ms)`);
    await chooseView("Details");
    await page.locator(".view-button").click();
    await menuItem("Sort by name").click();
  });

  await step("opening a folder of 10,000 files, typing a search and moving the focus stay within the plan's targets", async () => {
    // The first visit puts the new files into the index; the second is an indexed folder's.
    for (const visit of ["first", "second"]) {
      await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
      await item("alpha.txt").waitFor();
      const since = await swapCount();
      const painted = await page.evaluate(async (big) => {
        const started = performance.now();
        window.__kestrelParity2.navigate(big);
        return await new Promise((resolve) => {
          const check = () => {
            // A row of the folder's own, not the folder's row in the one above it.
            const first = document.querySelector(".tab-panel.active .row[data-index='0']");
            if (first?.getAttribute("data-path")?.toLowerCase().startsWith(`${big.toLowerCase()}\\`)) resolve(performance.now() - started);
            else if (performance.now() - started > 15000) resolve(Infinity);
            else requestAnimationFrame(check);
          };
          check();
        });
      }, BIG);
      const [swap] = (await lastSwaps(since)).filter((each) => each.kind.startsWith("new"));
      const listed = await page.evaluate(() => document.querySelector(".statusbar")?.textContent ?? "");
      timings[`open ${visit}`] = { painted, swap: swap?.waited };
      console.log(`    ${visit} visit: rows painted ${painted.toFixed(1)} ms after asking, of which waiting for the rows ${swap?.waited.toFixed(1)} ms (${listed.replace(/\s+/g, " ").trim()})`);
    }
    assert.match(await page.locator(".statusbar").innerText(), /10,000 items/);
    assert(Number.isFinite(timings["open second"].painted), "the folder's rows were painted");
    assert(timings["open second"].swap < 20, `the rows of an indexed folder of 10k items come within the 20 ms target (${timings["open second"].swap} ms)`);
    // A keystroke in the list: the focus moves and is painted.
    await list.focus();
    await page.keyboard.press("Home");
    // Ten keys, one after another; the median, as the machine may be busy with other work.
    const keystrokes = await page.evaluate(async () => {
      const viewport = document.querySelector(".tab-panel.active .list-viewport");
      const times = [];
      for (let key = 1; key <= 10; key++) {
        const started = performance.now();
        viewport.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
        times.push(
          await new Promise((resolve) => {
            const check = () => {
              if (document.querySelector(".tab-panel.active .row.focused")?.getAttribute("data-index") === String(key)) requestAnimationFrame(() => resolve(performance.now() - started));
              else if (performance.now() - started > 15000) resolve(Infinity);
              else requestAnimationFrame(check);
            };
            check();
          }),
        );
      }
      return times;
    });
    const keystroke = median(keystrokes);
    timings.keystroke = keystroke;
    console.log(`    a key in the list painted ${keystroke.toFixed(1)} ms after it was pressed (median of ${keystrokes.length}; ${keystrokes.map((time) => time.toFixed(0)).join(", ")})`);
    assert(keystroke < 50, `keystroke to paint within 50 ms (${keystroke} ms)`);
    // A key typed into the search box: the results are painted.
    const search = await page.evaluate(async (big) => {
      const input = document.querySelector(".searchbox input");
      input.focus();
      const started = performance.now();
      input.value = "file 0999";
      input.dispatchEvent(new InputEvent("input", { bubbles: true }));
      return await new Promise((resolve) => {
        const check = () => {
          // Results, which match both words: "file 00999.dat" comes first.
          const first = document.querySelector(".tab-panel.active .filelist.results .row[data-index='0']");
          if (first?.getAttribute("data-path")?.toLowerCase() === `${big.toLowerCase()}\\file 00999.dat`) resolve(performance.now() - started);
          else if (performance.now() - started > 15000) resolve(Infinity);
          else requestAnimationFrame(check);
        };
        check();
      });
    }, BIG);
    timings.search = search;
    console.log(`    a search typed ("file 0999", everywhere) painted its results ${search.toFixed(1)} ms after the key`);
    assert(Number.isFinite(search), "the search's results were painted");
    assert(search < 100, `a search's results are painted promptly (${search} ms; the plan's keystroke target is 50 ms)`);
    await page.locator(".searchbox input").fill("");
    await page.keyboard.press("Escape");
  });

  // ---------- 3. views and selection ----------

  await step("compact view packs the rows tighter, in the View menu and remembered", async () => {
    await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
    await item("alpha.txt").waitFor();
    const height = () => item("alpha.txt").evaluate((row) => row.getBoundingClientRect().height);
    assert.equal(await height(), 28);
    // The View menu: Compact view, and Show with the panes and item check boxes.
    await page.locator(".view-button").click();
    await menuItem("Show").hover();
    await menuItem("Item check boxes").waitFor();
    assert.equal(await menuItem("Navigation pane").getAttribute("aria-checked"), "true");
    assert.equal(await menuItem("Item check boxes").getAttribute("aria-checked"), "false");
    await shotBoth("0-view-menu-show");
    await page.keyboard.press("Escape");
    await page.keyboard.press("Escape");
    await menu.first().waitFor({ state: "detached" });
    await chooseView("Compact view");
    await panel.locator(".filelist.compact").waitFor();
    assert.equal(await height(), 22);
    assert.equal((await hook("options")).compact, true);
    assert.match(await page.evaluate(() => localStorage.getItem("kestrel.parity2")), /"compact":true/);
    await shotBoth("1-compact-details");
    await chooseView("Medium icons");
    await panel.locator('.filelist.compact[data-view="medium"]').waitFor();
    await item("picture.png").waitFor();
    await shotBoth("2-compact-icons");
    await chooseView("Details");
    await chooseView("Compact view");
    await panel.locator(".filelist:not(.compact)").waitFor();
    assert.equal(await height(), 28);
  });

  await step("item check boxes show on the item under the pointer and on selected items, and a click toggles selection in every view", async () => {
    await chooseShow("Item check boxes");
    await panel.locator(".filelist.checkboxes").waitFor();
    const check = (name) => item(name).locator(".item-check");
    /** Clicks an item's check box, which shows once the pointer is over the item. */
    const toggle = async (name) => {
      await item(name).hover();
      await check(name).click();
    };
    const selectedNames = () => panel.locator(".row.selected .label, .view-item.selected .label").allTextContents();
    await item("alpha.txt").hover();
    assert.equal(await check("alpha.txt").evaluate((box) => getComputedStyle(box).visibility), "visible");
    assert.equal(await check("gamma.txt").evaluate((box) => getComputedStyle(box).visibility), "hidden");
    await toggle("alpha.txt");
    await toggle("gamma.txt");
    assert.deepEqual((await selectedNames()).sort(), ["alpha.txt", "gamma.txt"]);
    assert.equal(await check("gamma.txt").getAttribute("aria-checked"), "true");
    await page.mouse.move(5, 300);
    await shotBoth("3-check-boxes-details");
    await toggle("alpha.txt");
    assert.deepEqual(await selectedNames(), ["gamma.txt"]);
    for (const [label, mode] of [["Medium icons", "medium"], ["List", "list"], ["Tiles", "tiles"], ["Content", "content"]]) {
      await chooseView(label);
      await panel.locator(`.filelist.checkboxes[data-view="${mode}"]`).waitFor();
      await toggle("beta.txt");
      assert.deepEqual((await selectedNames()).sort(), ["beta.txt", "gamma.txt"], `${label}: the check box adds to the selection`);
      if (mode === "medium") await shotBoth("4-check-boxes-icons");
      await toggle("beta.txt");
      assert.deepEqual(await selectedNames(), ["gamma.txt"], `${label}: and takes away`);
      // A double-click on the box toggles twice and opens nothing.
      await item("beta.txt").hover();
      await check("beta.txt").dblclick();
      assert.equal(await panel.locator(`.filelist[data-view="${mode}"]`).count(), 1);
      assert.deepEqual(await selectedNames(), ["gamma.txt"]);
    }
    await chooseView("Details");
    await chooseShow("Item check boxes");
    await panel.locator(".filelist:not(.checkboxes)").waitFor();
    assert.equal(await panel.locator(".item-check").count(), 0);
  });

  await step("Ctrl+Space selects and deselects the focused item, and Ctrl+arrows move the focus without changing the selection", async () => {
    // Files come after the folders, by name: alpha, beta, dropped, gamma...
    await item("alpha.txt").click();
    const focusedName = () => panel.locator(".row.focused .label").innerText();
    const selectedNames = () => panel.locator(".row.selected .label").allTextContents();
    await page.keyboard.press("Control+ArrowDown");
    await page.keyboard.press("Control+ArrowDown");
    assert.equal(await focusedName(), "dropped.txt");
    assert.deepEqual(await selectedNames(), ["alpha.txt"], "the selection stays");
    await page.keyboard.press("Control+Space");
    assert.deepEqual(await selectedNames(), ["alpha.txt", "dropped.txt"]);
    await page.keyboard.press("Control+ArrowUp");
    assert.equal(await focusedName(), "beta.txt");
    await page.keyboard.press("Control+Space");
    assert.deepEqual(await selectedNames(), ["alpha.txt", "beta.txt", "dropped.txt"]);
    await page.keyboard.press("Control+Space");
    assert.deepEqual(await selectedNames(), ["alpha.txt", "dropped.txt"], "Ctrl+Space again takes it out");
    await page.keyboard.press("Control+End");
    assert.equal(await focusedName(), "squeezed.txt");
    assert.deepEqual(await selectedNames(), ["alpha.txt", "dropped.txt"]);
    // A plain arrow selects as before.
    await page.keyboard.press("ArrowUp");
    assert.equal((await selectedNames()).length, 1);
  });

  await step("a compressed file's name is blue, as Explorer colours it, until the option is off", async () => {
    execFileSync("compact", ["/c", join(sandbox, "squeezed.txt")], { stdio: "ignore" });
    await list.focus();
    await page.keyboard.press("F5");
    await eventually(async () => assert(await item("squeezed.txt").evaluate((row) => row.classList.contains("ntfs-compressed"))));
    const colour = () => item("squeezed.txt").locator(".label").evaluate((label) => getComputedStyle(label).color);
    const plain = await item("alpha.txt").locator(".label").evaluate((label) => getComputedStyle(label).color);
    assert.notEqual(await colour(), plain);
    assert.equal(await item("alpha.txt").evaluate((row) => row.classList.contains("ntfs-compressed")), false);
    console.log(`    squeezed.txt is ${await colour()} (plain names ${plain})`);
    await item("squeezed.txt").click();
    await shotBoth("5-compressed-colour");
    await setOptions({ ntfsColour: false });
    assert.equal(await colour(), plain);
    await setOptions({ ntfsColour: true });
    assert.notEqual(await colour(), plain);
  });

  await step("Always show icons, never thumbnails", async () => {
    await chooseView("Large icons");
    const thumb = item("picture.png").locator("img.view-item-thumb.shown");
    await thumb.waitFor({ timeout: 30_000 });
    await setOptions({ alwaysIcons: true });
    await eventually(async () => assert.equal(await item("picture.png").locator("img.view-item-thumb").count(), 0));
    assert(await item("picture.png").locator("img.view-item-icon").isVisible());
    await setOptions({ alwaysIcons: false });
    await thumb.waitFor({ timeout: 30_000 });
    await chooseView("Details");
  });

  await step("the navigation pane hides from View > Show and stays hidden after a reload", async () => {
    const sidebar = page.locator(".sidebar");
    assert(await sidebar.isVisible());
    await chooseShow("Navigation pane");
    await sidebar.waitFor({ state: "hidden" });
    const left = await panel.evaluate((element) => element.getBoundingClientRect().left);
    assert(left < 4, `the files take the room (${left})`);
    await shotBoth("6-navigation-pane-hidden");
    await page.reload();
    await page.waitForFunction(() => typeof window.__kestrelParity2 === "object");
    await page.locator(".tab-panel.active .list-viewport").waitFor();
    assert(!(await page.locator(".sidebar").isVisible()), "remembered");
    await chooseShow("Navigation pane");
    await page.locator(".sidebar").waitFor();
    await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
  });

  await step("Num Lock+plus and Num Lock+minus expand and collapse in the folder tree", async () => {
    await page.keyboard.press("Control+Shift+E");
    const node = () => page.evaluate((path) => window.__kestrelTree.rows().find((row) => row.path.toLowerCase() === path.toLowerCase()), sandbox);
    await eventually(async () => assert((await node())?.path, "the scratch folder is in the tree"), 20_000);
    // The keyboard goes to the scratch folder's row: a click on its name (its middle may be its chevron).
    const label = await page.waitForFunction(
      (path) => [...document.querySelectorAll(".tree-row")].find((row) => row.getAttribute("data-tree-path")?.toLowerCase() === path.toLowerCase())?.querySelector(".tree-label"),
      sandbox,
      { timeout: 20_000 },
    );
    await label.asElement().click();
    await page.waitForFunction(() => document.activeElement?.classList.contains("tree"));
    // The keyboard is on the open folder, which holds folders.
    await page.keyboard.press("NumpadAdd");
    await eventually(async () => assert.equal((await node()).expanded, true));
    await page.keyboard.press("NumpadSubtract");
    await eventually(async () => assert.equal((await node()).expanded, false));
    await page.keyboard.press("NumpadAdd");
    await eventually(async () => assert.equal((await node()).expanded, true));
    assert(
      await page.evaluate((path) => window.__kestrelTree.rows().some((row) => row.path.toLowerCase() === `${path.toLowerCase()}\\many`), sandbox),
      "its folders are in the tree",
    );
    await page.keyboard.press("NumpadSubtract");
    await eventually(async () => assert.equal((await node()).expanded, false));
    await list.focus();
  });

  // ---------- 4. tabs and windows ----------

  await step("a tab's menu duplicates it, with its history, and closes the others", async () => {
    await open(join(sandbox, "sub"), "sub");
    await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
    const original = await hook("activeTab");
    await page.locator(`.titlebar .tab[data-tab-id="${original}"]`).click({ button: "right" });
    await menuItem("Duplicate tab").waitFor();
    assert.deepEqual(await hook("tabMenu", original), ["Duplicate tab", "Open in new window", "-", "Close tab", "Close other tabs", "Close tabs to the right"]);
    await shotBoth("7-tab-menu");
    await menuItem("Duplicate tab").click();
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 2);
    const [first, copy] = await hook("tabs");
    assert.equal(first.id, original, "the copy comes after the tab it was made from");
    assert.equal(comparable(copy.path), comparable(sandbox));
    assert.deepEqual(copy.history.map(comparable), first.history.map(comparable));
    assert.equal(await hook("activeTab"), copy.id);
    await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, `kestrel-e2e-parity2-${process.pid}`);
    // Back in the copy goes where the original had been.
    await page.keyboard.press("Alt+ArrowLeft");
    await page.waitForFunction(() => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === "sub");
    await page.keyboard.press("Alt+ArrowRight");
    await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, `kestrel-e2e-parity2-${process.pid}`);
    await hook("duplicate", copy.id);
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 3);
    await hook("closeOthers", original);
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 1);
    await hook("duplicate", original);
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 2);
  });

  await step("a tab dragged out of the strip opens in a new window, and dropped on the first window's strip moves back", async () => {
    const windowsBefore = await invoke("app_windows_dev");
    const [, leaving] = await hook("tabs");
    const result = await hook("dropTab", leaving.id, { label: null, x: 0, y: 0 });
    assert.equal(result, "window");
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 1);
    let label = null;
    await eventually(async () => {
      label = (await invoke("app_windows_dev")).find((each) => !windowsBefore.includes(each));
      assert(label, "a new window");
    });
    const second = await waitForKestrelWindow(browser, label);
    const secondSession = await second.context().newCDPSession(second);
    await second.waitForFunction(() => typeof window.__kestrelParity2 === "object");
    await second.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, `kestrel-e2e-parity2-${process.pid}`, { timeout: 20_000 });
    await second.locator(".tab-panel.active .row").first().waitFor();
    await shotBoth("8-tab-in-new-window", second, secondSession);
    // Dropped on the first window's tab strip, the tab moves back and its window closes.
    const strip = await page.evaluate(() => {
      const box = document.querySelector(".titlebar").getBoundingClientRect();
      const tabs = document.querySelector(".titlebar .tabs").getBoundingClientRect();
      return { x: Math.min(tabs.right + 40, box.right - 200), y: box.top + box.height / 2 };
    });
    // A point of the first window that is not its strip is no place for it.
    const [stray] = await second.evaluate(() => window.__kestrelParity2.tabs());
    const content = await page.evaluate(() => {
      const box = document.querySelector(".tab-panel.active").getBoundingClientRect();
      return { x: box.left + box.width / 2, y: box.top + box.height / 2 };
    });
    await second.evaluate(() => window.__kestrelParity2.duplicate(window.__kestrelParity2.tabs()[0].id));
    await second.waitForFunction(() => window.__kestrelParity2.tabs().length === 2);
    const declined = await second.evaluate(([id, at]) => window.__kestrelParity2.dropTab(id, at), [stray.id, { label: "main", ...content }]);
    assert.equal(declined, "window", "let go over the first window's files, the tab gets a window of its own");
    let third = null;
    await eventually(async () => {
      third = (await invoke("app_windows_dev")).find((each) => !windowsBefore.includes(each) && each !== label);
      assert(third, "a third window");
    });
    const thirdPage = await waitForKestrelWindow(browser, third);
    await thirdPage.waitForFunction(() => typeof window.__kestrelParity2 === "object" && window.__kestrelParity2.tabs().length === 1);
    await thirdPage.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, `kestrel-e2e-parity2-${process.pid}`, { timeout: 20_000 });
    const [lastOfThird] = await thirdPage.evaluate(() => window.__kestrelParity2.tabs());
    const moved = await thirdPage.evaluate(([id, at]) => window.__kestrelParity2.dropTab(id, at), [lastOfThird.id, { label: "main", ...strip }]);
    assert.equal(moved, "moved");
    const [lastOfSecond] = await second.evaluate(() => window.__kestrelParity2.tabs());
    const movedAgain = await second.evaluate(([id, at]) => window.__kestrelParity2.dropTab(id, at), [lastOfSecond.id, { label: "main", ...strip }]);
    assert.equal(movedAgain, "moved");
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 3);
    const tabs = await hook("tabs");
    assert(tabs.every((tab) => comparable(tab.path) === comparable(sandbox)), JSON.stringify(tabs));
    await eventually(async () => assert.deepEqual((await invoke("app_windows_dev")).sort(), [...windowsBefore].sort(), "the windows whose last tab left have closed"));
    await page.waitForFunction((name) => [...document.querySelectorAll(".crumb")].at(-1)?.textContent === name, `kestrel-e2e-parity2-${process.pid}`);
    await shotBoth("9-tabs-moved-back");
    await hook("closeOthers", (await hook("activeTab")));
    await page.waitForFunction(() => window.__kestrelParity2.tabs().length === 1);
  });

  // ---------- 2. libraries ----------

  await step("Libraries lists the account's libraries, and a library shows the items of all its folders as one", async () => {
    await invoke("libraries_dev_folder", { folder: LIBS });
    await open("Libraries", "Libraries");
    const tile = page.locator(`.tab-panel.active .pc-tile[data-tile="shell:Libraries\\\\Shelves"]`);
    await tile.waitFor();
    assert.match(await tile.innerText(), /Shelves[\s\S]*2 folders/);
    await shotBoth("10-libraries");
    await tile.dblclick();
    await page.waitForFunction(() => [...document.querySelectorAll(".crumb")].map((crumb) => crumb.textContent).join(" > ") === "Libraries > Shelves");
    await item("from-a.txt").waitFor();
    assert.deepEqual(await panel.locator(".row .label").allTextContents(), ["b-folder", "from-a.txt", "from-b.txt"]);
    assert.equal(comparable(await item("from-a.txt").getAttribute("data-path")), comparable(join(SHELF_A, "from-a.txt")));
    assert.equal(comparable(await item("from-b.txt").getAttribute("data-path")), comparable(join(SHELF_B, "from-b.txt")));
    assert.match(await page.locator(".titlebar .tab.active").innerText(), /Shelves/);
    await shotBoth("11-library-listing");
    // New folder goes into the library's save folder.
    await list.click({ position: { x: 400, y: 300 } });
    await page.keyboard.press("Control+Shift+N");
    const input = panel.locator("input.rename");
    await input.waitFor();
    await page.keyboard.press("Escape");
    await eventually(() => assert(existsSync(join(SHELF_B, "New folder")), "made in the save folder"));
    // Up goes back to Libraries.
    await page.keyboard.press("Alt+ArrowUp");
    await tile.waitFor();
    // With "Show libraries" (the folder tree's menu has it, as Explorer's navigation pane does),
    // the navigation pane lists them.
    await page.locator(".tree-row .tree-label").first().click({ button: "right" });
    assert.equal(await menuItem("Show libraries").getAttribute("aria-checked"), "false");
    await menuItem("Show libraries").click();
    assert.equal((await hook("options")).showLibraries, true);
    const place = page.locator(".sidebar .libraries-section .place", { hasText: "Shelves" });
    await place.waitFor();
    await place.click();
    await item("from-b.txt").waitFor();
    await shotBoth("12-libraries-in-navigation-pane");
    await setOptions({ showLibraries: false });
    await page.locator(".sidebar .libraries-section").waitFor({ state: "detached" });
  });

  // ---------- 1. the network ----------

  await step("the Network location shows the computers found, and the network's dialogs are Windows' own", async () => {
    await invoke("network_dev_computers", { computers: ["KESTREL-TEST-NAS", "KESTREL-TEST-DESK"], delayMs: 0 });
    await open("Network", "Network");
    const nas = page.locator('.tab-panel.active .pc-tile[data-tile="\\\\\\\\KESTREL-TEST-NAS"]');
    await nas.waitFor({ timeout: 15_000 });
    assert.equal(await page.locator(".tab-panel.active .pc-tile").filter({ hasText: "KESTREL-TEST" }).count(), 2);
    await shotBoth("13-network");
    // Up from a computer's page is the Network.
    await nas.dblclick();
    await page.waitForFunction(() => [...document.querySelectorAll(".crumb")].map((crumb) => crumb.textContent).join(" > ") === "Network > KESTREL-TEST-NAS");
    await page.locator(".tab-panel.active .tile-page-note").waitFor({ timeout: 20_000 });
    await page.keyboard.press("Alt+ArrowUp");
    await nas.waitFor();

    // This PC's "..." menu, its page's menu and the tree's This PC: the dialogs are recorded, not shown.
    await invoke("network_dev_record", { on: true });
    await open("This PC", "This PC");
    await page.locator(".commandbar .command[aria-label='More']").click();
    assert.deepEqual((await page.locator(".menu .menu-label").allTextContents()).slice(0, 3), ["Map network drive…", "Disconnect network drive…", "Add a network location"]);
    await shotBoth("14-this-pc-more-menu");
    await menuItem("Map network drive…").click();
    await page.locator(".commandbar .command[aria-label='More']").click();
    await menuItem("Disconnect network drive…").click();
    await page.locator(".pc-page").click({ button: "right", position: { x: 6, y: 6 } });
    await menuItem("Add a network location").click();
    await eventually(async () => {
      const calls = await invoke("network_dev_take");
      assert.deepEqual(
        calls.map((call) => call.function),
        ["WNetConnectionDialog1", "WNetDisconnectDialog", "AddNetPlaceRunDll"],
      );
    });
    await invoke("network_dev_record", { on: false });
  });

  // ---------- 5. privacy ----------

  await step("Folder Options' privacy switches hide recent files and frequent folders, and Clear forgets Kestrel's history", async () => {
    const opened = join(sandbox, "beta.txt");
    assert(await invoke("frecency_visit", { path: "C:\\Windows\\Help" }));
    await invoke("record_opened", { path: opened });
    await page.evaluate(() => window.__kestrelHome.windowsRecent(false));
    await open("Home", "Home");
    const recent = page.locator('.home section[aria-label="Recent"]');
    const quick = page.locator('.home section[aria-label="Quick access"]');
    await recent.getByText("beta.txt").waitFor();
    await quick.getByText("Help").waitFor();
    await page.evaluate(() => window.__kestrelParity.openOptions());
    const dialog = page.locator(".options-dialog");
    await dialog.waitFor();
    await dialog.locator('input[data-option="showRecent"]').uncheck();
    await recent.waitFor({ state: "detached" });
    await dialog.locator('input[data-option="showFrequent"]').uncheck();
    await quick.waitFor({ state: "detached" });
    await page.locator('.sidebar section[aria-label="Frequent"]').waitFor({ state: "detached" });
    await dialog.locator(".options-privacy").scrollIntoViewIfNeeded();
    await shotBoth("15-folder-options-privacy");
    await dialog.locator('input[data-option="showRecent"]').check();
    await dialog.locator('input[data-option="showFrequent"]').check();
    await recent.getByText("beta.txt").waitFor();
    await dialog.locator(".options-clear .dialog-button").click();
    await dialog.locator(".options-clear .dialog-button", { hasText: "Cleared" }).waitFor();
    await dialog.locator(".dialog-footer .dialog-button.primary").click();
    await recent.getByText("Files you open will be here.").waitFor();
    await quick.getByText("The folders you open most often will be here.").waitFor();
    assert.deepEqual(await invoke("frecency_top", { limit: 50 }), []);
    assert.deepEqual(await invoke("home_recent", { limit: 50, windows: false }), []);
    // Windows' own Recent items are as they were: none went.
    const recentAfter = existsSync(recentFolder) ? readdirSync(recentFolder) : [];
    assert(recentBefore.every((name) => recentAfter.includes(name)), "Windows' Recent folder lost nothing");
    console.log(`    Windows' Recent folder: ${recentBefore.length} items before, ${recentAfter.length} after`);
    await page.evaluate(() => window.__kestrelHome.windowsRecent(true));
  });

  // ---------- 6. files dropped on a zip ----------

  await step("a file dropped on a zip is added to it, the old zip goes to the Recycle Bin, and Ctrl+Z puts it back", async () => {
    await open(sandbox, `kestrel-e2e-parity2-${process.pid}`);
    await item(ZIP_NAME).waitFor();
    assert.deepEqual(zipEntries(), ["inside.txt"]);
    assert.equal(await item(ZIP_NAME).getAttribute("data-zip"), "");
    const box = await item(ZIP_NAME).boundingBox();
    const at = { x: box.x + 60, y: box.y + box.height / 2 };
    const dropped = join(sandbox, "dropped.txt");
    await page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [[dropped], at]);
    await page.waitForFunction(() => window.__kestrelDragDrop.target()?.kind === "zip");
    await page.waitForFunction(() => window.__kestrelDragDrop.hint()?.verb === "Copy");
    const hint = await page.evaluate(() => window.__kestrelDragDrop.hint());
    assert.deepEqual([hint.verb, hint.name], ["Copy", ZIP_NAME]);
    assert(await item(ZIP_NAME).evaluate((row) => row.classList.contains("drop-target")));
    await shotBoth("16-drop-on-zip");
    const result = await page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [[dropped], at]);
    assert.deepEqual(result, { effect: "copy", names: ["dropped.txt"] });
    assert.deepEqual(zipEntries(), ["dropped.txt", "inside.txt"]);
    assert(existsSync(dropped), "the file dropped stays where it was");
    const binned = await oursInBin();
    assert.deepEqual(binned.map((entry) => entry.name), [ZIP_NAME], "the old zip is in the Recycle Bin");
    // Inside the zip, a paste is an addition too (the clipboard is left alone: the backend's
    // own adding is what Ctrl+V calls), and so is a drop on the zip's open folder.
    await item(ZIP_NAME).dblclick();
    await item("dropped.txt").waitFor();
    const inner = await list.boundingBox();
    const into = { x: inner.x + inner.width / 2, y: inner.y + inner.height - 30 };
    const gamma = join(sandbox, "gamma.txt");
    await page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [[gamma], into]);
    await page.waitForFunction(() => window.__kestrelDragDrop.target()?.kind === "zip");
    await page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [[gamma], into]);
    await item("gamma.txt").waitFor();
    assert.deepEqual(zipEntries(), ["dropped.txt", "gamma.txt", "inside.txt"]);
    await shotBoth("17-zip-with-added-files");
    await page.keyboard.press("Backspace");
    await item(ZIP_NAME).waitFor();
    // Undo puts the zip before the last addition back.
    await list.focus();
    await page.keyboard.press("Control+z");
    await eventually(() => assert.deepEqual(zipEntries(), ["dropped.txt", "inside.txt"]), 15_000);
    await page.keyboard.press("Control+z");
    await eventually(() => assert.deepEqual(zipEntries(), ["inside.txt"]), 15_000);
    // What of the suite's is in the bin (the zips undone) is deleted from it.
    const ours = await oursInBin();
    assert(ours.length >= 2, `${ours.length} of the suite's zips in the bin`);
    await invoke("recycle_bin_delete", { ids: ours.map((entry) => entry.id) });
    await eventually(async () => assert.equal((await oursInBin()).length, 0));
  });
} finally {
  await invoke("libraries_dev_folder", { folder: null }).catch(() => {});
  await invoke("network_dev_computers", { computers: null }).catch(() => {});
  await invoke("network_dev_record", { on: false }).catch(() => {});
  await page.evaluate(() => window.__kestrelHome?.windowsRecent(true)).catch(() => {});
  // Close any window the suite opened, and tabs it left.
  for (const label of (await invoke("app_windows_dev").catch(() => [])).filter((label) => label !== "main")) {
    const other = await waitForKestrelWindow(browser, label, 2000).catch(() => null);
    await other?.evaluate(() => window.__kestrelParity2.closeWindow()).catch(() => {});
  }
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  await page.reload().catch(() => {});
  const ours = await oursInBin().catch(() => []);
  if (ours.length > 0) await invoke("recycle_bin_delete", { ids: ours.map((entry) => entry.id) }).catch(() => {});
  const othersAfter = await othersInBin().catch(() => othersBefore);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
  assert.deepEqual(othersAfter, othersBefore, "the bin's other items are untouched");
  console.log(`timings: ${JSON.stringify(timings)}`);
}
