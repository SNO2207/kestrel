// End-to-end test of cleanup recommendations and the duplicate finder, against the real app.
//
// Everything happens in a scratch folder under %TEMP% laid out like a machine: a profile with
// its temp folder, crash dumps, Cargo's cache, a stale Rust project, old installers, a browser
// cache, duplicate photos, and the protected places (Documents, OneDrive, Program Files) with
// things in them the rules must never offer. The development hook (dev builds only) points every
// rule at that folder instead of the machine, says Chrome is running and Cargo is installed, and
// records tool runs and openings instead of performing them, so no real cache is looked at and
// no tool runs. Projects are allowed in the folder through the projects hook.
//
// Two tiny files named kestrel-test-* go through the Recycle Bin and come back with Undo, as in
// undo.mjs. Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/cleanup.mjs`. Screenshots go to e2e/out/cleanup, dark and light.

import assert from "node:assert/strict";
import { existsSync, mkdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { png } from "./fixtures.mjs";
import { webviews } from "./windows.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "cleanup");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

const sandbox = join(tmpdir(), `kestrel-e2e-cleanup-${process.pid}`);
const profile = join(sandbox, "Users", "me");
const local = join(profile, "AppData", "Local");
const temp = join(local, "Temp");
const project = join(profile, "code", "oldapp");
const photo = join(profile, "Pictures", "kestrel-test-photo.jpg");
const photoCopy = join(profile, "Downloads", "kestrel-test-photo copy.jpg");
const oldTemp = join(temp, "kestrel-test-old.tmp");
const DAY = 86_400_000;
rmSync(sandbox, { recursive: true, force: true });

/** Writes `size` bytes from `seed`, and dates the file `days` back. */
function put(path, size, seed, days = 0) {
  mkdirSync(dirname(path), { recursive: true });
  const bytes = Buffer.alloc(size);
  for (let i = 0; i < size; i++) bytes[i] = (i * 31 + seed) & 0xff;
  writeFileSync(path, bytes);
  if (days > 0) age(path, days);
}

function age(path, days) {
  const when = new Date(Date.now() - days * DAY);
  utimesSync(path, when, when);
}

put(oldTemp, 20_000, 1, 10);
put(join(temp, "kestrel-test-older.log"), 30_000, 2, 20);
put(join(temp, "fresh.tmp"), 10_000, 3);
put(join(local, "CrashDumps", "app.exe.4242.dmp"), 50_000, 4, 2);
put(join(profile, ".cargo", "registry", "cache", "index.crates.io-test", "serde-1.0.0.crate"), 40_000, 5);
put(join(project, "Cargo.toml"), 5_000, 6, 120);
put(join(project, "src", "main.rs"), 5_000, 7, 120);
put(join(project, "target", "debug", "oldapp.exe"), 60_000, 8, 120);
for (const folder of [join(project, "src"), join(project, "target", "debug"), join(project, "target"), project]) age(folder, 120);
put(join(profile, "Downloads", "setup-old.exe"), 70_000, 9, 100);
put(join(local, "Google", "Chrome", "User Data", "Default", "Cache", "Cache_Data", "data_1"), 30_000, 10);
// What must never be offered: the owner's own folders, OneDrive and the system's.
put(join(profile, "Documents", "keep.dmp"), 50_000, 11, 2);
put(join(profile, "OneDrive", "synced.dmp"), 50_000, 12, 2);
put(join(sandbox, "Program Files", "Thing", "crash.dmp"), 50_000, 13, 2);
// Duplicates: a photo and its copy, and a file the same size with other contents.
put(photo, 1_500_000, 14, 30);
put(photoCopy, 1_500_000, 14, 1);
put(join(profile, "Music", "kestrel-test-lookalike.bin"), 1_500_000, 15, 5);
// An image to preview, for the preview's own webview stepping aside for the page.
const viewing = join(sandbox, "viewing");
mkdirSync(viewing, { recursive: true });
writeFileSync(join(viewing, "photo.png"), png(160, 120, (x, y) => [x + 60, y + 80, 200]));

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const step = async (title, run) => {
  const started = Date.now();
  await run();
  console.log(`ok  ${title} (${Date.now() - started} ms)`);
};

async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** The panels are tabs of the disk usage hub. */
const cleanupPage = page.locator(".disk-hub");
const shownTab = page.locator('.disk-tab[aria-selected="true"]');
const rec = (id) => page.locator(`.cleanup-rec[data-id="${id}"]`);
const recByTitle = (title) => page.locator(".cleanup-rec").filter({ has: page.locator("h3", { hasText: title }) });
const dialog = page.locator(".cleanup-dialog");
const outcome = page.locator(".cleanup-outcome");
const lower = (path) => path.toLowerCase();
/** The full paths of path labels (each shows a name, then its folder). */
const pathsOf = (locator) => locator.evaluateAll((labels) => labels.map((label) => label.dataset.path));

/** Scans until every recommendation in `ids` is there: new files reach the index in a moment. */
async function scanUntil(ids, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const report = await invoke("cleanup_scan", { root: null });
    const found = report.recommendations.map((r) => r.id);
    if (ids.every((id) => found.some((f) => f === id || f.startsWith(id)))) return report;
    if (Date.now() > deadline) throw new Error(`expected ${ids.join(", ")}; found ${found.join(", ")}`);
    await page.waitForTimeout(400);
  }
}

let previewPaneOpened = false;

/** Waits until the preview's own webview (the window's second) is as `wanted`. */
async function previewView(wanted, what, timeout = 10_000) {
  const deadline = Date.now() + timeout;
  let seen;
  do {
    seen = webviews()[1] ?? null;
    if (seen && wanted(seen)) return seen;
    await page.waitForTimeout(100);
  } while (Date.now() < deadline);
  throw new Error(`the preview's webview is not ${what}: ${JSON.stringify(seen)}`);
}

async function choosePaletteAction(title) {
  await page.keyboard.press("Control+k");
  const input = page.locator(".palette input");
  await input.waitFor();
  await input.fill(`>${title}`);
  await page.locator('.palette-list[aria-busy="false"]').waitFor();
  const item = page.locator(".palette-item").filter({ has: page.locator(".palette-title", { hasText: title }) }).first();
  await item.waitFor();
  await item.click();
}

try {
  await step("the development hook points the rules at the scratch folder", async () => {
    await invoke("projects_probe", { on: false, allow: [sandbox] });
    await invoke("cleanup_dev_sandbox", {
      sandbox: { base: sandbox, running: ["chrome.exe"], tools: ["cargo"], recycle_bin: { bytes: 123_456_789, items: 7 } },
    });
    const report = await scanUntil(["temp", "crash-dumps", "cargo:", "cache:cargo", "downloads", "app:chrome", "recycle-bin", "windows"]);
    const paths = report.recommendations.flatMap((r) => r.items.map((item) => lower(item.path)));
    assert(paths.length > 0);
    for (const path of paths) {
      assert(path.startsWith(lower(sandbox)), `${path} lies outside the scratch folder`);
      for (const never of ["\\documents\\", "\\onedrive\\", "\\program files\\", "\\pictures\\"]) {
        assert(!path.includes(never), `${path} must never be recommended`);
      }
    }
    const temp = report.recommendations.find((r) => r.id === "temp");
    assert.deepEqual(temp.items.map((i) => lower(i.path)).sort(), [lower(oldTemp), lower(join(sandbox, "Users", "me", "AppData", "Local", "Temp", "kestrel-test-older.log"))].sort());
    assert.equal(temp.tier, "safe");
    const chrome = report.recommendations.find((r) => r.id === "app:chrome");
    assert.equal(chrome.tier, "info", "Chrome runs, so its cache is only mentioned");
  });

  await step("the palette opens the hub's Cleanup tab with recommendations, sizes and tiers", async () => {
    await choosePaletteAction("Cleanup recommendations");
    await cleanupPage.waitFor();
    await rec("temp").waitFor();
    assert.equal(await shownTab.textContent(), "Cleanup");
    const tiers = await page.locator(".cleanup-tier-head h2").allTextContents();
    assert.deepEqual(tiers, ["Safe to remove", "Review before removing", "Worth knowing"]);
    assert.equal(await rec("temp").locator(".cleanup-tier-chip").textContent(), "Safe");
    assert.match(await rec("temp").locator(".cleanup-rec-size").textContent(), /^\d+(\.\d)? KB$/);
    assert.equal(await rec("downloads").locator(".cleanup-tier-chip").textContent(), "Review");
    assert.equal(await rec("app:chrome").locator(".cleanup-tier-chip").textContent(), "Info");
    assert.match(await rec("app:chrome").locator(".cleanup-advice").textContent(), /Close Google Chrome/);
    assert.equal(await rec("recycle-bin").locator(".cleanup-rec-size").textContent(), "118 MB");
    assert.match(await page.locator(".cleanup-total-size").textContent(), /KB$/);
    await shotBoth("recommendations");
    await page.locator('.cleanup-tier[data-tier="info"]').scrollIntoViewIfNeeded();
    await shotBoth("review-and-info");
    await page.locator(".cleanup-scroll").evaluate((list) => list.scrollTo(0, 0));
  });

  await step("ticking one item and a dry run change nothing", async () => {
    await rec("temp").locator(".cleanup-expand").click();
    const item = rec("temp").locator(".cleanup-item").filter({ has: page.locator(".cleanup-item-path", { hasText: "kestrel-test-old.tmp" }) });
    await item.locator("input").check();
    assert.match(await page.locator(".cleanup-selected").textContent(), /^1 item selected · 20(\.0)? KB$/);
    await page.locator(".cleanup-dry-run").click();
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "Dry run");
    assert.match(await dialog.locator(".cleanup-dialog-lead").textContent(), /^Nothing was changed\. 1 item \(\d+(\.\d)? KB\) would go to the Recycle Bin\.$/);
    assert.deepEqual(await pathsOf(dialog.locator(".cleanup-dialog-path")), [oldTemp]);
    await shotBoth("dry-run");
    await dialog.locator(".dialog-button", { hasText: "Close" }).click();
    await dialog.waitFor({ state: "detached" });
    assert(existsSync(oldTemp));
  });

  await step("recycling it asks first, says where it goes, and Undo brings it back", async () => {
    await page.locator(".cleanup-recycle").click();
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "Move 1 item to the Recycle Bin?");
    assert.match(
      await dialog.locator(".cleanup-dialog-lead").textContent(),
      /^This moves 1 item \(20(\.0)? KB\) to the Recycle Bin\. You can restore it from there, or with Undo\./,
    );
    assert.deepEqual(await pathsOf(dialog.locator(".cleanup-dialog-path")), [oldTemp]);
    await shotBoth("confirm-recycle");
    await dialog.locator(".dialog-button.primary").click();
    await outcome.filter({ hasText: "Moved 1 item" }).waitFor({ timeout: 20_000 });
    assert(!existsSync(oldTemp), "the item went to the Recycle Bin");
    assert(existsSync(join(temp, "kestrel-test-older.log")), "the other item stayed");
    // The scan that follows no longer offers it, though the index may not have caught up yet.
    await page.waitForFunction(() => document.querySelectorAll('.cleanup-rec[data-id="temp"] .cleanup-item').length === 1);
    await shotBoth("recycled");
    await outcome.locator(".cleanup-undo").click();
    await outcome.filter({ hasText: "Undid" }).waitFor({ timeout: 20_000 });
    assert(existsSync(oldTemp), "Undo brought it back from the Recycle Bin");
    // Once the index has it again, a scan offers it again.
    for (let attempt = 0; (await rec("temp").locator(".cleanup-item").count()) < 2; attempt++) {
      assert(attempt < 40, "the restored item is offered again");
      await page.waitForTimeout(250);
      await page.locator(".cleanup-rescan").click();
    }
  });

  await step("a tool runs only after a confirmation that says it can't be undone", async () => {
    const cargo = page.locator('.cleanup-rec[data-id^="cargo:"]');
    await cargo.waitFor();
    assert.equal(await cargo.locator(".cleanup-command").textContent(), "cargo clean");
    await cargo.locator(".cleanup-run").click();
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "Run cargo clean?");
    assert.match(await dialog.locator(".cleanup-warning").textContent(), /can't be undone/);
    assert.equal(await dialog.locator(".cleanup-dialog-path-line").textContent(), project);
    await shotBoth("confirm-tool");
    await dialog.locator(".dialog-button.primary").click();
    await dialog.locator(".cleanup-output").waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "cargo clean finished");
    assert.match(await dialog.locator(".cleanup-output").textContent(), /recorded cargo clean instead of running it/);
    await shotBoth("tool-output");
    await dialog.locator(".dialog-button", { hasText: "Close" }).click();
    const records = await invoke("cleanup_dev_take");
    assert.deepEqual(records, [{ kind: "tool", program: "cargo", args: ["clean"], dir: project }]);
    assert(existsSync(join(project, "target")), "the hook ran nothing");
  });

  await step("Windows' own cleanup opens Storage Sense", async () => {
    await rec("windows").locator('.cleanup-open[data-target="ms-settings:storagesense"]').click();
    await page.waitForTimeout(200);
    const records = await invoke("cleanup_dev_take");
    assert.deepEqual(records.map((r) => [r.kind, r.program]), [["open", "ms-settings:storagesense"]]);
    assert.equal(await rec("recycle-bin").locator(".cleanup-open").textContent(), "Open the Recycle Bin");
  });

  await step("the Recycle Bin item closes the hub and shows the Recycle Bin page", async () => {
    await rec("recycle-bin").locator(".cleanup-open").click();
    await cleanupPage.waitFor({ state: "detached" });
    await page.locator(".tab-panel.active .bin-page").waitFor();
    assert.equal(await page.locator(".tab-panel.active.location-panel").count(), 1, "the page stands in for the file list");
    // Back where it was, and the hub's Cleanup tab once more.
    await page.keyboard.press("Alt+ArrowLeft");
    await page.locator(".tab-panel.active .bin-page").waitFor({ state: "detached" });
    await choosePaletteAction("Cleanup recommendations");
    await rec("temp").waitFor();
  });

  await step("duplicates are found and one copy goes to the Recycle Bin, then comes back", async () => {
    // The palette's way in opens the hub on the user folder, at its Duplicates tab.
    await choosePaletteAction("Find duplicate files");
    assert.equal(await shownTab.textContent(), "Duplicates");
    await page.locator(".dupes-root").first().waitFor();
    assert.deepEqual(await page.locator(".dupes-root-path").allTextContents(), [profile]);
    await page.locator(".dupes-search").click();
    const group = page.locator(".dupes-group");
    await group.first().waitFor({ timeout: 30_000 });
    assert.equal(await group.count(), 1, "the lookalike of the same size is no duplicate");
    const files = await pathsOf(group.locator(".dupes-file-path"));
    assert.deepEqual(files.map(lower).sort(), [lower(photo), lower(photoCopy)].sort());
    assert.match(await group.locator(".dupes-group-wasted").textContent(), /MB extra$/);
    // The oldest copy is kept unless told otherwise.
    const keptPath = async () => lower((await pathsOf(group.locator(".dupes-file.kept .dupes-file-path")))[0]);
    assert.equal(await keptPath(), lower(photo));
    await page.locator(".dupes-keep-newest").click();
    assert.equal(await keptPath(), lower(photoCopy));
    await page.locator(".dupes-keep-oldest").click();
    await group.locator(".dupes-group-check").check();
    await shotBoth("duplicates");
    await page.locator(".cleanup-recycle").click();
    await dialog.waitFor();
    assert.equal(await dialog.locator(".dialog-title").textContent(), "Move 1 copy to the Recycle Bin?");
    const listed = await pathsOf(dialog.locator(".cleanup-dialog-path"));
    assert.deepEqual(listed.map(lower), [lower(photoCopy), lower(photo)], "the copy to move, then the one kept");
    await shotBoth("confirm-duplicates");
    await dialog.locator(".dialog-button.primary").click();
    await outcome.filter({ hasText: "Moved 1 item" }).waitFor({ timeout: 20_000 });
    assert(!existsSync(photoCopy) && existsSync(photo), "the copy went, the original stayed");
    assert.equal(await page.locator(".dupes-group").count(), 0);
    await outcome.locator(".cleanup-undo").click();
    await outcome.filter({ hasText: "Undid" }).waitFor({ timeout: 20_000 });
    assert(existsSync(photoCopy), "Undo brought the copy back");
    // Searching again finds the pair once the index has heard the copy is back.
    for (let attempt = 0; (await page.locator(".dupes-group").count()) === 0; attempt++) {
      assert(attempt < 40, "the restored copy is a duplicate again");
      await page.waitForTimeout(250);
      if ((await page.locator(".dupes-search").count()) > 0 && (await page.locator(".dupes-group").count()) === 0) {
        await page.locator(".dupes-search").click();
      }
    }
  });

  await step("Escape closes the hub", async () => {
    await page.locator(".disk-hub h1").click();
    await page.keyboard.press("Escape");
    await cleanupPage.waitFor({ state: "detached" });
  });

  await step("the preview's webview steps aside while the hub covers it", async () => {
    const list = page.locator(".tab-panel.active .list-viewport");
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(viewing);
    await page.keyboard.press("Enter");
    const photoItem = page.locator(".tab-panel.active").locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: /^photo\.png$/ }) });
    await photoItem.waitFor();
    await list.focus();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor();
    previewPaneOpened = true;
    await photoItem.click();
    await page.locator(".preview-pane .preview-surface[data-kind='image']").waitFor();
    await previewView((view) => view.visible, "shown over the pane");
    await choosePaletteAction("Find duplicate files");
    await cleanupPage.waitFor();
    assert.equal(await shownTab.textContent(), "Duplicates");
    await previewView((view) => !view.visible, "hidden under the disk usage hub");
    await page.locator(".disk-hub h1").click();
    await page.keyboard.press("Escape");
    await cleanupPage.waitFor({ state: "detached" });
    await previewView((view) => view.visible, "shown again once the hub closes");
    await list.focus();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor({ state: "detached" });
    previewPaneOpened = false;
  });
} finally {
  if (previewPaneOpened) await page.keyboard.press("Alt+p").catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  await invoke("cleanup_dev_sandbox", { sandbox: null }).catch(() => {});
  await invoke("projects_probe", { on: false, allow: [] }).catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
console.log("cleanup: all passed");
