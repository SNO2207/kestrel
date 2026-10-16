// End-to-end smoke test against the real app window.
//
// Start the app with the web view's debugging port open, then run this script:
//
//   $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9333"
//   pnpm tauri dev
//   node e2e/smoke.mjs
//
// It only reads the file system: it navigates, searches and sorts, and never modifies files.

import assert from "node:assert/strict";
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const { browser, page } = await connectToKestrel();
// Start from a clean front end so the test does not depend on what the window was showing.
await page.reload();

const panel = page.locator(".tab-panel.active");
const rows = panel.locator(".row");
const names = () => panel.locator(".row .label").allTextContents();
const status = () => page.locator(".statusbar").innerText();
const shot = (name) => page.screenshot({ path: join(out, `${name}.png`) });
const step = (await import("./steps.mjs")).stepper("smoke", () => page);

await step("the home folder is listed", async () => {
  await rows.first().waitFor();
  assert((await rows.count()) > 3);
  assert.match(await status(), /\d+ items/);
  await shot("1-home");
});

await step("the index is loaded", async () => {
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
});

await step("typing an address navigates there", async () => {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill("C:\\Windows");
  await page.keyboard.press("Enter");
  await page.locator(".crumb", { hasText: "Windows" }).waitFor();
  await page.keyboard.type("system32");
  await panel.locator(".row.focused .label", { hasText: /^System32$/ }).waitFor();
});

await step("folders carry sizes and sorting by size puts the largest first", async () => {
  await panel.locator(".column-header.size").click();
  await page.waitForFunction(() => {
    const first = document.querySelector(".tab-panel.active .row .amount");
    return first && /GB/.test(first.textContent ?? "");
  });
  await shot("2-windows-by-size");
  await panel.locator(".column-header.name").click();
});

await step("back and up work", async () => {
  await page.keyboard.press("Alt+ArrowUp");
  await page.locator(".tab.active .tab-title", { hasText: "C:" }).waitFor();
  await page.keyboard.press("Alt+ArrowLeft");
  await page.locator(".tab.active .tab-title", { hasText: "Windows" }).waitFor();
});

await step("searching everywhere returns results as you type", async () => {
  await page.keyboard.press("Control+f");
  await page.keyboard.type("notepad.exe", { delay: 15 });
  await panel.locator(".row .label", { hasText: /^notepad\.exe$/i }).first().waitFor();
  assert.match(await status(), /results?/);
  assert((await panel.locator(".cell.location").count()) > 0, "search rows show their folder");
  await shot("3-search");
});

await step("filters narrow a search", async () => {
  const before = await status();
  await page.locator(".searchbox input").fill("ext:exe size:>100mb");
  await page.waitForFunction((old) => document.querySelector(".statusbar")?.innerText !== old, before);
  await panel.locator(".row .label").first().waitFor();
  const found = await names();
  assert(found.length > 0 && found.every((name) => name.toLowerCase().endsWith(".exe")), found.join(", "));
  await shot("4-filters");
});

await step("a query with no matches explains itself", async () => {
  await page.locator(".searchbox input").fill("zzz-no-such-file-qqq");
  await panel.locator(".list-empty", { hasText: "Nothing matches" }).waitFor();
});

await step("clearing the search returns to the folder", async () => {
  await page.locator(".searchbox input").press("Escape");
  await panel.locator(".row .cell.type").first().waitFor();
  assert.match(await status(), /items/);
});

await step("the keyboard selects and opens folders", async () => {
  await panel.locator(".list-viewport").focus();
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+ArrowDown");
  await page.keyboard.press("Shift+ArrowDown");
  assert.match(await status(), /3 selected/);
  await page.keyboard.type("sys");
  await panel.locator(".row.focused .label", { hasText: /^sys/i }).waitFor();
  await page.keyboard.press("Control+a");
  assert.match(await status(), /\d+ selected/);
});

await step("tabs open and close", async () => {
  await page.keyboard.press("Control+t");
  await page.locator(".tab").nth(1).waitFor();
  assert.equal(await page.locator(".tab").count(), 2);
  await shot("5-two-tabs");
  await page.keyboard.press("Control+w");
  assert.equal(await page.locator(".tab").count(), 1);
});

await step("a huge result set scrolls to its end", async () => {
  await page.locator(".searchbox input").fill("e");
  await page.waitForFunction(() => /[\d,.\s]{7,} results/.test(document.querySelector(".statusbar")?.textContent ?? ""));
  await panel.locator(".list-viewport").focus();
  await page.keyboard.press("End");
  await page.waitForFunction(() => {
    const rows = document.querySelectorAll(".tab-panel.active .row");
    const total = Number(document.querySelector(".tab-panel.active .list-viewport")?.getAttribute("aria-rowcount"));
    return rows.length > 0 && Number(rows[rows.length - 1].getAttribute("aria-rowindex")) === total;
  });
  await rows.last().locator(".label").waitFor();
  await shot("6-end-of-millions");
  await page.locator(".searchbox input").press("Escape");
});

console.log("\nall checks passed; screenshots are in", out);
await browser.close();
