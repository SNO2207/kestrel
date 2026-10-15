// End-to-end test of the live index: files created, renamed and deleted by another program
// show up in search results without any refresh, because the app follows the drive's change
// journal. Works in a scratch folder under %TEMP% that it removes afterwards.
//
// Start the app as described in smoke.mjs, then run: node e2e/live-index.mjs

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connectToKestrel } from "./connect.mjs";

const tag = `kestrelprobe${Date.now()}`;
const sandbox = join(tmpdir(), `kestrel-e2e-live-${process.pid}`);
const junction = join(sandbox, `${tag}-link`);
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

/**
 * Re-runs the search until `names` is exactly what it finds, or fails after `timeout`. No
 * results count only once the list says so for this very query, so rows still on screen from
 * an earlier search, or a list not yet filled, are never taken for the answer.
 */
async function expectResults(query, names, timeout = 5000) {
  const deadline = Date.now() + timeout;
  let found = [];
  while (Date.now() < deadline) {
    await search.fill("");
    await search.fill(query);
    await page.waitForTimeout(150);
    found = (await panel.locator(".row .label").allTextContents()).sort();
    const settled = names.length > 0 || (await panel.locator(".list-empty", { hasText: `Nothing matches “${query}”` }).count()) > 0;
    if (settled && JSON.stringify(found) === JSON.stringify([...names].sort())) return;
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
    // Only the new place counts: the plain search above also passes before the move is seen.
    await expectResults(`${tag} in:${join(sandbox, "elsewhere")}`, [`${tag}-final.txt`]);
  });

  await step("a deleted file disappears from results", async () => {
    rmSync(join(sandbox, "elsewhere", "nested", "deeper", `${tag}-final.txt`));
    await expectResults(tag, []);
  });

  await step("a file deleted just after its folder moved disappears too", async () => {
    // Both changes land in one read of the journal, which once left the file in the index: its
    // folder was re-read at the path it had before the move.
    mkdirSync(join(sandbox, "moving", "inner"), { recursive: true });
    writeFileSync(join(sandbox, "moving", "inner", `${tag}-late.txt`), "late");
    await expectResults(tag, [`${tag}-late.txt`]);
    renameSync(join(sandbox, "moving"), join(sandbox, "elsewhere", "moving"));
    rmSync(join(sandbox, "elsewhere", "moving", "inner", `${tag}-late.txt`));
    writeFileSync(join(sandbox, "elsewhere", "moving", "inner", `${tag}-kept.txt`), "kept");
    await expectResults(tag, [`${tag}-kept.txt`]);
    await expectResults(`${tag} in:${join(sandbox, "elsewhere", "moving", "inner")}`, [`${tag}-kept.txt`]);
  });

  await step("a junction made by another program is indexed but never followed", async () => {
    // What `npm link`, pnpm or `mklink /J` leave behind. Following it took the real folder's
    // subfolders for moves and re-parented them under the link.
    mkdirSync(join(sandbox, "real", "sub"), { recursive: true });
    writeFileSync(join(sandbox, "real", "sub", `${tag}-inside.txt`), "inside");
    await expectResults(`${tag}-inside`, [`${tag}-inside.txt`]);
    execFileSync("cmd", ["/c", "mklink", "/J", junction, join(sandbox, "real")], { stdio: "ignore" });
    await expectResults(`${tag}-link`, [`${tag}-link`]);
    // Once more after the link is in: the real folder still holds its subfolder and file.
    await page.waitForTimeout(1000);
    await expectResults(`${tag}-inside in:${join(sandbox, "real", "sub")}`, [`${tag}-inside.txt`]);
    await expectResults(`${tag}-inside in:${junction}`, []);
  });

  console.log("\nall checks passed");
} finally {
  await search.fill("").catch(() => {});
  await browser.close();
  // The junction goes first, on its own, so that nothing is ever removed through it.
  try {
    execFileSync("cmd", ["/c", "rmdir", junction], { stdio: "ignore" });
  } catch {
    // Never made.
  }
  rmSync(sandbox, { recursive: true, force: true });
}
