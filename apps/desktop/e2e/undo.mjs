// End-to-end test of undo and redo against the real app window.
//
// Everything happens inside a scratch folder this script creates under the temp directory and
// removes afterwards; the clipboard is never touched. With KESTREL_E2E_RECYCLE=1 it also undoes
// a new folder and a deletion, which passes two items through the Recycle Bin (both inside a
// folder named kestrel-test-*) and brings both back through the redo and undo being tested.
//
// Start the app (`node e2e/dev.mjs start`), then run: node e2e/undo.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-test-e2e-undo-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(sandbox);
writeFileSync(join(sandbox, "notes.txt"), "notes");
writeFileSync(join(sandbox, "kestrel-test-report.txt"), "report");
const onDisk = (...parts) => existsSync(join(sandbox, ...parts));
const recycling = process.env.KESTREL_E2E_RECYCLE === "1";

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const selectedNames = () => panel.locator(".row.selected .label").allTextContents();
const notice = page.locator(".notice span");
const menu = page.locator(".menu");
const step = (await import("./steps.mjs")).stepper("undo", () => page);

/** Captures the window through the debugging protocol: Playwright's own screenshot briefly
 * resizes the viewport, which closes an open menu. */
async function capture(name) {
  const session = await page.context().newCDPSession(page);
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(data, "base64"));
  await session.detach();
}

/** Takes the same screenshot in the dark and the light theme. */
async function captureBoth(name, prepare = async () => {}) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await prepare();
    await capture(`${name}-${scheme}`);
  }
}

/** Waits for the notice to say `text`. */
const expectNotice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 15_000 });

/** Right-clicks empty space in the list, to the left of where notices appear. */
async function openFolderMenu() {
  const box = await list.boundingBox();
  await page.mouse.click(box.x + 60, box.y + box.height - 30, { button: "right" });
  await menu.waitFor();
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

try {
  await step("the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("notes.txt").waitFor();
  });

  await step("Ctrl+Z undoes a rename, says so and selects the item again", async () => {
    await row("notes.txt").click();
    await page.keyboard.press("F2");
    await panel.locator("input.rename").waitFor();
    await page.keyboard.type("minutes");
    await page.keyboard.press("Enter");
    await row("minutes.txt").waitFor();
    assert(onDisk("minutes.txt") && !onDisk("notes.txt"));

    await list.focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid rename of notes.txt");
    await row("notes.txt").waitFor();
    assert(onDisk("notes.txt") && !onDisk("minutes.txt"));
    assert.equal(readFileSync(join(sandbox, "notes.txt"), "utf8"), "notes");
    await page.waitForFunction(() => document.querySelector(".tab-panel.active .row.selected .label")?.textContent === "notes.txt");
    await captureBoth("undo-1-notice");
  });

  await step("Ctrl+Y redoes it", async () => {
    await list.focus();
    await page.keyboard.press("Control+y");
    await expectNotice("Redid rename of notes.txt");
    await row("minutes.txt").waitFor();
    assert(onDisk("minutes.txt") && !onDisk("notes.txt"));
  });

  await step("the folder menu names the action to undo and redo", async () => {
    await openFolderMenu();
    const undoItem = menu.locator(".menu-item", { hasText: "Undo rename of notes.txt" });
    await undoItem.waitFor();
    assert.equal(await undoItem.isDisabled(), false);
    await captureBoth("undo-2-menu", async () => {
      await page.waitForTimeout(150);
    });
    await undoItem.click();
    await expectNotice("Undid rename of notes.txt");
    await row("notes.txt").waitFor();

    await openFolderMenu();
    const redoItem = menu.locator(".menu-item", { hasText: "Redo rename of notes.txt" });
    await redoItem.waitFor();
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "detached" });
  });

  await step("Ctrl+Z in a text field is left to the field", async () => {
    await page.keyboard.press("Control+l");
    const address = page.locator(".address-input");
    await address.fill(`${sandbox}\\typed`);
    await page.keyboard.press("Control+z");
    await page.waitForTimeout(300);
    assert(onDisk("notes.txt"), "the rename was not redone or undone again");
    await page.keyboard.press("Escape");
  });

  await step("an undo whose folder is gone says so, and older operations can still be undone", async () => {
    await row("notes.txt").click();
    await page.keyboard.press("F2");
    await panel.locator("input.rename").waitFor();
    await page.keyboard.type("agenda");
    await page.keyboard.press("Enter");
    await row("agenda.txt").waitFor();

    // A move, as a drop makes it, from Drafts to Final.
    mkdirSync(join(sandbox, "Drafts"));
    mkdirSync(join(sandbox, "Final"));
    writeFileSync(join(sandbox, "Drafts", "draft.txt"), "draft");
    await row("Drafts").waitFor();
    const dropped = await page.evaluate(
      ({ paths, destination }) => window.__TAURI_INTERNALS__.invoke("drop_items", { paths, destination, keys: { shift: true } }),
      { paths: [join(sandbox, "Drafts", "draft.txt")], destination: join(sandbox, "Final") },
    );
    assert.deepEqual(dropped, { effect: "move", names: ["draft.txt"] });
    // Another program removes the folder the file came from.
    rmSync(join(sandbox, "Drafts"), { recursive: true });
    await row("Drafts").waitFor({ state: "detached" });

    await list.focus();
    await page.keyboard.press("Control+z");
    await expectNotice(`Couldn't undo the move of draft.txt: ${join(sandbox, "Drafts")} no longer exists.`);
    await captureBoth("undo-3-folder-gone");
    assert(onDisk("Final", "draft.txt"), "the file stays where it is");
    await list.focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid rename of notes.txt");
    await row("notes.txt").waitFor();
    assert(onDisk("notes.txt") && !onDisk("agenda.txt"));
  });

  if (recycling) {
    await step("undoing a new folder sends it to the Recycle Bin, and redo brings it back", async () => {
      await list.focus();
      await page.keyboard.press("Control+Shift+N");
      await panel.locator("input.rename").waitFor();
      // Keep the name it was given, so the folder's creation is the operation to undo.
      await page.keyboard.press("Escape");
      await row("New folder").waitFor();

      await list.focus();
      await page.keyboard.press("Control+z");
      await expectNotice("Undid creation of New folder");
      await row("New folder").waitFor({ state: "detached" });
      assert(!onDisk("New folder"));
      assert.equal(recycledFromSandbox(), 1, "the folder is in the Recycle Bin, not erased");

      await page.keyboard.press("Control+y");
      await expectNotice("Redid creation of New folder");
      await row("New folder").waitFor();
      assert(onDisk("New folder"));
      assert.equal(recycledFromSandbox(), 0);
    });

    await step("a new folder another program has saved into is not sent to the Recycle Bin", async () => {
      writeFileSync(join(sandbox, "New folder", "kestrel-test-inside.txt"), "inside");
      await list.focus();
      await page.keyboard.press("Control+z");
      await expectNotice("Couldn't undo the creation of New folder: it has changed since.");
      assert.equal(readFileSync(join(sandbox, "New folder", "kestrel-test-inside.txt"), "utf8"), "inside");
      assert.equal(recycledFromSandbox(), 0, "nothing went to the Recycle Bin");
    });

    await step("undoing a deletion restores exactly that item from the Recycle Bin", async () => {
      await row("kestrel-test-report.txt").click();
      await page.keyboard.press("Delete");
      await row("kestrel-test-report.txt").waitFor({ state: "detached", timeout: 10_000 });
      assert(!onDisk("kestrel-test-report.txt"));

      await list.focus();
      await page.keyboard.press("Control+z");
      await expectNotice("Undid delete of kestrel-test-report.txt");
      await row("kestrel-test-report.txt").waitFor();
      assert.equal(readFileSync(join(sandbox, "kestrel-test-report.txt"), "utf8"), "report");
      // The row can show a moment before the selection: it is found in the listing afterwards.
      await page.waitForFunction(
        () => document.querySelector(".tab-panel.active .row.selected .label")?.textContent === "kestrel-test-report.txt",
        null,
        { timeout: 5_000 },
      );
      assert.deepEqual(await selectedNames(), ["kestrel-test-report.txt"]);
      assert.equal(recycledFromSandbox(), 0);

      // Redo deletes it again, and undo restores it again.
      await page.keyboard.press("Control+y");
      await expectNotice("Redid delete of kestrel-test-report.txt");
      await row("kestrel-test-report.txt").waitFor({ state: "detached" });
      assert.equal(recycledFromSandbox(), 1);
      await page.keyboard.press("Control+z");
      await row("kestrel-test-report.txt").waitFor();
      assert.equal(recycledFromSandbox(), 0, "the Recycle Bin ends as it began");
    });
  } else {
    console.log("--  undoing a new folder and a deletion skipped (set KESTREL_E2E_RECYCLE=1 to run them)");
  }

  console.log("\nall checks passed");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  if (recycling && recycledFromSandbox() > 0) {
    console.error(`!!  items deleted from ${sandbox} are still in the Recycle Bin; restore them from there`);
  }
  rmSync(sandbox, { recursive: true, force: true });
}
