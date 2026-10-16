// End-to-end test of drag and drop against the real app window.
//
// A real mouse drag between applications can't be scripted, so the drops are synthetic: they
// go through the dev build's test hook (window.__kestrelDragDrop), which feeds the same handler
// Tauri's drag events reach, and through the backend's real copy and move. Ctrl and Shift are
// named by the test, since synthetic drops can't hold real keys. Dragging out is checked up to
// the native drag: the hook makes it a dry run that builds the shell data object on the
// window's thread but never starts the drag, so nothing can be dropped anywhere.
//
// Everything happens inside a scratch folder this script creates under the temp directory
// and removes afterwards. Screenshots go to e2e/out.
//
// Start the app with `node e2e/dev.mjs start`, then run: node e2e/dragdrop.mjs

import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-dragdrop-${process.pid}`);
const inbox = join(sandbox, "inbox");
const archive = join(inbox, "Archive");
const outside = join(sandbox, "outside");
const long = join(sandbox, "long");
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(archive, { recursive: true });
mkdirSync(outside);
mkdirSync(long);
writeFileSync(join(inbox, "notes.txt"), "notes");
for (const name of ["report.txt", "moveme.txt", "photo.txt", "first.txt", "second.txt", "tauri.txt", "late.txt"]) {
  writeFileSync(join(outside, name), name);
}
for (let i = 0; i < 80; i++) writeFileSync(join(long, `file-${String(i).padStart(2, "0")}.txt`), "x");
const onDisk = (...parts) => existsSync(join(...parts));

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const selectedNames = () => panel.locator(".row.selected .label").allTextContents();
const hint = page.locator(".drop-hint");
const step = (await import("./steps.mjs")).stepper("dragdrop", () => page);

/** The centre of an element, in the page's CSS pixels. */
async function centre(locator) {
  const box = await locator.boundingBox();
  assert(box, "element is on screen");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** A point in the list's empty space, below its last row. */
async function emptySpace() {
  const box = await list.boundingBox();
  return { x: box.x + box.width / 2, y: box.y + box.height - 60 };
}

const hook = {
  keys: (keys) => page.evaluate((keys) => window.__kestrelDragDrop.keys(keys ?? undefined), keys ?? null),
  enter: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.enter(paths, at.x, at.y), [paths, at]),
  over: (at) => page.evaluate((at) => window.__kestrelDragDrop.over(at.x, at.y), at),
  leave: () => page.evaluate(() => window.__kestrelDragDrop.leave()),
  drop: (paths, at) => page.evaluate(([paths, at]) => window.__kestrelDragDrop.drop(paths, at.x, at.y), [paths, at]),
  target: () => page.evaluate(() => window.__kestrelDragDrop.target()),
};

/** Opens a folder through the address bar. */
async function open(path, expectName) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await page.locator(".crumb", { hasText: basename(path) }).last().waitFor();
  // By full path: the list keeps the previous folder's rows drawn until the new ones are in,
  // and the previous folder may hold an item of the same name.
  await page.waitForFunction(
    (wanted) =>
      [...document.querySelectorAll(".tab-panel.active [data-path]")].some((element) => element.getAttribute("data-path").toLowerCase() === wanted),
    join(path, expectName).toLowerCase(),
  );
}

/** Captures the window through the debugging protocol, in the given colour scheme. */
async function shot(name, colorScheme) {
  await page.emulateMedia({ colorScheme });
  await page.waitForTimeout(100);
  const session = await page.context().newCDPSession(page);
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}-${colorScheme}.png`), Buffer.from(data, "base64"));
  await session.detach();
}

try {
  await step("the hook is available in the dev build", async () => {
    await panel.locator(".row").first().waitFor();
    assert.equal(await page.evaluate(() => typeof window.__kestrelDragDrop), "object");
    await open(inbox, "Archive");
    await row("notes.txt").waitFor();
    await hook.keys({ ctrl: false, shift: false });
  });

  await step("the folder row under the cursor is highlighted, with what a drop would do", async () => {
    await hook.enter([join(outside, "report.txt")], await centre(row("Archive")));
    await panel.locator(".row.drop-target .label", { hasText: /^Archive$/ }).waitFor();
    await hint.filter({ hasText: "Move to Archive" }).waitFor();
    assert.equal(await page.locator(".drop-target").count(), 1);
    await shot("dragdrop-1-row", "dark");
    await shot("dragdrop-1-row", "light");
    await page.emulateMedia({ colorScheme: null });
  });

  await step("empty space targets the open folder itself", async () => {
    await hook.over(await emptySpace());
    await page.locator(".tab-panel.active.drop-target").waitFor();
    await panel.locator(".row.drop-target").waitFor({ state: "detached" });
    await hint.filter({ hasText: "Move to inbox" }).waitFor();
    await shot("dragdrop-2-folder", "dark");
    await shot("dragdrop-2-folder", "light");
    await page.emulateMedia({ colorScheme: null });
  });

  await step("a file row passes the drop to its folder", async () => {
    await hook.over(await centre(row("notes.txt")));
    await page.locator(".tab-panel.active.drop-target").waitFor();
    assert.equal(await panel.locator(".row.drop-target").count(), 0);
  });

  await step("holding Ctrl turns the move into a copy", async () => {
    await hook.keys({ ctrl: true, shift: false });
    await hint.filter({ hasText: "Copy to inbox" }).waitFor();
    await hook.keys({ ctrl: false, shift: false });
    await hint.filter({ hasText: "Move to inbox" }).waitFor();
  });

  await step("sidebar places and tabs are targets too", async () => {
    // Hovering either for a second would open it, so each screenshot starts a fresh hover.
    const place = await centre(page.locator(".sidebar .place").first());
    // The first place is your user folder, named after it.
    const placeName = await page.locator(".sidebar .place").first().getAttribute("data-place-name");
    const tab = await centre(page.locator(".tab.active"));
    const away = await emptySpace();
    for (const colorScheme of ["dark", "light"]) {
      await hook.over(away);
      await hook.over(place);
      await page.locator(".sidebar .place.drop-target").waitFor();
      assert.equal(await page.locator(".tab-panel.drop-target").count(), 0);
      // A place is named as the sidebar names it.
      await hint.filter({ hasText: `Move to ${placeName}` }).waitFor();
      await shot("dragdrop-3-place", colorScheme);
    }
    for (const colorScheme of ["dark", "light"]) {
      await hook.over(away);
      await hook.over(tab);
      await page.locator(".tab.active.drop-target").waitFor();
      await page.locator(".sidebar .place.drop-target").waitFor({ state: "detached" });
      await shot("dragdrop-4-tab", colorScheme);
    }
    await page.emulateMedia({ colorScheme: null });
    assert.match(await page.locator(".crumb").last().innerText(), /^inbox$/, "nothing opened");
  });

  await step("leaving the window clears every highlight", async () => {
    await hook.leave();
    await page.waitForFunction(() => document.querySelectorAll(".drop-target").length === 0);
    await hint.waitFor({ state: "detached" });
    assert.equal(await hook.target(), null);
  });

  await step("dropping items on the folder they are in does nothing", async () => {
    const notes = join(inbox, "notes.txt");
    await hook.enter([notes], await emptySpace());
    assert.equal(await hook.target(), null, "the folder they are in is not a target");
    assert.equal(await page.locator(".drop-target").count(), 0);
    const result = await hook.drop([notes], await emptySpace());
    assert.deepEqual(result, { effect: "none", names: [] });
    // Not even with Ctrl held: the drop is ignored rather than duplicating in place.
    await hook.keys({ ctrl: true, shift: false });
    const withCtrl = await hook.drop([notes], await centre(page.locator(".tab.active")));
    assert.equal(withCtrl.effect, "none");
    await hook.keys({ ctrl: false, shift: false });
    await page.waitForTimeout(300);
    assert.deepEqual(await panel.locator(".row .label").allTextContents(), ["Archive", "notes.txt"]);
  });

  await step("a folder can't be dropped into itself", async () => {
    await hook.enter([archive], await centre(row("Archive")));
    assert.equal(await hook.target(), null);
    const result = await hook.drop([archive], await centre(row("Archive")));
    assert.equal(result.effect, "none");
    assert(onDisk(archive));
  });

  await step("Ctrl copies into the open folder and selects the copy", async () => {
    const report = join(outside, "report.txt");
    await hook.keys({ ctrl: true, shift: false });
    await hook.enter([report], await emptySpace());
    const result = await hook.drop([report], await emptySpace());
    assert.deepEqual(result, { effect: "copy", names: ["report.txt"] });
    await row("report.txt").waitFor();
    assert(onDisk(inbox, "report.txt") && onDisk(report), "the original stays");
    assert.equal(readFileSync(join(inbox, "report.txt"), "utf8"), "report.txt");
    await page.waitForFunction(() => [...document.querySelectorAll(".tab-panel.active .row.selected .label")].map((l) => l.textContent).join() === "report.txt");
    await hook.keys({ ctrl: false, shift: false });
  });

  await step("without keys, a drop on the same drive moves", async () => {
    const files = [join(outside, "first.txt"), join(outside, "second.txt")];
    await hook.enter(files, await emptySpace());
    const result = await hook.drop(files, await emptySpace());
    assert.equal(result.effect, "move");
    assert.deepEqual([...result.names].sort(), ["first.txt", "second.txt"]);
    await row("first.txt").waitFor();
    await page.waitForFunction(() => [...document.querySelectorAll(".tab-panel.active .row.selected .label")].map((l) => l.textContent).join() === "first.txt,second.txt");
    assert(!onDisk(outside, "first.txt") && !onDisk(outside, "second.txt"));
    assert(onDisk(inbox, "first.txt") && onDisk(inbox, "second.txt"));
  });

  await step("Shift moves onto a folder row", async () => {
    const moveme = join(outside, "moveme.txt");
    await hook.keys({ ctrl: false, shift: true });
    await hook.enter([moveme], await centre(row("Archive")));
    const result = await hook.drop([moveme], await centre(row("Archive")));
    assert.deepEqual(result, { effect: "move", names: ["moveme.txt"] });
    // The shell has finished the move when the drop returns.
    assert(onDisk(archive, "moveme.txt") && !onDisk(moveme) && !onDisk(inbox, "moveme.txt"));
    // The folder on screen is not where the file went, so nothing new appears here.
    await page.waitForTimeout(300);
    assert.equal(await row("moveme.txt").count(), 0);
    await hook.keys({ ctrl: false, shift: false });
  });

  await step("a drop on a folder row while the list refreshes still lands in that folder", async () => {
    // A refreshed list used to fetch its rows again and, for a few milliseconds, draw them with
    // no path; a drop then was taken for one on empty space and sent the file into the open
    // folder instead. The list now keeps its rows until the new ones are in (FileList.tsx), so
    // the row stays a folder row throughout: none goes blank, and the drop, made as the refresh
    // starts, lands in it. (Rows scrolled into view still load; dragdrop.ts's LOADING waits.)
    const late = join(outside, "late.txt");
    await hook.keys({ ctrl: false, shift: true });
    const at = await centre(row("Archive"));
    await hook.enter([late], at);
    const { result, blank, swapped } = await page.evaluate(
      ([paths, at]) =>
        new Promise((resolve) => {
          let blank = 0;
          const rows = document.querySelector(".tab-panel.active .list-rows");
          const observer = new MutationObserver(() => {
            if (document.querySelector(".tab-panel.active .row[data-index]:not([data-path])")) blank++;
          });
          observer.observe(rows, { subtree: true, childList: true, attributes: true });
          const swaps = window.__kestrelSwaps.length;
          document.querySelector('.toolbar button[aria-label="Refresh"]').click();
          window.__kestrelDragDrop.drop(paths, at.x, at.y).then((result) => {
            // The refresh's own swap, and the one the drop's selection asks for.
            const settle = () => {
              if (window.__kestrelSwaps.length > swaps) {
                observer.disconnect();
                resolve({ result, blank, swapped: true });
              } else setTimeout(settle, 20);
            };
            settle();
            setTimeout(() => resolve({ result, blank, swapped: false }), 5000);
          });
        }),
      [[late], at],
    );
    assert(swapped, "the refreshed rows were swapped in");
    assert.equal(blank, 0, "no row went blank while the list refreshed");
    assert.deepEqual(result, { effect: "move", names: ["late.txt"] });
    assert(onDisk(archive, "late.txt") && !onDisk(late) && !onDisk(inbox, "late.txt"), "the file is in Archive");
    await hook.keys({ ctrl: false, shift: false });
  });

  await step("Tauri's own drag events, in physical pixels, reach the same handling", async () => {
    // These are the events Tauri sends when Windows reports a drag over the window.
    const emit = (event, payload) =>
      page.evaluate(([event, payload]) => window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event, payload }), [event, payload]);
    const scale = await page.evaluate(() => window.devicePixelRatio);
    const physical = (at) => ({ x: at.x * scale, y: at.y * scale });
    const file = join(outside, "tauri.txt");
    await emit("tauri://drag-enter", { paths: [file], position: physical(await centre(row("Archive"))) });
    await panel.locator(".row.drop-target .label", { hasText: /^Archive$/ }).waitFor();
    await emit("tauri://drag-over", { position: physical(await emptySpace()) });
    await page.locator(".tab-panel.active.drop-target").waitFor();
    await emit("tauri://drag-drop", { paths: [file], position: physical(await emptySpace()) });
    await row("tauri.txt").waitFor();
    await page.waitForFunction(() => [...document.querySelectorAll(".tab-panel.active .row.selected .label")].map((l) => l.textContent).join() === "tauri.txt");
    assert(onDisk(inbox, "tauri.txt") && !onDisk(file));
    assert.equal(await page.locator(".drop-target").count(), 0);
  });

  await step("hovering a folder for a second opens it, and the drop lands there", async () => {
    const photo = join(outside, "photo.txt");
    await hook.enter([photo], await centre(row("Archive")));
    await page.locator(".crumb", { hasText: /^Archive$/ }).waitFor({ timeout: 3000 });
    await row("moveme.txt").waitFor();
    await hook.over(await emptySpace());
    await hint.filter({ hasText: "Move to Archive" }).waitFor();
    const result = await hook.drop([photo], await emptySpace());
    assert.deepEqual(result, { effect: "move", names: ["photo.txt"] });
    await page.waitForFunction(() => [...document.querySelectorAll(".tab-panel.active .row.selected .label")].map((l) => l.textContent).join() === "photo.txt");
    assert(onDisk(archive, "photo.txt") && !onDisk(photo));
    await page.keyboard.press("Alt+ArrowUp");
    await row("Archive").waitFor();
  });

  await step("hovering another tab for a second switches to it", async () => {
    await page.keyboard.press("Control+t");
    await page.locator(".tab").nth(1).waitFor();
    await page.locator(".tab").first().click();
    await page.locator(".tab").first().and(page.locator(".tab.active")).waitFor();
    // The second tab shows the inbox too, so the dragged file comes from somewhere else.
    const second = page.locator(".tab").nth(1);
    await hook.enter([join(archive, "moveme.txt")], await centre(second));
    await page.locator(".tab.drop-target").waitFor();
    await page.locator(".tab").nth(1).and(page.locator(".tab.active")).waitFor({ timeout: 3000 });
    await hook.leave();
    await page.keyboard.press("Control+w");
    assert.equal(await page.locator(".tab").count(), 1);
  });

  await step("the list scrolls while the cursor is near its edges", async () => {
    await open(long, "file-00.txt");
    const box = await list.boundingBox();
    const scrollTop = () => list.evaluate((element) => element.scrollTop);
    await hook.enter([join(inbox, "notes.txt")], { x: box.x + box.width / 2, y: box.y + box.height - 8 });
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .list-viewport").scrollTop > 200);
    await hook.over({ x: box.x + box.width / 2, y: box.y + box.height / 2 });
    const stopped = await scrollTop();
    await page.waitForTimeout(250);
    assert.equal(await scrollTop(), stopped, "scrolling stops away from the edges");
    await hook.over({ x: box.x + box.width / 2, y: box.y + 6 });
    await page.waitForFunction((from) => document.querySelector(".tab-panel.active .list-viewport").scrollTop < from - 100, stopped);
    await hook.leave();
  });

  /** Presses on a row and moves the mouse far enough to start a drag, then lets go. */
  async function pressAndDrag(name) {
    const from = await centre(row(name));
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 3, from.y + 1);
    assert.equal(await page.evaluate(() => window.__kestrelDragDrop.lastDragOut()), null, "a tiny move is still a click");
    await page.mouse.move(from.x + 14, from.y + 6, { steps: 3 });
    await page.mouse.up();
    await page.waitForFunction(() => window.__kestrelDragDrop.lastDragOut() !== null);
    return page.evaluate(() => window.__kestrelDragDrop.lastDragOut());
  }

  await step("a press and a short move on the selection starts a drag of all of it", async () => {
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut("prepare"));
    await list.evaluate((element) => void (element.scrollTop = 0));
    await row("file-00.txt").click();
    await row("file-02.txt").click({ modifiers: ["Shift"] });
    assert.deepEqual(await selectedNames(), ["file-00.txt", "file-01.txt", "file-02.txt"]);
    assert.deepEqual(await pressAndDrag("file-01.txt"), { effect: "none", items: 3 });
    assert.deepEqual(await selectedNames(), ["file-00.txt", "file-01.txt", "file-02.txt"], "the drag keeps the selection");
  });

  await step("the shell's drag loop runs on the window's thread and can be called off", async () => {
    // The drop source used here cancels before anything could be dropped.
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut("cancel"));
    assert.deepEqual(await pressAndDrag("file-00.txt"), { effect: "none", items: 3 });
    await page.waitForFunction(() => document.querySelectorAll(".drop-target").length === 0);
    assert.deepEqual(await selectedNames(), ["file-00.txt", "file-01.txt", "file-02.txt"]);
  });

  await step("the window keeps delivering events while a drag is in progress", async () => {
    // Kestrel's own drop handling, when a drag ends in Kestrel, depends on this: the drag loop
    // must not hold up the window's events until it ends. A file created in the open folder
    // during a two-and-a-half-second drag has to appear while the drag still runs.
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut("hold"));
    const from = await centre(row("file-01.txt"));
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 14, from.y + 6, { steps: 3 });
    await page.mouse.up();
    const started = Date.now();
    await page.waitForTimeout(200);
    // Named to sort first, so it appears at the top of the list.
    writeFileSync(join(long, "during-drag.txt"), "x");
    await row("during-drag.txt").waitFor({ timeout: 2000 });
    const seenAfter = Date.now() - started;
    assert.equal(await page.evaluate(() => window.__kestrelDragDrop.lastDragOut()), null, "the drag was still running");
    assert(seenAfter < 2000, `the new file appeared ${seenAfter} ms into the drag`);
    await page.waitForFunction(() => window.__kestrelDragDrop.lastDragOut() !== null, null, { timeout: 5000 });
    assert.deepEqual(await page.evaluate(() => window.__kestrelDragDrop.lastDragOut()), { effect: "none", items: 3 });
    assert.deepEqual(await selectedNames(), ["file-00.txt", "file-01.txt", "file-02.txt"]);
  });

  await step("a click on a selected row without moving selects just that row", async () => {
    await row("file-01.txt").click();
    assert.deepEqual(await selectedNames(), ["file-01.txt"]);
    await page.evaluate(() => window.__kestrelDragDrop.rehearseDragOut(undefined));
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  await page.evaluate(() => {
    window.__kestrelDragDrop?.leave();
    window.__kestrelDragDrop?.keys(undefined);
    window.__kestrelDragDrop?.rehearseDragOut(undefined);
  }).catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
