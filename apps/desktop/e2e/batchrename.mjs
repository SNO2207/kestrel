// End-to-end test of batch rename against the real app window: the dialog, its live preview
// with conflicts and invalid names, applying, and undoing the whole batch at once.
//
// Everything happens inside a scratch folder this script creates under the temp directory and
// removes afterwards. Nothing goes to the Recycle Bin and the clipboard is never touched.
//
// Start the app (`node e2e/dev.mjs start`), then run: node e2e/batchrename.mjs

import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-batch-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(sandbox);
const originals = ["beach.jpg", "city.jpg", "forest.jpg"];
for (const name of originals) writeFileSync(join(sandbox, name), name);
// Not selected: renaming onto it must be flagged.
writeFileSync(join(sandbox, "Photo 002.jpg"), "already here");
const onDisk = () => readdirSync(sandbox).sort();

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const dialog = page.locator(".rename-dialog");
const field = (label) => dialog.locator(".dialog-field", { hasText: label }).locator("input, select");
const summary = dialog.locator(".rename-summary");
const renameButton = dialog.locator(".dialog-button.primary");
const newNames = () => dialog.locator(".rename-new-name").allTextContents();
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** Waits until the preview's new names are exactly `names`. */
async function expectPreview(names) {
  await page.waitForFunction(
    (expected) =>
      JSON.stringify([...document.querySelectorAll(".rename-dialog .rename-new-name")].map((n) => n.textContent)) === expected,
    JSON.stringify(names),
  );
}

/** Waits until exactly `names` are selected. */
async function expectSelected(names) {
  await page.waitForFunction(
    (expected) =>
      JSON.stringify([...document.querySelectorAll(".tab-panel.active .row.selected .label")].map((l) => l.textContent).sort()) ===
      expected,
    JSON.stringify([...names].sort()),
  );
}

async function selectOriginals() {
  await row("beach.jpg").click();
  await row("forest.jpg").click({ modifiers: ["Shift"] });
  await expectSelected(originals);
}

try {
  await step("the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("beach.jpg").waitFor();
  });

  await step("F2 with several items selected opens the dialog with a preview", async () => {
    await selectOriginals();
    await page.keyboard.press("F2");
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "Rename 3 items");
    await expectPreview(originals);
    assert.deepEqual(await dialog.locator(".rename-old").allTextContents(), originals);
    await summary.filter({ hasText: "No names change yet." }).waitFor();
    assert(await renameButton.isDisabled());
    assert.equal(await page.evaluate(() => document.activeElement?.closest(".dialog-field")?.textContent?.trim().startsWith("Find")), true);
  });

  await step("numbering previews live and flags a name that is taken", async () => {
    await field("Numbering").fill("Photo ###");
    await expectPreview(["Photo 001.jpg", "Photo 002.jpg", "Photo 003.jpg"]);
    await dialog.locator(".rename-preview-row.problem .rename-problem", { hasText: "Photo 002.jpg already exists." }).waitFor();
    await summary.filter({ hasText: "1 new name can't be used." }).waitFor();
    assert(await renameButton.isDisabled());
    for (const scheme of ["dark", "light"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.screenshot({ path: join(out, `batchrename-1-conflict-${scheme}.png`) });
    }
    await page.emulateMedia({ colorScheme: null });
  });

  await step("a start value moves the numbers clear of the conflict", async () => {
    await field("Start at").fill("10");
    await expectPreview(["Photo 010.jpg", "Photo 011.jpg", "Photo 012.jpg"]);
    await summary.filter({ hasText: "3 of 3 names change." }).waitFor();
    assert(await renameButton.isEnabled());
  });

  await step("an invalid character is flagged", async () => {
    await field("Suffix").fill("?");
    await dialog.locator(".rename-problem", { hasText: "A name can't contain ?" }).first().waitFor();
    await summary.filter({ hasText: "3 new names can't be used." }).waitFor();
    assert(await renameButton.isDisabled());
    await field("Suffix").fill("");
  });

  await step("prefix and case changes combine with numbering", async () => {
    await field("Prefix").fill("trip ");
    await field("Case").selectOption("title");
    await expectPreview(["Trip Photo 010.jpg", "Trip Photo 011.jpg", "Trip Photo 012.jpg"]);
  });

  await step("Rename applies every name and selects the renamed items", async () => {
    await renameButton.click();
    await dialog.waitFor({ state: "detached" });
    await page.locator(".notice span", { hasText: "Renamed 3 items" }).waitFor();
    await row("Trip Photo 012.jpg").waitFor();
    assert.deepEqual(onDisk(), ["Photo 002.jpg", "Trip Photo 010.jpg", "Trip Photo 011.jpg", "Trip Photo 012.jpg"]);
    assert.equal(readFileSync(join(sandbox, "Trip Photo 011.jpg"), "utf8"), "city.jpg");
    await expectSelected(["Trip Photo 010.jpg", "Trip Photo 011.jpg", "Trip Photo 012.jpg"]);
  });

  await step("one Ctrl+Z undoes the whole batch and selects the items again", async () => {
    await list.focus();
    await page.keyboard.press("Control+z");
    await page.locator(".notice span", { hasText: "Undid rename of 3 items" }).waitFor({ timeout: 15_000 });
    await row("beach.jpg").waitFor();
    assert.deepEqual(onDisk(), ["Photo 002.jpg", ...originals]);
    assert.equal(readFileSync(join(sandbox, "city.jpg"), "utf8"), "city.jpg");
    await expectSelected(originals);
  });

  await step("regular expressions replace with groups, on the extension too", async () => {
    await selectOriginals();
    await page.keyboard.press("F2");
    await dialog.waitFor();
    await dialog.locator(".dialog-check", { hasText: "Regular expression" }).locator("input").check();
    await field("Find").fill("^(\\w)(\\w*)$");
    await field("Replace with").fill("$2-$1");
    await expectPreview(["each-b.jpg", "ity-c.jpg", "orest-f.jpg"]);
    // An unclosed group is explained rather than applied.
    await field("Find").fill("(");
    await summary.filter({ hasText: "The pattern isn't valid" }).waitFor();
    assert(await renameButton.isDisabled());
    await field("Find").fill("");
    await field("Case").selectOption("upper");
    await field("Apply to").selectOption("extension");
    await expectPreview(["beach.JPG", "city.JPG", "forest.JPG"]);
    await page.keyboard.press("Enter");
    await dialog.waitFor({ state: "detached" });
    await row("forest.JPG").waitFor();
    assert.deepEqual(onDisk(), ["Photo 002.jpg", "beach.JPG", "city.JPG", "forest.JPG"]);
    await list.focus();
    await page.keyboard.press("Control+z");
    await row("forest.jpg").waitFor();
    assert.deepEqual(onDisk(), ["Photo 002.jpg", ...originals]);
    await expectSelected(originals);
  });

  await step("the keyboard stays inside the dialog, and Escape closes it without renaming anything", async () => {
    await selectOriginals();
    await page.keyboard.press("F2");
    await dialog.waitFor();
    const focusInDialog = () => page.evaluate(() => document.activeElement?.closest(".rename-dialog") !== null);
    await page.keyboard.press("Shift+Tab");
    assert.equal(await page.evaluate(() => document.activeElement?.textContent), "Cancel", "Shift+Tab from the first field wraps to the last button");
    await page.keyboard.press("Tab");
    assert(await focusInDialog());
    await page.keyboard.press("Delete");
    assert.deepEqual(onDisk(), ["Photo 002.jpg", ...originals], "keys never reach the list behind");
    await field("Prefix").fill("discarded ");
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert.deepEqual(onDisk(), ["Photo 002.jpg", ...originals]);
  });

  await step("the item menu offers to rename the selection", async () => {
    await selectOriginals();
    await row("city.jpg").click({ button: "right" });
    const item = page.locator(".menu .menu-item", { hasText: "Rename 3 items" });
    await item.waitFor();
    await item.click();
    await dialog.waitFor();
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });
    assert(existsSync(join(sandbox, "city.jpg")));
  });

  console.log("\nall checks passed");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
