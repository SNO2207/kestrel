// End-to-end test of file operations against the real app window.
//
// Everything happens inside a scratch folder this script creates under the temp directory
// and removes afterwards. Two side effects reach outside it:
//   - copy and cut replace whatever is on the clipboard;
//   - with KESTREL_E2E_RECYCLE=1, one small file is sent to the Recycle Bin.
//
// Start the app as described in smoke.mjs, then run: node e2e/fileops.mjs

import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(sandbox);
writeFileSync(join(sandbox, "alpha.txt"), "alpha");
writeFileSync(join(sandbox, "beta.txt"), "beta");
const onDisk = (...parts) => existsSync(join(sandbox, ...parts));

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`) }) });
const selectedNames = () => panel.locator(".row.selected .label").allTextContents();
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

try {
  await step("the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("alpha.txt").waitFor();
    await row("beta.txt").waitFor();
  });

  await step("a file created by another program appears by itself", async () => {
    writeFileSync(join(sandbox, "gamma.txt"), "gamma");
    await row("gamma.txt").waitFor({ timeout: 5000 });
  });

  await step("Ctrl+Shift+N creates a folder and starts renaming it", async () => {
    await list.focus();
    await page.keyboard.press("Control+Shift+N");
    const input = panel.locator("input.rename");
    await input.waitFor();
    assert.equal(await input.inputValue(), "New folder");
    await page.keyboard.type("Projects");
    await page.keyboard.press("Enter");
    await row("Projects").waitFor();
    assert(onDisk("Projects") && !onDisk("New folder"));
    assert.deepEqual(await selectedNames(), ["Projects"]);
  });

  await step("F2 renames a file, keeping its extension", async () => {
    await row("alpha.txt").click();
    await page.keyboard.press("F2");
    await panel.locator("input.rename").waitFor();
    // Only the stem is pre-selected, so typing replaces "alpha" and leaves ".txt".
    await page.keyboard.type("renamed");
    await page.keyboard.press("Enter");
    await row("renamed.txt").waitFor();
    assert(onDisk("renamed.txt") && !onDisk("alpha.txt"));
    assert.equal(readFileSync(join(sandbox, "renamed.txt"), "utf8"), "alpha");
  });

  await step("Escape cancels a rename", async () => {
    await row("beta.txt").click();
    await page.keyboard.press("F2");
    await panel.locator("input.rename").waitFor();
    await page.keyboard.type("discarded");
    await page.keyboard.press("Escape");
    await panel.locator("input.rename").waitFor({ state: "detached" });
    assert(onDisk("beta.txt") && !onDisk("discarded.txt"));
  });

  await step("copy and paste in place makes a duplicate and selects it", async () => {
    await row("beta.txt").click();
    await page.keyboard.press("Control+c");
    await page.keyboard.press("Control+v");
    await row("beta - Copy.txt").waitFor();
    assert(onDisk("beta - Copy.txt") && onDisk("beta.txt"));
    assert.deepEqual(await selectedNames(), ["beta - Copy.txt"]);
  });

  await step("cut and paste moves a file into a folder", async () => {
    await row("gamma.txt").click();
    await page.keyboard.press("Control+x");
    await row("Projects").dblclick();
    await page.locator(".crumb", { hasText: "Projects" }).waitFor();
    await panel.locator(".list-empty", { hasText: "This folder is empty" }).waitFor();
    await list.focus();
    await page.keyboard.press("Control+v");
    await row("gamma.txt").waitFor();
    assert(onDisk("Projects", "gamma.txt") && !onDisk("gamma.txt"));
    await page.keyboard.press("Alt+ArrowUp");
    await row("Projects").waitFor();
    await row("gamma.txt").waitFor({ state: "detached" });
  });

  await step("the selection stays on its file when the folder changes", async () => {
    await row("beta.txt").click();
    writeFileSync(join(sandbox, "aaa-first.txt"), "x");
    await row("aaa-first.txt").waitFor({ timeout: 5000 });
    assert.deepEqual(await selectedNames(), ["beta.txt"]);
  });

  await step("sorting keeps the selection on its file", async () => {
    await panel.locator(".column-header.name").click();
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .column-header.name")?.getAttribute("aria-sort") === "descending");
    assert.deepEqual(await selectedNames(), ["beta.txt"]);
    await panel.locator(".column-header.name").click();
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .column-header.name")?.getAttribute("aria-sort") === "ascending");
  });

  await step("right-click opens a menu for the item, and Escape closes it", async () => {
    await row("renamed.txt").click({ button: "right" });
    const menu = page.locator(".menu");
    await menu.waitFor();
    assert.deepEqual(await selectedNames(), ["renamed.txt"]);
    for (const label of ["Open", "Cut", "Copy", "Rename", "Delete", "Properties"]) {
      await menu.locator(".menu-item", { hasText: new RegExp(`^${label}`) }).first().waitFor();
    }
    // Captured through the debugging protocol directly: Playwright's own screenshot resizes
    // the viewport for an instant, and a resize closes the menu.
    const session = await page.context().newCDPSession(page);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, "7-context-menu.png"), Buffer.from(data, "base64"));
    await session.detach();
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });
  });

  await step("right-clicking empty space offers folder actions", async () => {
    const box = await list.boundingBox();
    await page.mouse.click(box.x + box.width / 2, box.y + box.height - 30, { button: "right" });
    const menu = page.locator(".menu");
    await menu.locator(".menu-item", { hasText: "New folder" }).waitFor();
    await menu.locator(".menu-item", { hasText: "Paste" }).waitFor();
    await page.keyboard.press("Escape");
  });

  if (process.env.KESTREL_E2E_RECYCLE === "1") {
    await step("Delete sends a file to the Recycle Bin", async () => {
      await row("beta - Copy.txt").click();
      await page.keyboard.press("Delete");
      await row("beta - Copy.txt").waitFor({ state: "detached", timeout: 10_000 });
      assert(!onDisk("beta - Copy.txt"));
    });
  } else {
    console.log("--  Delete to the Recycle Bin skipped (set KESTREL_E2E_RECYCLE=1 to run it)");
  }

  console.log("\nall checks passed");
} finally {
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
