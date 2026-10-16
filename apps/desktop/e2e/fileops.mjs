// End-to-end test of file operations against the real app window.
//
// Everything happens inside a scratch folder this script creates under the temp directory
// and removes afterwards. Two side effects reach outside it:
//   - copy and cut replace whatever is on the clipboard;
//   - with KESTREL_E2E_RECYCLE=1, one small file is sent to the Recycle Bin.
//
// Start the app as described in smoke.mjs, then run: node e2e/fileops.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";
import { release } from "./slot.mjs";

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
const step = (await import("./steps.mjs")).stepper("fileops", () => page);

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

  await step("a refresh on its way when F2 is pressed doesn't end the rename", async () => {
    await row("beta.txt").click();
    // Both in one go, so the refresh is certain to come back after the edit has begun.
    await page.evaluate(() => {
      document.querySelector('button[aria-label="Refresh"]').click();
      document
        .querySelector(".tab-panel.active .list-viewport")
        .dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true, cancelable: true }));
    });
    const input = panel.locator("input.rename");
    await input.waitFor();
    await page.waitForTimeout(600);
    assert.equal(await input.count(), 1, "the rename box outlives the refresh");
    assert.equal(await input.inputValue(), "beta.txt");
  });

  await step("F2 pressed while the rows are being fetched again still renames", async () => {
    await page.keyboard.press("Escape");
    await panel.locator("input.rename").waitFor({ state: "detached" });
    await row("beta.txt").click();
    // F2 goes in at the moment the refreshed list draws its rows without their details yet. A
    // list that keeps its rows drawn until the new ones are in has no such moment: then F2
    // goes in once the refresh is over.
    const whileLoading = await page.evaluate(
      () =>
        new Promise((resolve) => {
          const list = document.querySelector(".tab-panel.active .list-viewport");
          const press = (loading) => {
            observer.disconnect();
            clearTimeout(timer);
            list.dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true, cancelable: true }));
            resolve(loading);
          };
          const observer = new MutationObserver(() => {
            const rows = [...document.querySelectorAll(".tab-panel.active .row")];
            if (rows.length > 0 && rows.some((row) => !row.hasAttribute("data-path"))) press(true);
          });
          const timer = setTimeout(() => press(false), 2_000);
          observer.observe(list, { subtree: true, childList: true, attributes: true });
          document.querySelector('button[aria-label="Refresh"]').click();
        }),
    );
    const input = panel.locator("input.rename");
    await input.waitFor({ timeout: 5_000 });
    assert.equal(await input.inputValue(), "beta.txt");
    if (!whileLoading) console.log("    (the rows stayed drawn through the refresh, so F2 came after it)");
  });

  await step("a file that arrives during a rename shows once the rename is over", async () => {
    writeFileSync(join(sandbox, "during.txt"), "during");
    await page.waitForTimeout(600);
    assert.equal(await row("during.txt").count(), 0, "the list stays still while a name is edited");
    await page.keyboard.press("Escape");
    await panel.locator("input.rename").waitFor({ state: "detached" });
    await row("during.txt").waitFor({ timeout: 5_000 });
  });

  await step("a refresh already on its way to the list when F2 is pressed is drawn after the rename", async () => {
    await row("beta.txt").click();
    const swapsBefore = await page.evaluate(() => window.__kestrelSwaps.length);
    // The refreshed listing reaches the list and waits there, its rows fetched, so F2 comes
    // before it is drawn every time (a folder change a moment before F2 once did this by chance,
    // and the listing drawn over the edit ended it).
    await page.evaluate(() => window.__kestrelStage.hold(true));
    try {
      await page.locator('button[aria-label="Refresh"]').click();
      await page.waitForFunction(() => window.__kestrelStage.held() > 0);
      await page.evaluate(() =>
        document.querySelector(".tab-panel.active .list-viewport").dispatchEvent(new KeyboardEvent("keydown", { key: "F2", bubbles: true, cancelable: true })),
      );
      await panel.locator("input.rename").waitFor();
    } finally {
      await page.evaluate(() => window.__kestrelStage.hold(false));
    }
    await page.waitForTimeout(400);
    const input = panel.locator("input.rename");
    assert.equal(await input.count(), 1, "the rename box outlives the refreshed listing");
    assert.equal(await input.inputValue(), "beta.txt");
    assert.equal(await page.evaluate(() => window.__kestrelSwaps.length), swapsBefore, "not drawn during the edit");
    await page.keyboard.press("Escape");
    await input.waitFor({ state: "detached" });
    // Drawn once the edit is over.
    await page.waitForFunction((count) => window.__kestrelSwaps.length > count, swapsBefore);
    await row("beta.txt").waitFor();
  });

  await step("copy and paste in place makes a duplicate and selects it", async () => {
    await row("beta.txt").click();
    await page.keyboard.press("Control+c");
    await page.keyboard.press("Control+v");
    await row("beta - Copy.txt").waitFor();
    assert(onDisk("beta - Copy.txt") && onDisk("beta.txt"));
    assert.deepEqual(await selectedNames(), ["beta - Copy.txt"]);
  });

  if (!release) {
    await step("Ctrl+V while the copy before it is still on its way pastes what was copied", async () => {
      // Commands run on a pool of threads, so a paste could be handled before the copy sent just
      // before it had put its files on the clipboard, and paste what was there before. Here the
      // copy is held on its way (`__kestrelClipboard.hold`, development builds), so the paste
      // always comes while it is.
      const clip = join(sandbox, "Clip");
      mkdirSync(clip);
      writeFileSync(join(clip, "earlier.txt"), "earlier");
      writeFileSync(join(clip, "later.txt"), "later");
      await row("Clip").dblclick();
      await page.locator(".crumb").last().filter({ hasText: /^Clip$/ }).waitFor();
      await row("later.txt").waitFor();
      // What the clipboard holds before: earlier.txt.
      await row("earlier.txt").click();
      await page.keyboard.press("Control+c");
      await page.waitForFunction(() => window.__kestrelClipboard.writing() === 0);
      await page.evaluate(() => window.__kestrelClipboard.hold(true));
      try {
        await row("later.txt").click();
        await page.keyboard.press("Control+c");
        await page.waitForFunction(() => window.__kestrelClipboard.writing() === 1);
        await page.keyboard.press("Control+v");
        // Either the paste waits for the copy, or it has gone ahead and pasted earlier.txt.
        await page.waitForFunction(
          () =>
            window.__kestrelClipboard.waiting() === 1 ||
            [...document.querySelectorAll(".tab-panel.active .row .label")].some((label) => label.textContent === "earlier - Copy.txt"),
        );
      } finally {
        await page.evaluate(() => window.__kestrelClipboard.hold(false));
      }
      await panel.locator(".row .label", { hasText: / - Copy\.txt$/ }).first().waitFor();
      assert.deepEqual(readdirSync(clip).sort(), ["earlier.txt", "later - Copy.txt", "later.txt"], "the paste brings what was copied last");
      await row("later - Copy.txt").waitFor();
      assert.deepEqual(await selectedNames(), ["later - Copy.txt"]);
      await page.keyboard.press("Alt+ArrowUp");
      await row("Clip").waitFor();
      await row("gamma.txt").waitFor();
    });

    await step("a paste that ends while the tab is on its way elsewhere doesn't take the tab back", async () => {
      // An operation refreshes its tab when it ends. One that ended while the tab was on its way
      // to another folder (Up pressed just after a paste, in a full run) listed the folder being
      // left again, and the tab went back into it. Here the way elsewhere is held: into a
      // compressed tar, whose reading the backend holds (`archive_dev_hold`), while the paste,
      // held behind its copy, is let go.
      const invoke = (command, args) => page.evaluate(([name, values]) => window.__TAURI_INTERNALS__.invoke(name, values), [command, args]);
      const tar = join(sandbox, "held.tar.gz");
      execFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe"), ["-czf", tar, "-C", sandbox, "during.txt"]);
      await row("held.tar.gz").waitFor();
      const pastedBefore = await page.evaluate(() => window.__kestrelClipboard.pasted());
      await invoke("archive_dev_hold", { path: tar, hold: true });
      await page.evaluate(() => window.__kestrelClipboard.hold(true));
      try {
        await row("during.txt").click();
        await page.keyboard.press("Control+c");
        await page.keyboard.press("Control+v");
        await page.waitForFunction(() => window.__kestrelClipboard.waiting() === 1);
        // On its way into the archive, which waits for its turn to be read.
        await row("held.tar.gz").dblclick();
        await page.evaluate(() => window.__kestrelClipboard.hold(false));
        await page.waitForFunction((before) => window.__kestrelClipboard.pasted() > before, pastedBefore);
        assert(onDisk("during - Copy.txt"), "the paste was made");
      } finally {
        await page.evaluate(() => window.__kestrelClipboard.hold(false));
        await invoke("archive_dev_hold", { path: tar, hold: false });
      }
      await page.locator(".crumb").last().filter({ hasText: /^held\.tar\.gz$/ }).waitFor({ timeout: 10_000 });
      await row("during.txt").waitFor();
      await page.keyboard.press("Alt+ArrowUp");
      await row("held.tar.gz").waitFor();
      await row("during - Copy.txt").waitFor();
    });
  } else {
    console.log("--  Ctrl+V while a copy is on its way, and a paste ending on the way elsewhere, skipped (a release build can't hold them)");
  }

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
    // The header shows the new order at once; the rows follow a moment later, briefly unnamed
    // while they are fetched again in that order. The selection is checked once they are in.
    await page
      .waitForFunction(() => document.querySelector(".tab-panel.active .row.selected .label")?.textContent === "beta.txt", null, { timeout: 5_000 })
      .catch(() => {});
    assert.deepEqual(await selectedNames(), ["beta.txt"]);
    await panel.locator(".column-header.name").click();
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .column-header.name")?.getAttribute("aria-sort") === "ascending");
    // The rows follow the header a moment later; the next step clicks by position.
    await page.waitForFunction(() => {
      const labels = [...document.querySelectorAll(".tab-panel.active .row .label")].map((label) => label.textContent);
      const first = labels.indexOf("aaa-first.txt");
      const later = labels.indexOf("renamed.txt");
      return first >= 0 && later > first;
    });
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
