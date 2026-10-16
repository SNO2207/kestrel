// End-to-end test of the Windows context menu: "Show more options", Shift+F10,
// Shift+right-click and "Open with…".
//
// A script can neither use nor close a real popup menu or the Open With dialog, so this suite
// turns on the app's development hook first: choosing an entry then starts the helper
// (kestrel-shellhost.exe) in `verbs` mode for the same items, which builds the menu without
// showing it, and the app records what it ran. Showing the real menu and dialog are manual
// checks.
//
// Files are created only in a scratch folder under the temp directory, removed afterwards.
// Start the app as described in smoke.mjs, then run: node e2e/shellmenu.mjs

import assert from "node:assert/strict";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-shellmenu-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "Projects"), { recursive: true });
writeFileSync(join(sandbox, "report.txt"), "report");
writeFileSync(join(sandbox, "notes.md"), "notes");
const inSandbox = (name) => join(sandbox, name);

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const menu = page.locator(".menu");
const menuItem = (label) => menu.locator(".menu-item", { hasText: new RegExp(`^${label}`) });
const menuLabels = () => menu.locator(".menu-item > span:first-child").allTextContents();
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`) }) });
const step = (await import("./steps.mjs")).stepper("shellmenu", () => page);

/** Calls a backend command, as the app's own code does. */
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);

/** Waits for the app to start the helper and returns what it recorded. */
async function helperRun() {
  for (let attempt = 0; attempt < 150; attempt++) {
    const record = await invoke("shell_probe_take");
    if (record) return record;
    await page.waitForTimeout(100);
  }
  throw new Error("the helper was not started");
}

/** The value after `flag` in the helper's arguments. */
const argument = (record, flag) => record.args[record.args.indexOf(flag) + 1];
/** Canonical verbs the helper listed, submenus included. */
const verbsOf = (items) =>
  items.flatMap((item) => [...(item.verb ? [item.verb.toLowerCase()] : []), ...verbsOf(item.items ?? [])]);

/** Checks that the helper was asked to show the menu where the user clicked. */
function assertPlacedAt(record, client) {
  const { x, y, width, height, scale } = record.window;
  const at = { x: Number(argument(record, "--x")), y: Number(argument(record, "--y")) };
  const where = `menu at ${JSON.stringify(at)}, window ${JSON.stringify(record.window)}, click ${JSON.stringify(client)}`;
  assert(Math.abs(at.x - (x + client.x * scale)) <= 2 && Math.abs(at.y - (y + client.y * scale)) <= 2, where);
  assert(at.x >= x && at.x <= x + width && at.y >= y && at.y <= y + height, "the menu opens inside the window");
}

/**
 * Saves a screenshot through the debugging protocol: Playwright's own resizes the viewport
 * for an instant, which closes the menu.
 */
async function capture(name) {
  const session = await page.context().newCDPSession(page);
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(data, "base64"));
  await session.detach();
}

/** Right-clicks a spot inside a row and returns the click's position in the page. */
async function rightClickRow(name, modifiers = []) {
  const box = await row(name).boundingBox();
  const client = { x: Math.round(box.x + 40), y: Math.round(box.y + box.height / 2) };
  await page.mouse.move(client.x, client.y);
  for (const key of modifiers) await page.keyboard.down(key);
  await page.mouse.click(client.x, client.y, { button: "right" });
  for (const key of modifiers) await page.keyboard.up(key);
  return client;
}

try {
  await step("the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    for (const name of ["report.txt", "notes.md", "Projects"]) await row(name).waitFor();
    await invoke("shell_probe", { mode: "verbs" });
  });

  await step("a file's menu offers Open with and ends with Show more options", async () => {
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await rightClickRow("report.txt");
      await menu.waitFor();
      const labels = await menuLabels();
      assert.equal(labels.at(-1), "Show more options", labels.join(" | "));
      assert.equal(labels[labels.indexOf("Open") + 1], "Open with…", labels.join(" | "));
      await menuItem("Show more options").locator(".menu-hint", { hasText: "Shift+F10" }).waitFor();
      await capture(`shellmenu-1-item-menu-${scheme}`);
      await page.keyboard.press("Escape");
      await menu.waitFor({ state: "detached" });
    }
    await page.emulateMedia({ colorScheme: null });
  });

  await step("Show more options asks the helper for the file's Windows menu", async () => {
    const client = await rightClickRow("report.txt");
    await menuItem("Show more options").click();
    await menu.waitFor({ state: "detached" });
    const record = await helperRun();
    assert.equal(record.kind, "menu");
    assert.equal(record.error, null);
    assert.deepEqual(record.paths, [inSandbox("report.txt")]);
    assert.equal(record.args[0], "menu");
    assert(!record.args.includes("--extended") && !record.args.includes("--background"), record.args.join(" "));
    assert.match(argument(record, "--owner"), /^\d+$/);
    assertPlacedAt(record, client);
    const verbs = verbsOf(record.items);
    for (const verb of ["open", "copy", "delete", "properties"]) {
      assert(verbs.includes(verb), `${verb} is missing from ${verbs.join(", ")}`);
    }
  });

  await step("the Windows menu matches the window's light or dark scheme", async () => {
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await rightClickRow("report.txt");
      await menuItem("Show more options").click();
      assert.equal(argument(await helperRun(), "--theme"), scheme);
    }
    await page.emulateMedia({ colorScheme: null });
  });

  await step("Open with asks the helper for the file's dialog", async () => {
    await rightClickRow("report.txt");
    await menuItem("Open with").click();
    const record = await helperRun();
    assert.equal(record.kind, "openwith");
    assert.equal(record.error, null);
    const words = record.args.filter((arg) => !/^\d+$/.test(arg));
    assert.deepEqual(words, ["openwith", "--owner", "--", inSandbox("report.txt")]);
  });

  await step("Open with is offered for one file only", async () => {
    await rightClickRow("Projects");
    await menu.waitFor();
    assert(!(await menuLabels()).includes("Open with…"));
    assert.equal((await menuLabels()).at(-1), "Show more options");
    await page.keyboard.press("Escape");
    await row("notes.md").click();
    await row("report.txt").click({ modifiers: ["Control"] });
    await rightClickRow("report.txt");
    await menu.waitFor();
    assert(!(await menuLabels()).includes("Open with…"));
    await page.keyboard.press("Escape");
  });

  await step("several selected items share one Windows menu", async () => {
    await rightClickRow("notes.md");
    await menuItem("Show more options").click();
    const record = await helperRun();
    assert.deepEqual([...record.paths].sort(), [inSandbox("notes.md"), inSandbox("report.txt")].sort());
    assert(verbsOf(record.items).includes("copy"));
  });

  await step("Shift+F10 opens the Windows menu directly", async () => {
    await row("notes.md").click();
    await page.keyboard.press("Shift+F10");
    const record = await helperRun();
    assert.equal(await menu.count(), 0, "Kestrel's own menu stays closed");
    assert.deepEqual(record.paths, [inSandbox("notes.md")]);
    assert(record.args.includes("--extended"), "Shift shows the extended commands, as in Explorer");
    assert(!record.args.includes("--background"));
  });

  await step("Shift+right-click opens the Windows menu directly", async () => {
    const client = await rightClickRow("Projects", ["Shift"]);
    const record = await helperRun();
    assert.equal(await menu.count(), 0, "Kestrel's own menu stays closed");
    assert.deepEqual(record.paths, [inSandbox("Projects")]);
    assert(record.args.includes("--extended"));
    assertPlacedAt(record, client);
  });

  await step("empty space offers the folder's own Windows menu", async () => {
    const box = await list.boundingBox();
    const client = { x: Math.round(box.x + box.width / 2), y: Math.round(box.y + box.height - 30) };
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.mouse.click(client.x, client.y, { button: "right" });
      await menu.waitFor();
      assert.equal((await menuLabels()).at(-1), "Show more options");
      await capture(`shellmenu-2-folder-menu-${scheme}`);
      if (scheme === "light") {
        await page.keyboard.press("Escape");
        await menu.waitFor({ state: "detached" });
      }
    }
    await menuItem("Show more options").click();
    await page.emulateMedia({ colorScheme: null });
    const record = await helperRun();
    assert.deepEqual(record.paths, [sandbox]);
    assert(record.args.includes("--background") && !record.args.includes("--extended"), record.args.join(" "));
    assertPlacedAt(record, client);
    assert(verbsOf(record.items).includes("properties"));

    await page.keyboard.down("Shift");
    await page.mouse.click(client.x, client.y, { button: "right" });
    await page.keyboard.up("Shift");
    const shifted = await helperRun();
    assert.equal(await menu.count(), 0);
    assert(shifted.args.includes("--background") && shifted.args.includes("--extended"), shifted.args.join(" "));
  });

  await step("a crashed helper is reported and Kestrel carries on", async () => {
    await invoke("shell_probe", { mode: "crash" });
    await rightClickRow("report.txt");
    await menuItem("Show more options").click();
    const record = await helperRun();
    assert.match(record.error, /crashed/);
    const notice = page.locator(".notice", { hasText: "crashed" });
    await notice.waitFor();
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await capture(`shellmenu-3-crash-notice-${scheme}`);
    }
    await page.emulateMedia({ colorScheme: null });
    await notice.locator("button", { hasText: "Dismiss" }).click();
    // Still working: the folder lists, and a file created now shows up.
    writeFileSync(inSandbox("after-crash.txt"), "still here");
    await row("after-crash.txt").waitFor({ timeout: 5000 });
  });

  await step("search results from different folders are refused with a notice", async () => {
    await invoke("shell_probe", { mode: "verbs" });
    // The Windows menu comes from one folder, so a selection spanning two cannot have one.
    const token = `shellmenu${process.pid}probe`;
    writeFileSync(inSandbox(`${token}-a.txt`), "a");
    writeFileSync(join(sandbox, "Projects", `${token}-b.txt`), "b");
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    await page.keyboard.press("Control+f");
    // The index picks new files up from the change journal within moments, but a search
    // shows what was indexed when it ran, so it is re-run until both files are found.
    const search = page.locator(".searchbox input");
    const found = async () =>
      (await panel.locator(".row .label").allTextContents()).filter((name) => name.startsWith(token)).length === 2 &&
      /^2 results/.test(await page.locator(".statusbar").innerText());
    for (let attempt = 0; !(await found()); attempt++) {
      assert(attempt < 100, `search for ${token} shows ${await page.locator(".statusbar").innerText()}`);
      await search.fill("");
      await search.fill(token);
      await page.waitForTimeout(300);
    }
    await list.focus();
    await page.keyboard.press("Control+a");
    await page.keyboard.press("Shift+F10");
    const record = await helperRun();
    assert.equal(record.paths.length, 2);
    assert.match(record.error ?? "", /different folders/);
    const notice = page.locator(".notice", { hasText: "different folders" });
    await notice.waitFor();
    for (const scheme of ["light", "dark"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await capture(`shellmenu-4-different-folders-${scheme}`);
    }
    await page.emulateMedia({ colorScheme: null });
    await notice.locator("button", { hasText: "Dismiss" }).click();
    await page.locator(".searchbox input").press("Escape");
    await row("report.txt").waitFor();
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  await invoke("shell_probe", { mode: "off" }).catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
