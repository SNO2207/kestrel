// End-to-end test of searching: inside files, phrases read into chips, the Downloads triage
// page and WSL's distributions, all in a scratch folder under %TEMP%.
//
// The suite makes a folder named kestrel-test-e2e-search-*, holding files to search inside
// (UTF-8, UTF-16 with a byte-order mark, a binary file, a repository whose .gitignore leaves a
// folder out), a small project and two photos with known dates taken (written by Windows
// PowerShell's System.Drawing), folders standing in for Downloads, Documents, Pictures, Videos
// and Music, and a folder standing in for a WSL distribution. Development hooks point Kestrel at
// the stand-ins (`search_dev_places`, `wsl_dev_distros`, `triage_dev_seen`), so the owner's own
// Downloads and Documents are never touched and no distribution is ever started.
//
// It checks: a search inside files streaming its matches into the results list, each with its
// line and the match marked, in the details, tile, list and content views; binary files passed
// over and .gitignore honoured (and not, when switched off); match case; a click on a match
// opening the preview at that line; a phrase read into editable chips with the right results,
// a chip edited and one removed; dates taken; the triage page's groups, a suggested move, Undo,
// the suggestion learnt from that move, several items recycled at once and brought back with
// Undo, an archive extracted through the archive feature, and the sidebar's count of new
// downloads; and a stand-in distribution in the sidebar and This PC, browsed with Linux's dot
// files hidden and its names decoded, with Open in Terminal starting its shell (recorded by the
// projects feature's hook rather than started).
//
// Two small files named kestrel-test-* pass through the Recycle Bin and come back with Undo.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/search.mjs`. Screenshots go to e2e/out/search, dark and light.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, rmSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "search");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

const sandbox = join(tmpdir(), `kestrel-test-e2e-search-${process.pid}`);
const content = join(sandbox, "content");
/** Files that each match, searched slowly to watch the results arrive. */
const streaming = join(sandbox, "streaming");
const STREAMED = 60;
const project = join(sandbox, "e2eproj");
const photos = join(sandbox, "photos");
const downloads = join(sandbox, "Downloads");
const documents = join(sandbox, "Documents");
const pictures = join(sandbox, "Pictures");
const videos = join(sandbox, "Videos");
const music = join(sandbox, "Music");
const distro = join(sandbox, "wsl", "Ubuntu");
const linuxHome = join(distro, "home", "levi");
const DAY = 86_400_000;

function utf16(text) {
  return Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, "utf16le")]);
}

/** A JPEG whose EXIF says when it was taken, as `2023:07:14 10:30:00`. */
function photo(path, when) {
  const script =
    "Add-Type -AssemblyName System.Drawing; $b = New-Object System.Drawing.Bitmap 24,16; " +
    "$i = [System.Runtime.Serialization.FormatterServices]::GetUninitializedObject([System.Drawing.Imaging.PropertyItem]); " +
    `$i.Id = 0x9003; $i.Type = 2; $v = [System.Text.Encoding]::ASCII.GetBytes('${when}' + [char]0); $i.Len = $v.Length; $i.Value = $v; ` +
    `$b.SetPropertyItem($i); $b.Save('${path.replace(/'/g, "''")}', [System.Drawing.Imaging.ImageFormat]::Jpeg); $b.Dispose()`;
  execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", script]);
}

function write(path, data) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, data);
}

function makeFixture() {
  rmSync(sandbox, { recursive: true, force: true });
  const lines = Array.from({ length: 120 }, (_, i) => `line ${i + 1} of the field notes`);
  lines[89] = "    and at last, at line ninety, the haystack gives up its needle";
  write(join(content, "notes.txt"), lines.join("\r\n"));
  write(join(content, "wide.txt"), utf16("first\r\nsecond\r\nthe needle, in UTF-16\r\n"));
  write(join(content, "blob.txt"), Buffer.concat([Buffer.from("needle before a nul\n"), Buffer.from([0, 1, 2, 3, 0])]));
  write(join(content, "plain.md"), "# Nothing to see\n\nNo match here.\n");
  write(join(content, "repo", ".git", "HEAD"), "ref: refs/heads/main\n");
  write(join(content, "repo", ".gitignore"), "build/\n");
  write(join(content, "repo", "src", "lib.rs"), "pub fn find() {\n    // the needle\n    // NEEDLE again\n}\n");
  write(join(content, "repo", "build", "out.txt"), "needle in build output\n");
  for (let n = 0; n < STREAMED; n++) write(join(streaming, `part-${String(n).padStart(2, "0")}.txt`), `part ${n}\nthe needle\n`);

  write(join(project, "main.rs"), "fn main() {}\n");
  write(join(project, "old.rs"), "fn old() {}\n");
  const old = new Date("2020-06-15T12:00:00");
  utimesSync(join(project, "old.rs"), old, old);
  write(join(project, "notes.txt"), "not code\n");

  mkdirSync(photos, { recursive: true });
  photo(join(photos, "beach.jpg"), "2023:07:14 10:30:00");
  photo(join(photos, "snow.jpg"), "2024:01:20 09:00:00");

  for (const dir of [downloads, documents, pictures, videos, music]) mkdirSync(dir, { recursive: true });
  write(join(downloads, "setup-tool-2.1.exe"), "MZ not really an installer");
  write(join(downloads, "Invoice-2026-10.pdf"), "%PDF-1.4 not really a pdf");
  write(join(downloads, "kestrel-test-IMG_2041.jpg"), "not really a picture");
  write(join(downloads, "kestrel-test-clip.mp4"), "not really a video");
  write(join(downloads, "data.bin"), "something else");
  // A real zip, made by Windows PowerShell, for the archive feature to extract.
  write(join(sandbox, "zip-source", "inside.txt"), "packed\n");
  execFileSync("powershell", [
    "-NoProfile",
    "-NonInteractive",
    "-Command",
    `Compress-Archive -Path '${join(sandbox, "zip-source", "inside.txt")}' -DestinationPath '${join(downloads, "kestrel-test-photos.zip")}'`,
  ]);
  // Still downloading: never listed.
  write(join(downloads, "movie.mp4.crdownload"), "partial");

  write(join(linuxHome, ".bashrc"), "# bash\n");
  // WSL hands Linux names holding ':' to Windows as U+F03A.
  write(join(linuxHome, "notes\uf03atodo.txt"), "colon in the name\n");
  write(join(linuxHome, "readme.md"), "# home\n");
}

makeFixture();

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
const STORAGE_KEYS = ["kestrel.views", "preview-open", "details-open", "search-scope", "kestrel.search-options"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate(() => {
  localStorage.removeItem("kestrel.views");
  localStorage.setItem("preview-open", "0");
  localStorage.setItem("details-open", "0");
  localStorage.removeItem("kestrel.search-options");
  // Searching this folder keeps every search inside the scratch folder.
  localStorage.setItem("search-scope", "folder");
});
const invoke = (command, args = {}) => page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);

await invoke("search_dev_places", { folders: { downloads, documents, pictures, videos, music }, projects: [["e2eproj", project]] });
await invoke("triage_dev_seen", { at: Date.now() - DAY });
await invoke("wsl_dev_distros", { distros: [{ name: "Ubuntu", path: distro, version: 2, default: true }] });
await page.reload();

const panel = page.locator(".tab-panel.active");
const searchBox = page.locator(".searchbox input");
const step = (await import("./steps.mjs")).stepper("search", () => page, { timed: true });

/** Captured through the debugging protocol, in both themes. */
async function shotBoth(name, prepare = async () => {}) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(250);
    await prepare();
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Waits until `read` returns `want`, polling. */
async function until(read, want, what, timeout = 30_000) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    last = await read();
    if (JSON.stringify(last) === JSON.stringify(want)) return;
    if (Date.now() > deadline) throw new Error(`${what}: expected ${JSON.stringify(want)}, last saw ${JSON.stringify(last)}`);
    await page.waitForTimeout(150);
  }
}

async function openFolder(path) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await page.waitForFunction((path) => document.querySelector(".address .crumb:last-of-type")?.textContent === path.split("\\").pop(), path);
}

/** Types a search, as the owner would. */
async function search(text) {
  await searchBox.click();
  await searchBox.fill(text);
}

async function clearSearch() {
  await searchBox.click();
  await searchBox.fill("");
  await page.keyboard.press("Escape");
}

const activeTab = () => page.evaluate(() => Number(document.querySelector(".tab-panel.active")?.getAttribute("data-panel-tab")));
const reading = async () => page.evaluate((tab) => window.__kestrelSearch.reading(tab), await activeTab());
/** The result rows' names, as the list shows them, with their match's line. */
const shownHits = () =>
  panel.locator(".row").evaluateAll((rows) =>
    rows
      .map((row) => [row.querySelector(".label")?.textContent ?? "", Number(row.querySelector(".hit-snippet")?.getAttribute("data-line") ?? 0)])
      .sort((a, b) => a[0].localeCompare(b[0])),
  );
const shownNames = async () => (await shownHits()).map(([name]) => name);
/** How many results the list holds, as the status bar counts them (rows out of view aren't drawn). */
const shownTotal = async () => Number(((await page.locator(".statusbar > span").first().textContent()) ?? "").match(/^([\d,]+) result/)?.[1]?.replace(/,/g, "") ?? -1);
/** Waits for a search inside files to be done. */
const searchDone = () => until(async () => (await reading())?.content?.done ?? null, true, "the search inside files to finish");
const recycled = () =>
  Number(
    execFileSync(
      "powershell",
      [
        "-NoProfile",
        "-Command",
        `& { $pattern = $args[0]; $bin = (New-Object -ComObject Shell.Application).NameSpace(10); @($bin.Items() | Where-Object { [string]$_.ExtendedProperty('System.Recycle.DeletedFrom') -like $pattern }).Count } '${sandbox.replace(/'/g, "''")}*'`,
      ],
      { encoding: "utf8" },
    ).trim(),
  );
const recycledBefore = recycled();

try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });

  // ---------- inside files ----------

  await step("a search inside files streams its matches into the results, line and match shown", async () => {
    await openFolder(content);
    await page.locator(".search-inside").click();
    assert.equal(await page.locator(".search-inside").getAttribute("aria-pressed"), "true");
    assert.equal(await searchBox.getAttribute("placeholder"), "Search inside files here");
    await search("needle");
    await searchDone();
    await until(shownHits, [
      ["lib.rs", 2],
      ["notes.txt", 90],
      ["wide.txt", 3],
    ], "UTF-8 and UTF-16 files match; the binary file and the ignored build folder don't");
    const status = (await reading()).content;
    assert.equal(status.binary, 1, "blob.txt holds a NUL byte");
    assert.equal(status.ignored, 1, "build/ is ignored by the repository's .gitignore");
    // The match is marked, and the second line of lib.rs counted.
    const notes = panel.locator('.row[data-path$="notes.txt"]');
    assert.equal(await notes.locator(".hit-mark").first().textContent(), "needle");
    assert.equal(await panel.locator('.row[data-path$="lib.rs"] .hit-more').textContent(), "+1");
    const banner = panel.locator(".search-banner");
    assert.match(await banner.locator(".search-progress-main").textContent(), /^3 files match in content; \d+ files read\.$/);
    assert.match(await banner.locator(".search-progress-skipped").textContent(), /1 binary file skipped, 1 ignored by \.gitignore/);
    assert.equal(await banner.locator(".search-chip").first().textContent(), "Contains “needle”");
    assert.equal(await panel.locator(".row .location").first().count(), 1, "results show where each file is");
    await shotBoth("1-inside-files");
  });

  await step(".gitignore can be switched off, and match case on", async () => {
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Skip what .gitignore ignores" }).click();
    await until(shownNames, ["lib.rs", "notes.txt", "out.txt", "wide.txt"], "build output searched too");
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Skip what .gitignore ignores" }).click();
    await until(shownNames, ["lib.rs", "notes.txt", "wide.txt"], "ignored again");
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Match case" }).click();
    await search("NEEDLE");
    await until(shownHits, [["lib.rs", 3]], "only the line in capitals");
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Match case" }).click();
    // A regular expression, with the words after it choosing the files.
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Regular expression" }).click();
    await search("ne+dle,? in ext:txt");
    await until(shownHits, [["wide.txt", 3]], "the expression, in .txt files only");
    await page.locator(".search-options-button").click();
    await page.locator(".menu .menu-item", { hasText: "Regular expression" }).click();
  });

  await step("every keystroke starts the search again, and the last one wins", async () => {
    await searchBox.click();
    await searchBox.fill("");
    await searchBox.type("haystack", { delay: 15 });
    await searchDone();
    await until(shownHits, [["notes.txt", 90]], "only the last text's matches");
    assert.equal((await reading()).text, "haystack");
  });

  await step("results stream in while the search goes on, and Stop ends it where it got to", async () => {
    // Each file waits a moment before it is read (a development hook), so the search takes a
    // second or so and its results can be seen arriving.
    await invoke("search_dev_slow", { ms: 200 });
    try {
      await openFolder(streaming);
      await search("needle");
      let partial = null;
      for (const deadline = Date.now() + 30_000; Date.now() < deadline; ) {
        const [read, rows] = [await reading(), await shownTotal()];
        if (read?.content && !read.content.done && rows > 0 && rows < STREAMED) partial ??= { rows, read: read.content.searched };
        if (read?.content?.done) break;
        await page.waitForTimeout(40);
      }
      assert(partial, "some results were shown before the search finished");
      assert.match(await panel.locator(".search-progress-main").textContent(), new RegExp(`^${STREAMED} files match in streaming;`));
      await until(shownTotal, STREAMED, "every match in the end");
      // Again, stopped part way: what it found stays, and nothing more arrives.
      await page.keyboard.press("F5");
      await panel.locator(".search-progress.running").waitFor();
      await until(async () => (await reading())?.content?.matched > 0, true, "something found");
      await panel.locator(".search-progress .search-banner-link", { hasText: "Stop" }).click();
      await panel.locator(".search-progress.running").waitFor({ state: "detached" });
      await page.waitForTimeout(1500);
      const stopped = await shownTotal();
      assert(stopped > 0 && stopped < STREAMED, `stopped with ${stopped} of ${STREAMED} shown`);
      assert.equal((await reading()).content.done, true);
      await page.waitForTimeout(800);
      assert.equal(await shownTotal(), stopped, "nothing more arrived after Stop");
    } finally {
      await invoke("search_dev_slow", { ms: 0 });
    }
    await clearSearch();
    await openFolder(content);
    await search("needle");
    await searchDone();
  });

  await step("a click on a match opens the preview at its line, the match marked", async () => {
    await until(shownNames, ["lib.rs", "notes.txt", "wide.txt"], "the matches in content");
    await panel.locator('.row[data-path$="notes.txt"] .hit-snippet').click();
    const preview = page.locator(".preview-pane");
    await preview.locator(".preview-hit-line").waitFor();
    await page.waitForTimeout(300);
    const placed = await preview.locator(".preview-text").evaluate((scroller) => {
      const marker = scroller.querySelector(".preview-hit-line").getBoundingClientRect();
      const box = scroller.getBoundingClientRect();
      return { scrolled: scroller.scrollTop, inView: marker.top >= box.top && marker.bottom <= box.bottom };
    });
    assert(placed.scrolled > 0 && placed.inView, `the preview scrolled to line 90: ${JSON.stringify(placed)}`);
    assert.match(await preview.locator(".preview-name").textContent(), /notes\.txt/);
    // The match within the line is marked too (the CSS highlight API).
    const marked = await page.evaluate(() => [...(CSS.highlights.get("kestrel-hit") ?? [])].map((range) => range.toString()));
    assert.deepEqual(marked, ["needle"]);
    await shotBoth("2-preview-at-match");
    await page.keyboard.press("Alt+p");
    await preview.waitFor({ state: "detached" });
  });

  await step("matches show in the tile, list and content views too", async () => {
    await panel.locator(".list-viewport").focus();
    for (const [keys, view] of [
      ["Control+Shift+7", "tiles"],
      ["Control+Shift+5", "list"],
      ["Control+Shift+8", "content"],
    ]) {
      await page.keyboard.press(keys);
      await panel.locator(`.filelist[data-view="${view}"]`).waitFor();
      await until(() => panel.locator(".view-item .hit-snippet").count(), 3, `matches in the ${view} view`);
      if (view === "tiles") await shotBoth("3-tiles");
    }
    await page.keyboard.press("Control+Shift+6");
    await panel.locator('.filelist[data-view="details"]').waitFor();
    await page.locator(".search-inside").click();
    await clearSearch();
  });

  await step("Search inside files is off again after a restart, while its options are kept", async () => {
    await page.locator(".search-inside").click();
    assert.equal(await page.locator(".search-inside").getAttribute("aria-pressed"), "true");
    // Settings reach local storage when they change; a reload stands in for a restart.
    await page.reload();
    await page.locator(".search-inside").waitFor();
    assert.equal(await page.locator(".search-inside").getAttribute("aria-pressed"), "false");
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("kestrel.search-options") ?? "{}"));
    assert.equal(stored.inside, false, "the mode itself is not remembered");
    assert.equal(stored.gitignore, true, "the other options are");
  });

  // ---------- phrases ----------

  await step("a phrase is read into chips, with the right results", async () => {
    await openFolder(sandbox);
    await search("code changed today in e2eproj");
    await panel.locator(".search-chip").first().waitFor();
    assert.deepEqual(await panel.locator(".search-chip-label").allTextContents(), ["Code", "Modified today", "In e2eproj"]);
    assert.equal((await reading()).query, `type:code dm:today in:"${project}"`);
    // The index takes a moment to hear of a new folder.
    await until(shownNames, ["main.rs"], "today's code in the project", 60_000);
    await shotBoth("4-phrase");
  });

  await step("a chip can be changed, and removed", async () => {
    await panel.locator(".search-chip-label", { hasText: "Modified today" }).click();
    const input = panel.locator(".search-chip-input");
    await input.waitFor();
    assert.equal(await input.inputValue(), "dm:today");
    await input.fill("dm:2020");
    await input.press("Enter");
    await until(shownNames, ["old.rs"], "code changed in 2020");
    assert.equal(await searchBox.inputValue(), `type:code dm:2020 in:"${project}"`);
    assert.deepEqual(await panel.locator(".search-chip-label").allTextContents(), ["Code", "Modified in 2020", "In e2eproj"]);
    await panel.locator('.search-chip[data-clause="type:code"] .search-chip-remove').click();
    await until(async () => (await searchBox.inputValue()), `dm:2020 in:"${project}"`, "the chip's clause gone");
    await until(shownNames, ["old.rs"], "everything changed in 2020 there");
    await clearSearch();
  });

  await step("photos taken in a year are found by the date in the photo", async () => {
    await openFolder(photos);
    await search("photos taken in 2023");
    await panel.locator(".search-chip").first().waitFor();
    assert.deepEqual(await panel.locator(".search-chip-label").allTextContents(), ["Pictures", "Taken in 2023"]);
    await searchDone();
    await until(shownNames, ["beach.jpg"], "the 2023 photo, not the 2024 one");
    await clearSearch();
  });

  // ---------- Downloads triage ----------

  const triage = page.locator(".triage-page");
  const triageRow = (name) => triage.locator(`.triage-row[data-path$="\\\\${name}"]`);
  const listed = () => triage.locator(".triage-row .triage-label").allTextContents();

  await step("the sidebar counts new downloads, and the page groups them by kind", async () => {
    await until(() => page.evaluate(() => window.__kestrelTriage.count()), 6, "six new downloads; the partial one isn't");
    assert.equal(await page.locator(".triage-entry .triage-badge").textContent(), "6");
    await page.locator(".triage-entry").click();
    await triage.waitFor();
    await triageRow("Invoice-2026-10.pdf").waitFor();
    const groups = await triage.locator(".triage-group").evaluateAll((sections) =>
      sections.map((section) => [section.dataset.kind, section.querySelectorAll(".triage-row").length]),
    );
    assert.deepEqual(groups, [
      ["installers", 1],
      ["archives", 1],
      ["documents", 1],
      ["images", 1],
      ["videos", 1],
      ["other", 1],
    ]);
    assert(!(await listed()).some((name) => name.includes("crdownload")), "still downloading: not listed");
    // Looking at the page clears the count.
    await until(() => page.evaluate(() => window.__kestrelTriage.count()), 0, "nothing new once looked at");
    assert.equal(await page.locator(".triage-entry .triage-badge").count(), 0);
    assert.equal(await triageRow("Invoice-2026-10.pdf").locator(".triage-action.suggested").getAttribute("data-destination"), documents);
    assert.equal(await triageRow("kestrel-test-IMG_2041.jpg").locator(".triage-action.suggested").getAttribute("data-destination"), pictures);
    assert.equal(await triageRow("kestrel-test-clip.mp4").locator(".triage-action.suggested").getAttribute("data-destination"), videos);
    await shotBoth("5-triage");
  });

  await step("a suggested move goes through the undo history, and is learnt", async () => {
    await triageRow("Invoice-2026-10.pdf").locator(".triage-action.suggested").click();
    await until(() => existsSync(join(documents, "Invoice-2026-10.pdf")), true, "moved to Documents");
    await triageRow("Invoice-2026-10.pdf").waitFor({ state: "detached" });
    await triage.focus();
    await page.keyboard.press("Control+z");
    await until(() => existsSync(join(downloads, "Invoice-2026-10.pdf")), true, "moved back by Undo");
    await triageRow("Invoice-2026-10.pdf").waitFor();
    // A similar invoice arrives: where the last one went is now learnt.
    write(join(downloads, "Invoice-2026-11.pdf"), "%PDF-1.4 another");
    const next = triageRow("Invoice-2026-11.pdf").locator(".triage-action.suggested");
    await next.waitFor({ timeout: 15_000 });
    assert.equal(await next.getAttribute("data-destination"), documents);
    assert.match(await next.getAttribute("title"), /^Where you moved files like this before/);
  });

  await step("several downloads are recycled at once and come back with Undo", async () => {
    await triageRow("kestrel-test-IMG_2041.jpg").locator(".triage-check").check();
    await triageRow("kestrel-test-clip.mp4").locator(".triage-check").check();
    const bulk = triage.locator(".triage-bulk");
    assert.equal(await bulk.locator(".triage-bulk-count").textContent(), "2 items chosen");
    await shotBoth("6-triage-chosen");
    await bulk.locator(".triage-action.danger").click();
    await until(() => [existsSync(join(downloads, "kestrel-test-IMG_2041.jpg")), existsSync(join(downloads, "kestrel-test-clip.mp4"))], [false, false], "both recycled");
    await until(recycled, recycledBefore + 2, "both in the Recycle Bin");
    await triage.focus();
    await page.keyboard.press("Control+z");
    await until(() => [existsSync(join(downloads, "kestrel-test-IMG_2041.jpg")), existsSync(join(downloads, "kestrel-test-clip.mp4"))], [true, true], "both back");
    await until(recycled, recycledBefore, "the Recycle Bin as it was");
    await triageRow("kestrel-test-clip.mp4").waitFor();
  });

  await step("an archive is extracted through the archive feature's own dialog", async () => {
    const preferences = await invoke("archive_preferences");
    try {
      await triageRow("kestrel-test-photos.zip").locator(".triage-action", { hasText: "Extract…" }).click();
      const dialog = page.locator(".archive-extract");
      await dialog.waitFor();
      assert.equal(await dialog.locator("#archive-destination").inputValue(), join(downloads, "kestrel-test-photos"));
      const show = dialog.locator(".dialog-check input");
      if (await show.isChecked()) await show.uncheck();
      await dialog.locator(".dialog-button.primary").click();
      await until(() => existsSync(join(downloads, "kestrel-test-photos", "inside.txt")), true, "extracted beside the archive");
      // What it made arrives as a new item.
      await triageRow("kestrel-test-photos").waitFor();
    } finally {
      await invoke("archive_set_preferences", { preferences });
    }
  });

  await step("a download arriving while elsewhere is counted again", async () => {
    await openFolder(sandbox);
    write(join(downloads, "arrived-later.txt"), "new");
    await until(async () => (await page.evaluate(() => window.__kestrelTriage.count())) >= 1, true, "counted", 15_000);
    await page.locator(".triage-entry .triage-badge").waitFor();
  });

  // ---------- WSL ----------

  await step("a distribution shows in the sidebar and This PC, and browses as Linux would", async () => {
    await page.evaluate(() => window.__kestrelWsl.reload());
    const place = page.locator('.sidebar section[aria-label="Linux"] .wsl-place');
    await place.waitFor();
    assert.equal(await place.textContent(), "Ubuntu");
    await openFolder("This PC");
    const tile = page.locator('.pc-group[aria-label="Linux"] .pc-tile');
    await tile.waitFor();
    assert.equal(await tile.locator(".pc-tile-name").textContent(), "Ubuntu");
    await shotBoth("7-this-pc-linux");
    await openFolder(linuxHome);
    await panel.locator(".row").first().waitFor();
    // Dot files are hidden, as `ls` hides them; a name WSL encoded reads as Linux has it.
    await until(() => panel.locator(".row .label").allTextContents(), ["notes:todo.txt", "readme.md"], "the home folder");
    await shotBoth("8-wsl-folder");
  });

  await step("Open in Terminal there starts the distribution's shell in that folder", async () => {
    await invoke("projects_probe", { on: true, allow: [] });
    try {
      await panel.locator(".list-viewport").click({ button: "right", position: { x: 300, y: 200 } });
      await page.locator(".menu .menu-item", { hasText: "Open in Terminal" }).click();
      let launches = [];
      for (const deadline = Date.now() + 10_000; launches.length === 0 && Date.now() < deadline; ) {
        launches = await invoke("projects_probe_take");
        if (launches.length === 0) await page.waitForTimeout(150);
      }
      assert.equal(launches.length, 1, "one launch recorded");
      assert.match(launches[0].params, /wsl\.exe -d Ubuntu --cd \/home\/levi$/);
    } finally {
      await invoke("projects_probe", { on: false, allow: [] });
    }
  });

  await step("without WSL, nothing about it shows", async () => {
    await invoke("wsl_dev_distros", { distros: [] });
    await page.evaluate(() => window.__kestrelWsl.reload());
    await page.locator('.sidebar section[aria-label="Linux"]').waitFor({ state: "detached" });
    await openFolder("This PC");
    await page.locator(".pc-tile").first().waitFor();
    assert.equal(await page.locator('.pc-group[aria-label="Linux"]').count(), 0);
  });

  assert.deepEqual(errors, [], "no errors on the page");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  await invoke("search_dev_places", { folders: {}, projects: [] }).catch(() => {});
  await invoke("wsl_dev_distros", { distros: null }).catch(() => {});
  await invoke("triage_dev_seen", { at: null }).catch(() => {});
  await invoke("triage_dev_forget", { folder: sandbox }).catch(() => {});
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await page.reload().catch(() => {});
  await browser.close();
  if (recycled() > recycledBefore) console.warn("Some scratch items are still in the Recycle Bin.");
  rmSync(sandbox, { recursive: true, force: true });
  if (existsSync(sandbox)) console.warn(`Could not remove ${sandbox}: ${readdirSync(sandbox).join(", ")}`);
}
