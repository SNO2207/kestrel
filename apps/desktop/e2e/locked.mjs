// End-to-end test of locked results: items in, or being, a folder this account can't open show
// dimmed with a lock and say why instead of failing when opened.
//
// Works in a scratch folder under the temp directory, where it makes one folder unreadable by
// removing its permissions (icacls /inheritance:r leaves it with an empty list, which refuses
// everyone but still lets its owner change it), then gives them back and removes everything.
//
// Start the app (`node e2e/dev.mjs start`), then run: node e2e/locked.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

const sandbox = join(tmpdir(), `kestrel-e2e-locked-${process.pid}`);
const privateFolder = join(sandbox, "private");
const probe = `kestrel-locked-probe-${process.pid}.txt`;
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "open"), { recursive: true });
mkdirSync(privateFolder);
writeFileSync(join(sandbox, "readme.txt"), "readme");
writeFileSync(join(privateFolder, probe), "secret");

const icacls = (...args) => execFileSync("icacls", [privateFolder, ...args], { stdio: "ignore" });
const lock = () => icacls("/inheritance:r");
const unlock = () => icacls("/reset");

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const search = page.locator(".searchbox input");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const message = "Your account can't open this folder";
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** Searches for the probe file, again and again until the index has caught up with it. */
async function findProbe(timeout = 20_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    await search.fill("");
    await search.fill(probe);
    if (await row(probe).waitFor({ timeout: 500 }).then(() => true, () => false)) return;
  }
  throw new Error(`${probe} never showed up in search`);
}

try {
  await step("the index knows the file before its folder is locked", async () => {
    await panel.locator(".row").first().waitFor();
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    if (!(await page.locator(".scope.on").count())) await page.locator(".scope").click();
    await findProbe();
    assert.equal(await row(probe).getAttribute("class").then((c) => c.includes("locked")), false);
    await search.fill("");
  });

  await step("a folder this account can't open shows dimmed with a lock", async () => {
    lock();
    assert.throws(() => readdirSync(privateFolder), /EPERM|EACCES/);
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("private").waitFor();
    // Answers are remembered per folder, and this one was asked about before it was locked;
    // refreshing asks again, as a user would after changing permissions.
    await page.keyboard.press("F5");
    await row("private").locator(".lock-glyph").waitFor();
    assert.equal(await row("private").getAttribute("title"), message);
    assert.match(await row("private").getAttribute("class"), /\bdimmed\b/);
    assert.equal(await row("open").locator(".lock-glyph").count(), 0, "a folder that opens has no lock");
    assert.equal(await row("readme.txt").getAttribute("title"), null);
    for (const scheme of ["dark", "light"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.screenshot({ path: join(out, `locked-1-folder-${scheme}.png`) });
    }
    await page.emulateMedia({ colorScheme: null });
  });

  await step("opening it says why instead of failing", async () => {
    await row("private").dblclick();
    await page.locator(".notice span", { hasText: message }).waitFor();
    await page.locator(".crumb", { hasText: `kestrel-e2e-locked-${process.pid}` }).waitFor();
    assert.equal(await panel.locator(".list-error").count(), 0);
  });

  await step("typing the folder's address says why too", async () => {
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(privateFolder);
    await page.keyboard.press("Enter");
    await panel.locator(".list-error", { hasText: message }).waitFor();
    // The tab is still in the sandbox, so going back leaves the message for the sandbox, not
    // for the folder before it. The rows behind the message are the sandbox's either way, so
    // the message going away is what tells the two apart.
    await page.keyboard.press("Alt+ArrowLeft");
    await panel.locator(".list-error").waitFor({ state: "detached" });
    await row("private").waitFor();
    assert.equal(await page.locator(".tab.active").getAttribute("title"), sandbox);
    await page.waitForTimeout(300);
    assert.equal(await page.locator(".tab.active").getAttribute("title"), sandbox, "and stays there");
  });

  await step("a search result inside it shows the lock, and opening it says why", async () => {
    await findProbe();
    await row(probe).locator(".lock-glyph").waitFor();
    assert.equal(await row(probe).getAttribute("title"), message);
    for (const scheme of ["dark", "light"]) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.screenshot({ path: join(out, `locked-2-search-${scheme}.png`) });
    }
    await page.emulateMedia({ colorScheme: null });
    await row(probe).dblclick();
    await page.locator(".notice span", { hasText: message }).waitFor();
  });

  console.log("\nall checks passed");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await search.fill("").catch(() => {});
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  try {
    unlock();
  } catch {
    // Already open again.
  }
  rmSync(sandbox, { recursive: true, force: true });
}
