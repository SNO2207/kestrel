// End-to-end test of AI search ("Describe") on a tiny model, over a scratch folder.
//
// No real model is downloaded and nothing ever contacts Hugging Face. The suite:
//
// - opens the AI search page and checks both models are listed with their exact sizes, sources
//   and licence, with Download buttons, and that no download starts by itself;
// - installs the tiny test model (`kestrel_embed::onnx`, written by the development hook
//   `semantic_dev_install_tiny`) as the Fast model, "as if downloaded", whose self-test runs
//   and passes;
// - points the describing at a folder under %TEMP% named kestrel-test-e2e-semantic-*, holding
//   seven plain-colour PNGs and an MP4 (blue for six seconds, then green) that a development
//   hook encodes, and waits until all eight are described;
// - searches by description with the Describe toggle and checks the order against the tiny
//   model's arithmetic, done here by hand (its four dimensions are the normalised red, green and
//   blue means and the detail, which plain pictures lack); moves the relevance slider to fewer
//   and more; finds the video at the moment that matched, with the preview starting there; and
//   runs Find similar from a photo's menu;
// - opens the disk usage hub's Duplicates tab on the folder: the two near-identical photos
//   (khaki and olive) are a group of similar photos, ticked for nothing, apart from the exact
//   duplicates (there are none);
// - drives the Best model's Download button against a web server this suite starts on
//   127.0.0.1 (serving the tiny model's files, slowly), cancels, resumes (a Range request),
//   waits for the self-test, and deletes it again with the page's Delete.
//
// It deletes the tiny models, puts the folders, the slider and the window's settings back, and
// removes its folders at the end. It needs a dev build (its hooks).
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/semantic.mjs`. Screenshots go to e2e/out/semantic, dark and light.

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { createReadStream, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { png } from "./fixtures.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "semantic");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

const sandbox = join(tmpdir(), `kestrel-test-e2e-semantic-${process.pid}`);
const served = join(tmpdir(), `kestrel-test-e2e-semantic-serve-${process.pid}`);

/** Plain pictures, each a colour chosen so the tiny model ranks them clearly apart (khaki and
 * olive are near duplicates for it). */
const COLOURS = {
  "red.png": [220, 20, 30],
  "orange.png": [240, 140, 20],
  "purple.png": [140, 30, 160],
  "khaki.png": [210, 222, 85],
  "olive.png": [205, 225, 90],
  "blue.png": [30, 40, 220],
  "green.png": [30, 200, 60],
};
const VIDEO = "walk.mp4";

/** Pictures padded past the Duplicates tab's smallest "Skip files under" (100 KB). */
const LARGE = ["khaki.png", "olive.png"];

function makeFixture() {
  rmSync(sandbox, { recursive: true, force: true });
  mkdirSync(sandbox, { recursive: true });
  for (const [name, colour] of Object.entries(COLOURS)) {
    // Padded after the picture's end (decoders stop there), with different bytes each: no two
    // files are copies of each other.
    writeFileSync(join(sandbox, name), Buffer.concat([png(64, 64, () => colour), randomBytes(LARGE.includes(name) ? 120_000 : 6000)]));
  }
}

// ---------- the tiny model, by hand (kestrel_embed::onnx) ----------

const WORDS = { red: [1, -1, -1, 0], green: [-1, 1, -1, 0], blue: [-1, -1, 1, 0] };
const unit = (v) => {
  const norm = Math.hypot(...v);
  return norm > 0 ? v.map((x) => x / norm) : v;
};
const dot = (a, b) => a.reduce((sum, x, i) => sum + x * b[i], 0);
/** A plain picture: its normalised channel means, and no detail. */
const pictureVector = ([r, g, b]) => unit([r, g, b].map((v) => (v / 255 - 0.5) / 0.5).concat(0));
/** A description: the mean of its words' rows (unknown words and padding are zero). */
const textVector = (text) => unit(text.split(" ").map((w) => WORDS[w] ?? [0, 0, 0, 0]).reduce((a, b) => a.map((x, i) => x + b[i])));
/** The Fast model's ranges, which the tiny model is installed as: (fewer, more). */
const TEXT_RANGE = [0.15, 0.03];
const IMAGE_RANGE = [0.85, 0.55];
const threshold = ([fewer, more], cutoff) => fewer - (fewer - more) * cutoff;

/** The pictures a description finds at `cutoff`, best first, as the tiny model scores them. */
function expectedFor(text, cutoff) {
  const query = textVector(text);
  return Object.entries(COLOURS)
    .map(([name, colour]) => [name, dot(query, pictureVector(colour))])
    .filter(([, score]) => score >= threshold(TEXT_RANGE, cutoff))
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([name]) => name);
}

// ---------- a web server on this computer, for the Download button ----------

const requests = [];
let slow = true;
const server = createServer((request, response) => {
  const range = request.headers.range ?? null;
  requests.push({ url: request.url, range });
  // `/<repo>/resolve/<revision>/<path>`: the path is what follows the revision.
  const parts = decodeURIComponent(request.url ?? "").split("/resolve/")[1]?.split("/").slice(1).join("\\");
  const file = parts ? join(served, parts) : null;
  if (!file || !existsSync(file)) {
    response.writeHead(404).end();
    return;
  }
  const size = statSync(file).size;
  const from = range ? Number(/bytes=(\d+)-/.exec(range)?.[1] ?? 0) : 0;
  response.writeHead(from > 0 ? 206 : 200, {
    "Content-Length": size - from,
    ...(from > 0 ? { "Content-Range": `bytes ${from}-${size - 1}/${size}` } : {}),
  });
  const stream = createReadStream(file, { start: from, highWaterMark: 64 });
  stream.on("data", (chunk) => {
    response.write(chunk);
    if (slow) {
      stream.pause();
      setTimeout(() => stream.resume(), 250);
    }
  });
  stream.on("end", () => response.end());
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const baseUrl = `http://127.0.0.1:${server.address().port}`;

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
const STORAGE_KEYS = ["kestrel.describe", "kestrel.views", "search-scope", "preview-open", "preview-width"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate(() => {
  localStorage.removeItem("kestrel.describe");
  localStorage.setItem("search-scope", "folder");
  localStorage.removeItem("preview-open");
});
await page.reload();

const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const state = () => invoke("semantic_state");
const panel = page.locator(".tab-panel.active");
const names = () => panel.locator(".row .label").allTextContents();
const search = page.locator(".searchbox input");
const step = (await import("./steps.mjs")).stepper("semantic", () => page, { timed: true });

/** Captured through the debugging protocol, in both themes. */
async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(250);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Polls `read` until `ok` holds of what it returns. */
async function until(read, ok, what, timeout = 60_000) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    last = await read();
    if (ok(last)) return last;
    if (Date.now() > deadline) throw new Error(`${what}; last saw ${JSON.stringify(last)?.slice(0, 600)}`);
    await page.waitForTimeout(250);
  }
}

async function goTo(address) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(address);
  await page.keyboard.press("Enter");
}

/** Waits until the list shows exactly `expected`, in order. */
const showsExactly = (expected, what) =>
  until(names, (found) => JSON.stringify(found) === JSON.stringify(expected), `${what}: expected ${expected.join(", ")}`, 20_000);

const initial = await state();
const savedCutoff = initial.cutoff;
const savedScopes = initial.defaultScopes ? null : initial.scopes;
// Left over from an interrupted run: start with no model and the catalogue's own files.
for (const model of ["fast", "best"]) {
  await invoke("semantic_dev_setup", { setup: { model } });
  if (initial.models.find((each) => each.id === model)?.installed) await invoke("semantic_delete", { model });
}
const downloadsAtStart = await invoke("semantic_dev_downloads");

try {
  await step("the AI search page lists both models with their sizes and sources, and downloads nothing by itself", async () => {
    await goTo("AI search");
    const page_ = page.locator(".ai-page");
    await page_.waitFor();
    await page.locator(".tab.active .tab-title", { hasText: "AI search" }).waitFor();
    const fast = page.locator('.ai-model[data-model="fast"]');
    const best = page.locator('.ai-model[data-model="best"]');
    await fast.waitFor();
    await best.waitFor();
    assert.equal(await fast.locator(".ai-size").getAttribute("data-bytes"), "785356945");
    assert.equal(await best.locator(".ai-size").getAttribute("data-bytes"), "2307594564");
    assert.match(await fast.locator(".ai-size").innerText(), /785,356,945 bytes \(749 MB\) in 3 files/);
    assert.match(await best.locator(".ai-size").innerText(), /2,307,594,564 bytes \(2\.1 GB\) in 3 files/);
    assert.equal(await fast.locator(".ai-source a").innerText(), "huggingface.co/onnx-community/siglip2-base-patch16-256-ONNX");
    assert.equal(await best.locator(".ai-source a").innerText(), "huggingface.co/onnx-community/siglip2-so400m-patch16-384-ONNX");
    assert.match(await fast.locator(".ai-source").innerText(), /at revision d1114256/);
    assert.match(await best.locator(".ai-source").innerText(), /at revision 29f74e67/);
    for (const card of [fast, best]) {
      assert.equal(await card.locator(".ai-licence").innerText(), "Apache 2.0, by Google; converted to ONNX by onnx-community");
      assert.match(await card.innerText(), /graphics card/);
      assert.equal(await card.locator(".ai-progress").count(), 0, "no download in progress");
    }
    assert.match(await fast.locator(".ai-download").innerText(), /Download from Hugging Face \(749 MB\)/);
    assert.match(await best.locator(".ai-download").innerText(), /Download from Hugging Face \(2\.1 GB\)/);
    await shotBoth("1-page-nothing-downloaded");
    // The files and their checksums are listed too.
    await fast.locator(".ai-files summary").click();
    await fast.locator(".ai-sha", { hasText: "fe9ad8020a6d3d98d394c9be8f07064066135fc2f87ec11692de0b677c0ac4db" }).waitFor();
    await best.scrollIntoViewIfNeeded();
    await shotBoth("1-page-models");
    await fast.locator(".ai-files summary").click();
    await page.waitForTimeout(3000);
    assert.equal(await invoke("semantic_dev_downloads"), downloadsAtStart, "no download started by itself");
    assert((await state()).models.every((model) => !model.installed && model.download === null));
  });

  await step("without a model, Describe says how to get one", async () => {
    makeFixture();
    await goTo(sandbox);
    await panel.locator(".row .label", { hasText: "red.png" }).waitFor();
    await page.locator(".describe-toggle").click();
    await search.fill("dog");
    await panel.locator(".list-error", { hasText: "Describing needs a model" }).waitFor();
    await page.locator(".describe-bar .describe-setup").waitFor();
    await shotBoth("2-no-model");
    await page.locator(".describe-bar .describe-setup").click();
    await page.locator(".tab.active .tab-title", { hasText: "AI search" }).waitFor();
    await page.locator(".ai-page").waitFor();
    await page.keyboard.press("Control+w");
    await page.locator(".describe-toggle").click();
    assert.equal(await page.locator(".describe-toggle").getAttribute("aria-pressed"), "false");
    await search.fill("");
    await panel.locator(".row .label", { hasText: "red.png" }).waitFor();
    await goTo("AI search");
    await page.locator(".ai-page").waitFor();
  });

  await step("a tiny model installed as if downloaded passes its self-test", async () => {
    await invoke("semantic_dev_write_video", {
      path: join(sandbox, VIDEO),
      size: [128, 96],
      fps: 10,
      segments: [
        [[30, 40, 220], 6000],
        [[30, 200, 60], 3000],
        [[235, 235, 235], 3000],
      ],
    });
    await invoke("semantic_dev_setup", { setup: { scopes: [sandbox] } });
    const report = await invoke("semantic_dev_install_tiny", { model: "fast" });
    assert(report.passed, JSON.stringify(report));
    assert.equal(report.checks.length, 8);
    assert(report.device.gpu, `the tiny model runs on the GPU: ${JSON.stringify(report.device)}`);
    const fast = page.locator('.ai-model[data-model="fast"]');
    await fast.locator(".ai-in-use").waitFor();
    await fast.locator('.ai-selftest[data-passed="yes"]').waitFor();
    assert.equal(await fast.locator(".ai-checks tbody tr").count(), 8);
    assert.match(await fast.locator(".ai-throughput").innerText(), /pictures a second on /);
  });

  await step("the photos and the video in the folder are described", async () => {
    await until(async () => (await state()).indexing, (indexing) => indexing.state === "done" && indexing.files === 8, "all eight described");
    const page_ = page.locator(".ai-page");
    await page_.locator(".ai-indexing", { hasText: "Up to date." }).waitFor();
    await page_.locator(".ai-described", { hasText: "8 described with the Fast model" }).waitFor();
    await page_.locator(".ai-scope-path", { hasText: sandbox }).waitFor();
    await shotBoth("2-page-tiny-model");
    // Pause holds the describing until Resume.
    await page_.locator(".ai-pause", { hasText: "Pause" }).click();
    await page_.locator(".ai-indexing", { hasText: "Paused." }).waitFor();
    assert.equal((await state()).paused, true);
    await page_.locator(".ai-pause", { hasText: "Resume" }).click();
    await page_.locator(".ai-indexing", { hasText: "Up to date." }).waitFor({ timeout: 20_000 });
    assert.equal((await state()).paused, false);
  });

  await step("Describe ranks the pictures as the tiny model says", async () => {
    await goTo(sandbox);
    await panel.locator(".row .label", { hasText: "red.png" }).waitFor();
    await page.locator(".describe-toggle").click();
    assert.equal(await page.locator(".describe-toggle").getAttribute("aria-pressed"), "true");
    assert.equal(await search.getAttribute("placeholder"), "Describe a photo or video");
    await search.fill("red");
    const expected = expectedFor("red", 0.5);
    assert.deepEqual(expected, ["red.png", "orange.png", "purple.png", "khaki.png"], "the hand computation");
    await showsExactly(expected, "red at the middle of the slider");
    await page.locator(".describe-bar .describe-what", { hasText: "Looks like red" }).waitFor();
    await page.locator(".describe-bar .describe-count", { hasText: "4 found" }).waitFor();
    assert.equal(await search.inputValue(), "red", "the box shows the description without its ~");
    assert.equal(await panel.locator(".column-header.sorted").count(), 0, "best match first, in no column's order");
    await shotBoth("3-describe-red");
    // A column still sorts them, and its arrow shows.
    await panel.locator(".column-header.name").click();
    await panel.locator(".column-header.name.sorted").waitFor();
    const byName = [...expected].sort((a, b) => a.localeCompare(b));
    const either = [JSON.stringify(byName), JSON.stringify([...byName].reverse())];
    await until(names, (found) => either.includes(JSON.stringify(found)), "sorted by name", 10_000);
    await search.fill("");
    await panel.locator(".row .label", { hasText: "blue.png" }).waitFor();
    await search.fill("red");
    await showsExactly(expected, "a new search is best first again");
    assert.equal(await panel.locator(".column-header.sorted").count(), 0);
  });

  await step("the relevance slider shows fewer and more", async () => {
    const slider = page.locator(".describe-cutoff input");
    await slider.fill("0");
    await showsExactly(expectedFor("red", 0), "fewer");
    assert.deepEqual(expectedFor("red", 0), ["red.png", "orange.png", "purple.png"]);
    await slider.fill("100");
    await showsExactly(expectedFor("red", 1), "more");
    assert.deepEqual(expectedFor("red", 1), ["red.png", "orange.png", "purple.png", "khaki.png", "olive.png"]);
    assert.equal((await state()).cutoff, 1);
    await slider.fill("50");
    await showsExactly(expectedFor("red", 0.5), "back in the middle");
  });

  await step("a video is found at the moment that matched, and the preview starts there", async () => {
    await search.fill("green");
    const video = panel.locator(".row", { has: page.locator(".label", { hasText: VIDEO }) });
    await video.waitFor();
    const found = await names();
    assert(found.includes("green.png") && found.includes(VIDEO), found.join(", "));
    assert(!found.includes("blue.png") && !found.includes("red.png"), found.join(", "));
    // Blue for six seconds, green for three, then white: sampled about every five seconds
    // (blue, blue, white), with one more frame halfway between the two samples either side of
    // the cut, which is the only green frame. Where exactly depends on the encoder's key frames.
    const badge = await video.locator(".moment-badge").innerText();
    const [minutes, seconds] = badge.split(":").map(Number);
    assert(minutes === 0 && seconds >= 6 && seconds < 9, `matched in the green part: ${badge}`);
    await video.click();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane .preview-surface[data-kind='video']").waitFor();
    let view = null;
    for (let attempt = 0; !view && attempt < 100; attempt++) {
      view = browser.contexts().flatMap((context) => context.pages()).find((other) => other !== page && other.url().startsWith("http://preview.localhost/view")) ?? null;
      if (!view) await page.waitForTimeout(100);
    }
    assert(view, "the preview's webview shows the video");
    const url = await until(() => view.url(), (url) => decodeURIComponent(url).includes(`${VIDEO}&`) && /[?&]t=\d/.test(url), "the preview starts at the moment", 10_000);
    const start = Number(/[?&]t=([\d.]+)/.exec(url)[1]);
    assert.equal(Math.floor(start), seconds, `the preview's start ${start} s is the badge's moment ${badge}`);
    const at = await until(
      () => view.evaluate(() => document.querySelector("video")?.readyState >= 1 && document.querySelector("video").currentTime).catch(() => false),
      (time) => typeof time === "number" && time > 0,
      "the player is at the moment",
      10_000,
    );
    assert(Math.abs(at - start) < 0.2, `the player starts at ${at} s, not ${start} s`);
    await shotBoth("4-describe-video-preview");
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor({ state: "detached" });
    // The icon views show the moment too, with the thumbnails.
    await page.keyboard.press("Control+Shift+2");
    await panel.locator(".view-item", { has: page.locator(".moment-badge", { hasText: badge }) }).waitFor();
    await shotBoth("5-describe-large-icons");
    await page.keyboard.press("Control+Shift+6");
    await panel.locator(".row").first().waitFor();
  });

  await step("Find similar in a photo's menu finds the photos that look like it", async () => {
    await page.locator(".describe-toggle").click();
    await search.fill("");
    await search.press("Escape");
    await panel.locator(".row .label", { hasText: "red.png" }).waitFor();
    await panel.locator(".row", { has: page.locator(".label", { hasText: /^red\.png$/ }) }).click({ button: "right" });
    await page.locator(".menu .menu-item", { hasText: "Find similar" }).click();
    const red = pictureVector(COLOURS["red.png"]);
    const expected = Object.entries(COLOURS)
      .map(([name, colour]) => [name, dot(red, pictureVector(colour))])
      .filter(([, score]) => score >= threshold(IMAGE_RANGE, 0.5))
      .sort((a, b) => b[1] - a[1])
      .map(([name]) => name);
    assert.deepEqual(expected, ["red.png", "orange.png"]);
    await showsExactly(expected, "similar to red.png");
    assert.equal(await search.inputValue(), `like:"${join(sandbox, "red.png")}"`);
    await page.locator(".describe-bar .describe-what", { hasText: "Looks like red.png" }).waitFor();
    // Find similar has no moments: none of its hits is a video's frame.
    await shotBoth("6-find-similar");
    await search.press("Escape");
  });

  await step("the Duplicates tab lists the photos that look alike as similar photos, ticked for nothing", async () => {
    await goTo(sandbox);
    await panel.locator(".row .label", { hasText: "red.png" }).waitFor();
    await page.keyboard.press("Control+Shift+U");
    const hub = page.locator(".disk-hub");
    await hub.waitFor();
    await hub.locator(".disk-tab[data-tab=duplicates]").click();
    await hub.locator(".dupes-root-path", { hasText: sandbox }).waitFor();
    await hub.locator(".dupes-min select").selectOption(String(100 * 1024));
    await hub.locator(".dupes-search").click();
    const group = hub.locator(".similar-group");
    await group.waitFor({ timeout: 30_000 });
    // Khaki and olive look all but the same to the tiny model; nothing else is that close.
    const alike = dot(pictureVector(COLOURS["khaki.png"]), pictureVector(COLOURS["olive.png"]));
    assert(alike > 0.99, `khaki and olive: ${alike}`);
    assert.equal(await group.count(), 1);
    const paths = await group.locator(".dupes-file").evaluateAll((files) => files.map((file) => file.getAttribute("data-path")));
    assert.deepEqual(paths.map((path) => path.split("\\").pop()).sort(), ["khaki.png", "olive.png"]);
    assert.equal(await hub.locator(".dupes-group").count(), 0, "no exact duplicates: every file has its own bytes");
    await hub.locator(".dupes-summary-main", { hasText: "No duplicates found." }).waitFor();
    assert.match(await group.locator(".dupes-group-meta").innerText(), /^2 photos · (99|100)% alike$/);
    assert.equal(await group.locator(".dupes-group-check").isChecked(), false, "never ticked for the owner");
    assert.equal(await hub.locator(".dupes-all").isVisible(), false, "no Select all without exact duplicates");
    assert.equal(await group.locator(".dupes-file.kept").count(), 1, "one photo to keep, the oldest");
    assert.equal(await hub.locator(".cleanup-recycle").isDisabled(), true, "nothing to move until a group is ticked");
    // Ticked, a dry run moves only the photo not kept; nothing is changed.
    await group.locator(".dupes-group-check").check();
    await hub.locator(".cleanup-selected", { hasText: "1 photo selected" }).waitFor();
    await shotBoth("7-similar-photos");
    await hub.locator(".cleanup-dry-run").click();
    const dry = page.locator(".cleanup-dialog");
    await dry.waitFor();
    assert.match(await dry.innerText(), /Nothing was changed\. 1 item \(/);
    await dry.locator(".dialog-button", { hasText: "Close" }).click();
    await group.locator(".dupes-group-check").uncheck();
    assert(existsSync(join(sandbox, "khaki.png")) && existsSync(join(sandbox, "olive.png")));
    await hub.locator("h1").click();
    await page.keyboard.press("Escape");
    await hub.waitFor({ state: "detached" });
  });

  await step("the Download button fetches from this computer with progress, cancel and resume, then tests; Delete removes it", async () => {
    const files = await invoke("semantic_dev_tiny_files", { dir: served });
    const override = { baseUrl, files: files.map(([path, size, sha256]) => ({ path, size, sha256 })) };
    await invoke("semantic_dev_setup", { setup: { model: "best", override } });
    await goTo("AI search");
    const best = page.locator('.ai-model[data-model="best"]');
    await best.locator(".ai-download").waitFor();
    slow = true;
    await best.locator(".ai-download").click();
    await best.locator(".ai-progress").waitFor();
    // Cancelled part way through the first file, so the next attempt has something to resume.
    const first = files[0][1];
    await until(
      async () => (await state()).models.find((model) => model.id === "best").download,
      (download) => download?.state === "downloading" && download.done > 0 && download.done < first,
      "part of the first file arrives",
      15_000,
    );
    await until(() => best.locator(".ai-progress").getAttribute("aria-valuenow"), (now) => Number(now) > 0, "the bar moves", 5_000);
    await shotBoth("6-downloading");
    await best.locator(".ai-cancel").click();
    await best.locator(".ai-message", { hasText: "Cancelled. What arrived is kept" }).waitFor();
    await best.locator(".ai-discard").waitFor();
    await shotBoth("7-cancelled");
    const kept = requests.length;
    slow = false;
    await best.locator(".ai-download", { hasText: "Resume download from Hugging Face" }).click();
    await best.locator('.ai-selftest[data-passed="yes"]').waitFor({ timeout: 60_000 });
    assert(requests.slice(kept).some((request) => request.range !== null), `a resumed request asks for the rest: ${JSON.stringify(requests)}`);
    assert(requests.every((request) => request.url.startsWith("/onnx-community/siglip2-so400m-patch16-384-ONNX/resolve/29f74e673c298fd07482d8e20138179282001eb7/")));
    assert.equal(await invoke("semantic_dev_downloads"), downloadsAtStart + 2, "a download only for each click");
    await best.locator(".ai-use").waitFor();
    assert.equal((await state()).active, "fast", "a new download doesn't take over the model in use");
    await shotBoth("8-downloaded-and-tested");
    await best.locator(".ai-delete").click();
    const dialog = page.locator(".confirm-dialog");
    await dialog.waitFor();
    assert.match(await dialog.innerText(), /Delete the Best model\?/);
    assert.match(await dialog.innerText(), /for good \(they don't go to the Recycle Bin\)/);
    await dialog.locator(".dialog-button.primary").click();
    await best.locator(".ai-download", { hasText: "Download from Hugging Face" }).waitFor();
    assert(!(await state()).models.find((model) => model.id === "best").installed);
  });

  await step("a file whose checksum doesn't match is deleted, and the page says so", async () => {
    const files = await invoke("semantic_dev_tiny_files", { dir: served });
    const wrong = files.map(([path, size, sha256]) => ({ path, size, sha256: path === "tokenizer.json" ? "0".repeat(64) : sha256 }));
    await invoke("semantic_dev_setup", { setup: { model: "best", override: { baseUrl, files: wrong } } });
    const best = page.locator('.ai-model[data-model="best"]');
    await best.locator(".ai-download").click();
    const message = best.locator(".ai-message.ai-error");
    await message.waitFor({ timeout: 30_000 });
    assert.match(await message.innerText(), /tokenizer\.json: The file arrived damaged: its SHA-256 is [0-9a-f]{64}, not 0{64}\. It was deleted; try again\./);
    assert(!(await state()).models.find((model) => model.id === "best").installed, "not installed");
    await shotBoth("9-checksum-mismatch");
    // What did arrive and match is discarded on request.
    await best.locator(".ai-discard").click();
    await page.locator(".confirm-dialog .dialog-button.primary").click();
    await best.locator(".ai-download", { hasText: /^Download from Hugging Face/ }).waitFor();
  });

  assert.deepEqual(errors, [], "no errors in the page");
} finally {
  for (const model of ["best", "fast"]) {
    await invoke("semantic_delete", { model }).catch(() => {});
    await invoke("semantic_dev_setup", { setup: { model } }).catch(() => {});
  }
  await invoke("semantic_set_scopes", { scopes: savedScopes }).catch(() => {});
  await invoke("semantic_set_cutoff", { value: savedCutoff }).catch(() => {});
  await invoke("semantic_set_paused", { paused: initial.paused }).catch(() => {});
  await page.evaluate((saved) => {
    for (const [key, value] of saved) {
      if (value === null) localStorage.removeItem(key);
      else localStorage.setItem(key, value);
    }
  }, savedStorage);
  await page.reload().catch(() => {});
  server.close();
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
  rmSync(served, { recursive: true, force: true });
}
console.log("semantic: all passed");
