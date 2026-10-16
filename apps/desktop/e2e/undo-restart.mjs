// End-to-end test of an undo history that survives a restart: operations done in one run of
// the app are undone and redone in the next, and one whose file another program deleted while
// the app was closed is left out without a word.
//
// The suite restarts the slot's app itself (`e2e/dev.mjs stop`, then `start`), so run it on its
// own slot, with the same KESTREL_SLOT (and KESTREL_RELEASE) as the running instance. Everything
// happens inside a scratch folder under the temp directory, removed afterwards; the clipboard
// and the Recycle Bin are never touched. Screenshots go to e2e/out.
//
//   KESTREL_SLOT=<n> node e2e/dev.mjs start
//   KESTREL_SLOT=<n> node e2e/undo-restart.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-undo-restart-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(sandbox);
for (const name of ["one.txt", "two.txt", "three.txt"]) writeFileSync(join(sandbox, name), name);
const onDisk = (name) => existsSync(join(sandbox, name));

const step = (await import("./steps.mjs")).stepper("undo-restart", () => page);

let browser;
let page;

/** Connects to the slot's window and opens the scratch folder in it. */
async function attach() {
  ({ browser, page } = await connectToKestrel());
  const panel = page.locator(".tab-panel.active");
  await panel.locator(".row").first().waitFor({ timeout: 60_000 });
  // The suites expect the preview pane closed; a restart can bring back a stale setting.
  if (await page.locator(".preview-pane").isVisible()) await page.keyboard.press("Alt+p");
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(sandbox);
  await page.keyboard.press("Enter");
  await page.locator(".crumb", { hasText: /^kestrel-e2e-undo-restart-/ }).last().waitFor();
}

const panel = () => page.locator(".tab-panel.active");
const list = () => panel().locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel().locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const expectNotice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 15_000 });
const undoState = () => page.evaluate(() => window.__TAURI_INTERNALS__.invoke("undo_state"));

/** Renames a file through the app, as a user would: F2, a new name, Enter. */
async function rename(from, to) {
  await row(`${from}.txt`).click();
  await page.keyboard.press("F2");
  await panel().locator("input.rename").waitFor();
  await page.keyboard.type(to);
  await page.keyboard.press("Enter");
  await row(`${to}.txt`).waitFor();
}

/** Stops the slot's app and starts it again, as `e2e/dev.mjs` does from the command line. */
function restart() {
  const dev = join(here, "dev.mjs");
  for (const command of ["stop", "start"]) {
    execFileSync(process.execPath, [dev, command], { stdio: ["ignore", "inherit", "inherit"], env: process.env });
  }
}

/** Captures the window in the dark and the light theme. */
async function captureBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    const session = await page.context().newCDPSession(page);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
    await session.detach();
  }
  await page.emulateMedia({ colorScheme: null });
}

try {
  await step("three renames, the last one undone, in one run of the app", async () => {
    await attach();
    await row("one.txt").waitFor();
    await rename("one", "uno");
    await rename("two", "dos");
    await rename("three", "tres");
    await list().focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid rename of three.txt");
    await row("three.txt").waitFor();
    assert.deepEqual(await undoState(), { undo: "rename of two.txt", redo: "rename of three.txt", erased: null });
  });

  await step("the app is closed, and another program deletes one of the renamed files", async () => {
    // Leave the window somewhere that will still exist.
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill("C:\\");
    await page.keyboard.press("Enter");
    await browser.close();
    execFileSync(process.execPath, [join(here, "dev.mjs"), "stop"], { stdio: ["ignore", "inherit", "inherit"], env: process.env });
    rmSync(join(sandbox, "dos.txt"));
    assert(onDisk("uno.txt") && onDisk("three.txt") && !onDisk("dos.txt"));
  });

  await step("after the restart, undo and redo pick up where the last run left them", async () => {
    restart();
    await attach();
    await row("uno.txt").waitFor();
    // The rename of two.txt can't be undone any more: its file is gone. Nothing says so.
    await page.waitForFunction(
      () => window.__TAURI_INTERNALS__.invoke("undo_state").then((state) => state.undo === "rename of one.txt"),
      null,
      { timeout: 15_000, polling: 250 },
    );
    assert.deepEqual(await undoState(), { undo: "rename of one.txt", redo: "rename of three.txt", erased: null });
    assert.equal(await page.locator(".notice").count(), 0, "no message about what was left out");
    await captureBoth("undo-restart-1-after");

    await list().focus();
    await page.keyboard.press("Control+y");
    await expectNotice("Redid rename of three.txt");
    await row("tres.txt").waitFor();
    await list().focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid rename of three.txt");
    await row("three.txt").waitFor();
    await list().focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid rename of one.txt");
    await row("one.txt").waitFor();
    assert(onDisk("one.txt") && onDisk("three.txt") && !onDisk("uno.txt") && !onDisk("tres.txt"));
    assert.equal(readFileSync(join(sandbox, "one.txt"), "utf8"), "one.txt");
    await captureBoth("undo-restart-2-undone");
  });

  await step("the folder menu offers the restored history too", async () => {
    const box = await list().boundingBox();
    await page.mouse.click(box.x + 60, box.y + box.height - 30, { button: "right" });
    const menu = page.locator(".menu");
    await menu.locator(".menu-item", { hasText: "Redo rename of one.txt" }).waitFor();
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });
  });

  console.log("\nall checks passed");
} finally {
  if (page) {
    await page.emulateMedia({ colorScheme: null }).catch(() => {});
    await page.keyboard.press("Control+l").catch(() => {});
    await page.locator(".address-input").fill("C:\\").catch(() => {});
    await page.keyboard.press("Enter").catch(() => {});
    await page.waitForTimeout(300).catch(() => {});
  }
  await browser?.close().catch(() => {});
  rmSync(sandbox, { recursive: true, force: true });
}
