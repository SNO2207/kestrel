// End-to-end test of the live index: files created, renamed and deleted by another program
// show up in search results without any refresh, because the app follows the drive's change
// journal. Works in a scratch folder under %TEMP% that it removes afterwards.
//
// Start the app as described in smoke.mjs, then run: node e2e/live-index.mjs

import assert from "node:assert/strict";
import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connectToKestrel } from "./connect.mjs";

const tag = `kestrelprobe${Date.now()}`;
const sandbox = join(tmpdir(), `kestrel-e2e-live-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "nested", "deeper"), { recursive: true });

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const search = page.locator(".searchbox input");
const step = async (title, run) => {
  const started = Date.now();
  await run();
  console.log(`ok  ${title} (${Date.now() - started} ms)`);
};

/** Re-runs the search until `names` is exactly what it finds, or fails after `timeout`. */
async function expectResults(query, names, timeout = 5000) {
  const deadline = Date.now() + timeout;
  let found = [];
  while (Date.now() < deadline) {
    await search.fill("");
    await search.fill(query);
    await page.waitForTimeout(150);
    found = (await panel.locator(".row .label").allTextContents()).sort();
    if (JSON.stringify(found) === JSON.stringify([...names].sort())) return;
    await page.waitForTimeout(150);
  }
  assert.deepEqual(found, [...names].sort(), `searching for ${query}`);
}

try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
  if (!(await page.locator(".scope.on").count())) await page.locator(".scope").click();

  await step("a file created by another program becomes searchable", async () => {
    writeFileSync(join(sandbox, "nested", "deeper", `${tag}-report.txt`), "hello");
    await expectResults(tag, [`${tag}-report.txt`]);
  });

  await step("a renamed file is found under its new name only", async () => {
    renameSync(join(sandbox, "nested", "deeper", `${tag}-report.txt`), join(sandbox, "nested", "deeper", `${tag}-final.txt`));
    await expectResults(tag, [`${tag}-final.txt`]);
  });

  await step("a folder moved elsewhere keeps its contents searchable", async () => {
    mkdirSync(join(sandbox, "elsewhere"));
    renameSync(join(sandbox, "nested"), join(sandbox, "elsewhere", "nested"));
    await expectResults(tag, [`${tag}-final.txt`]);
    await search.fill(`${tag} in:${sandbox}\\elsewhere`);
    await panel.locator(".row .label", { hasText: `${tag}-final.txt` }).waitFor({ timeout: 3000 });
  });

  await step("a deleted file disappears from results", async () => {
    rmSync(join(sandbox, "elsewhere", "nested", "deeper", `${tag}-final.txt`));
    await expectResults(tag, []);
  });

  console.log("\nall checks passed");
} finally {
  await search.fill("").catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
