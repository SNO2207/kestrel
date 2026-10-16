// End-to-end test of where the features built side by side meet, against the real window.
//
// The command palette, the shortcuts (pins, frequent folders, saved searches, tags,
// workspaces), the projects hub and the isolated preview were each tested on their own. This
// suite checks what joins them: the palette's projects (`@`), tags (`#`), pins, frequent
// folders, saved searches and workspaces; the padlock, the git mark and the tag dots on the
// same items in every view; the projects hub closing when a pin is opened; the preview's own
// webview stepping aside for the palette, Go to folder and the hub; keys typed into the palette
// staying there; and the quick launcher still being allowed the app's commands.
//
// Files: everything happens in a scratch folder under %TEMP% that this script creates and
// removes: a git repository made with the git command line, one of whose folders is made
// unreadable with icacls and given its permissions back. Projects in %TEMP% are found only
// through the development hook, as in projects.mjs. The pins, tags, saved searches and
// workspaces the suite makes are removed again, and so is its visit to C:\Windows\Help if it
// wasn't recorded before. The window's view, preview and projects settings (local storage)
// are saved first and put back at the end. Screenshots go to e2e/out/connections.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/connections.mjs`. It needs a development build.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { png } from "./fixtures.mjs";
import { appUrl } from "./slot.mjs";
import { webviews } from "./windows.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "connections");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

/** A word found nowhere else on the drive. */
const token = `kcon${Date.now().toString(36)}`;
const sandbox = join(tmpdir(), `kestrel-e2e-connections-${process.pid}`);
const repo = join(sandbox, `${token}-app`);
const privateFolder = join(repo, "private");
const HELP = String.raw`C:\Windows\Help`;
const TAG = `${token}-urgent`;
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(repo, "src"), { recursive: true });
mkdirSync(privateFolder);
const gitConfig = join(sandbox, "gitconfig");
writeFileSync(gitConfig, "");

function git(...args) {
  return execFileSync("git", ["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", ...args], {
    cwd: repo,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    env: {
      ...process.env,
      GIT_CONFIG_GLOBAL: gitConfig,
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_AUTHOR_NAME: "Kestrel Tester",
      GIT_AUTHOR_EMAIL: "tester@example.com",
      GIT_COMMITTER_NAME: "Kestrel Tester",
      GIT_COMMITTER_EMAIL: "tester@example.com",
    },
  });
}

writeFileSync(join(repo, "Cargo.toml"), `[package]\nname = "${token}"\nversion = "0.1.0"\n`);
writeFileSync(join(repo, "src", "main.rs"), 'fn main() {\n    println!("hello");\n}\n');
writeFileSync(join(repo, "photo.png"), png(160, 120, (x, y) => [x + 60, y + 80, 200]));
git("init", "-q");
git("add", ".");
git("commit", "-q", "-m", "Initial commit");
// One folder with a modified file inside, one untracked file.
writeFileSync(join(repo, "src", "main.rs"), 'fn main() {\n    println!("changed");\n}\n');
writeFileSync(join(repo, "notes.txt"), "untracked\n");
const icacls = (...args) => execFileSync("icacls", [privateFolder, ...args], { stdio: "ignore" });

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open", "preview-width", "kestrel.projects"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate((keys) => keys.forEach((key) => localStorage.removeItem(key)), STORAGE_KEYS);
await page.reload();

const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
/** A details row or an item of the other views, by the name it shows. */
const item = (name) => panel.locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const activePath = () => page.locator(".tab.active").getAttribute("title");
const searchBox = page.locator(".searchbox input");
const hub = page.locator(".projects-hub");
const palette = page.locator(".palette");
const paletteItems = palette.locator(".palette-item");
const step = (await import("./steps.mjs")).stepper("connections", () => page, { timed: true });
const until = async (check, what, timeout = 10_000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await check()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting until ${what}`);
    await page.waitForTimeout(100);
  }
};
const samePath = (a, b) => (a ?? "").toLowerCase() === (b ?? "").toLowerCase();

async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

async function goTo(path) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await until(async () => samePath(await activePath(), path), `${path} opens`);
  await panel.locator(".row, .view-item").first().waitFor();
}

// ---------- the palette ----------

async function openPalette() {
  if ((await palette.count()) === 0) await page.keyboard.press("Control+k");
  await palette.locator("input").waitFor();
}

async function closePalette() {
  if ((await palette.count()) > 0) await page.keyboard.press("Escape");
  await palette.waitFor({ state: "detached" });
}

/**
 * Types `text` into the palette until a hit titled `title` from `provider` is listed (the
 * index and the backend catch up within moments), and returns that hit's element.
 */
async function findInPalette(text, title, provider, timeout = 15_000) {
  await openPalette();
  const input = palette.locator("input");
  const deadline = Date.now() + timeout;
  for (;;) {
    await input.fill("");
    await input.fill(text);
    await palette.locator('.palette-list[aria-busy="false"]').waitFor();
    const hit = palette.locator(`.palette-item[data-provider="${provider}"]`).filter({ has: page.locator(".palette-title", { hasText: exactly(title) }) });
    if ((await hit.count()) > 0) return hit.first();
    if (Date.now() > deadline) {
      const seen = await paletteItems.evaluateAll((els) => els.map((el) => `${el.dataset.provider}:${el.querySelector(".palette-title")?.textContent}`));
      throw new Error(`no ${provider} hit "${title}" for "${text}"; saw ${JSON.stringify(seen)}`);
    }
    await page.waitForTimeout(250);
  }
}

/** Chooses a hit with Enter, Ctrl+Enter or Shift+Enter, by moving the selection onto it. */
async function choose(hit, modifier = "") {
  const index = Number(await hit.getAttribute("data-index"));
  const selected = Number(await palette.locator(".palette-item.active").getAttribute("data-index"));
  for (let at = selected; at < index; at++) await page.keyboard.press("ArrowDown");
  await page.keyboard.press(modifier ? `${modifier}+Enter` : "Enter");
  await palette.waitFor({ state: "detached" });
}

/** The preview's webview as Windows shows it, waited for until `wanted` holds. */
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

// ---------- what the suite makes, to remove again ----------

const made = { pins: [], tags: [], searches: [], workspaces: [], helpVisited: false };
let unlocked = true;

try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
  await invoke("projects_probe", { on: true, allow: [sandbox] });

  await step("every view shows the padlock, the git mark and the tag dots on the same items", async () => {
    const tag = await invoke("tags_create", { name: TAG, color: "red" });
    made.tags.push(tag.id);
    await invoke("tags_assign", { paths: [join(repo, "Cargo.toml")], tag: tag.id, on: true });
    icacls("/inheritance:r");
    unlocked = false;
    await goTo(repo);
    // Access answers are remembered per folder; F5 asks again, as after changing permissions.
    await page.keyboard.press("F5");
    const modes = [
      ["details", "Control+Shift+6"],
      ["list", "Control+Shift+5"],
      // Explorer's Tiles; Ctrl+Shift+8 is its Content view.
      ["tiles", "Control+Shift+7"],
      ["small", "Control+Shift+4"],
      ["medium", "Control+Shift+3"],
      ["large", "Control+Shift+2"],
      ["xl", "Control+Shift+1"],
    ];
    for (const [mode, keys] of modes) {
      await list.focus();
      await page.keyboard.press(keys);
      await panel.locator(`.filelist[data-view="${mode}"]`).waitFor();
      await item("private").locator(".lock-glyph").waitFor({ timeout: 15_000 });
      await item("src").locator('.git-glyph[data-change="modified"]').waitFor({ timeout: 30_000 });
      await item("notes.txt").locator('.git-glyph[data-change="untracked"]').waitFor();
      await item("Cargo.toml").locator(`.tag-dot[data-tag="${TAG}"]`).waitFor();
      assert.equal(await item("Cargo.toml").locator(".git-glyph").count(), 0, `${mode}: an unchanged file has no git mark`);
      assert.equal(await item("notes.txt").locator(".tag-dot, .lock-glyph").count(), 0, `${mode}: nothing else on notes.txt`);
      if (["details", "list", "tiles", "large"].includes(mode)) await shotBoth(`1-marks-${mode}`);
    }
    await list.focus();
    await page.keyboard.press("Control+Shift+6");
    await panel.locator('.filelist[data-view="details"]').waitFor();
    icacls("/reset");
    unlocked = true;
  });

  await step("@ finds the project; Enter opens its folder and Shift+Enter its page in the hub", async () => {
    await goTo(sandbox);
    let hit = await findInPalette(`@${token}`, `${token}-app`, "projects", 60_000);
    assert.equal(await hit.locator(".palette-kind").textContent(), "Project");
    assert.match(await palette.locator(".palette-footer").innerText(), /Shift\+Enter\s+show in Projects/);
    await shotBoth("2-projects-in-palette");
    await choose(hit);
    await until(async () => samePath(await activePath(), repo), "the project's folder opens");
    hit = await findInPalette(`@${token}`, `${token}-app`, "projects");
    await choose(hit, "Shift");
    await hub.waitFor();
    await page.locator(`.project-detail[data-root="${repo.replace(/\\/g, "\\\\")}"]`).waitFor();
    await shotBoth("3-project-from-palette");
  });

  await step("a pin opened from the sidebar closes the hub; the palette lists pins with their keys", async () => {
    assert.equal(await hub.count(), 1, "the hub is still open");
    const result = await invoke("pins_add", { paths: [repo], group: null });
    assert.equal(result.added, 1);
    const board = await invoke("pins_board");
    const pin = board.pins.find((each) => samePath(each.path, repo));
    made.pins.push(pin.id);
    await invoke("pins_set_alias", { id: pin.id, alias: `${token} pinned` });
    await page.evaluate(() => window.__kestrelShortcuts.refresh());
    // The hub covers the project's own folder, which the pin opens: nothing changes but the hub
    // going away.
    assert(samePath(await activePath(), repo));
    await page.locator(`.shelf-item.pin[data-pin-id="${pin.id}"]`).click();
    await hub.waitFor({ state: "detached" });
    await until(async () => samePath(await activePath(), repo), "the pinned folder opens");

    await goTo(sandbox);
    const hit = await findInPalette(`${token} pinned`, `${token} pinned`, "pins");
    const key = board.pins.find((each) => each.id === pin.id).key;
    if (key) assert.equal(await hit.locator(".palette-hint").textContent(), `Ctrl+${key}`);
    else assert.equal(await hit.locator(".palette-kind").textContent(), "Pinned folder");
    await choose(hit);
    await until(async () => samePath(await activePath(), repo), "the pin opens from the palette");
  });

  await step("# lists the tags; choosing one shows everything that carries it", async () => {
    const hit = await findInPalette(`#${token}`, TAG, "tags");
    assert.equal(await hit.locator(".palette-subtitle").textContent(), "1 item");
    assert.equal(await hit.locator(`.tag-dot[data-color="red"]`).count(), 1, "the tag's colour is its icon");
    await shotBoth("4-tags-in-palette");
    await choose(hit);
    await until(async () => (await searchBox.inputValue()) === `tag:${TAG}`, "the tag's search runs");
    await item("Cargo.toml").waitFor({ timeout: 15_000 });
    await searchBox.fill("");
    await list.focus();
  });

  await step("saved searches and workspaces come up in the palette and run", async () => {
    const saved = await invoke("searches_save", { name: `${token} saved`, query: `${token}-app`, scope: null });
    made.searches.push(saved.id);
    let hit = await findInPalette(`${token} saved`, `${token} saved`, "searches");
    assert.equal(await hit.locator(".palette-subtitle").textContent(), `Everywhere: ${token}-app`);
    await choose(hit);
    await until(async () => (await searchBox.inputValue()) === `${token}-app`, "the saved search runs");
    await item(`${token}-app`).waitFor({ timeout: 15_000 });
    await searchBox.fill("");

    const tabs = [
      { path: repo, view: null, sort: null },
      { path: sandbox, view: null, sort: null },
    ];
    const workspace = await invoke("workspaces_save", { name: `${token} desk`, tabs, active: 1 });
    made.workspaces.push(workspace.id);
    hit = await findInPalette(`${token} desk`, `${token} desk`, "workspaces");
    assert.equal(await hit.locator(".palette-subtitle").textContent(), `2 tabs: ${token}-app, kestrel-e2e-connections-${process.pid}`);
    await choose(hit);
    await until(async () => (await page.locator(".tab").count()) === 2 && samePath(await activePath(), sandbox), "the workspace's tabs open");
  });

  await step("frequent folders take part in the palette's answers", async () => {
    const before = await invoke("frecency_top", { limit: 1000 });
    made.helpVisited = !before.some((folder) => samePath(folder.path, HELP));
    assert.equal(await invoke("frecency_visit", { path: HELP }), true);
    const request = { text: "windows help", session: "e2e", items: [], show_hidden: false, limit: 50 };
    const answer = await invoke("palette_query", { request });
    assert(answer.providers.includes("frequent"), answer.providers.join(", "));
    const help = answer.hits.filter((hit) => samePath(hit.path, HELP));
    assert.equal(help.length, 1, "listed once, whichever providers offer it");
  });

  await step("keys typed into the palette stay there", async () => {
    const tabs = await page.locator(".tab").count();
    const before = await activePath();
    await openPalette();
    await page.keyboard.press("Control+t");
    await page.keyboard.press("Control+1");
    await page.keyboard.press("Alt+p");
    await page.waitForTimeout(300);
    assert.equal(await page.locator(".tab").count(), tabs, "no new tab");
    assert.equal(await activePath(), before, "no pin opened");
    assert.equal(await page.locator(".preview-pane").count(), 0, "the preview pane stayed closed");
    await closePalette();
  });

  await step("the preview's webview steps aside for the palette, Go to folder and the hub", async () => {
    await goTo(repo);
    await list.focus();
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor();
    await item("photo.png").click();
    await page.locator(".preview-pane .preview-surface[data-kind='image']").waitFor();
    await previewView((view) => view.visible, "shown over the pane");
    for (const [keys, overlay, closing] of [
      ["Control+k", ".palette-backdrop", "Escape"],
      ["Control+g", ".goto-backdrop", "Escape"],
      ["Control+Shift+P", ".projects-hub", "Control+Shift+P"],
    ]) {
      await page.keyboard.press(keys);
      await page.locator(overlay).waitFor();
      await previewView((view) => !view.visible, `hidden under ${overlay}`);
      if (overlay === ".palette-backdrop") await shotBoth("5-palette-over-preview");
      await page.keyboard.press(closing);
      await page.locator(overlay).waitFor({ state: "detached" });
      await previewView((view) => view.visible, `shown again after ${overlay}`);
      await list.focus();
    }
    await page.keyboard.press("Alt+p");
    await page.locator(".preview-pane").waitFor({ state: "detached" });
  });

  await step("the quick launcher may use the app's commands, projects included", async () => {
    const quick = browser
      .contexts()
      .flatMap((context) => context.pages())
      .find((candidate) => candidate.url().startsWith(`${appUrl}/quick.html`));
    assert(quick, "the quick launcher's page is there");
    assert.equal(await quick.title(), "Kestrel quick launcher");
    const answer = await quick.evaluate(
      (text) => window.__TAURI_INTERNALS__.invoke("palette_query", { request: { text, session: "e2e-quick", items: [], show_hidden: false } }),
      `@${token}`,
    );
    assert.deepEqual(
      answer.hits.map((hit) => hit.title),
      [`${token}-app`],
    );
  });

  console.log(`
all checks passed; screenshots are in ${out}`);
} finally {
  if (!unlocked) icacls("/reset");
  if ((await palette.count()) > 0) await page.keyboard.press("Escape").catch(() => {});
  if ((await hub.count()) > 0) await page.keyboard.press("Control+Shift+P").catch(() => {});
  for (const id of made.pins) await invoke("pins_remove", { id }).catch(() => {});
  for (const id of made.tags) await invoke("tags_delete", { id }).catch(() => {});
  for (const id of made.searches) await invoke("searches_delete", { id }).catch(() => {});
  for (const id of made.workspaces) await invoke("workspaces_delete", { id }).catch(() => {});
  if (made.helpVisited) await invoke("frecency_forget", { path: HELP }).catch(() => {});
  await invoke("project_set_dismissed", { root: repo, dismissed: false }).catch(() => {});
  await invoke("projects_probe", { on: false, allow: [] }).catch(() => {});
  await page.evaluate(
    (saved) => saved.forEach(([key, value]) => (value === null ? localStorage.removeItem(key) : localStorage.setItem(key, value))),
    savedStorage,
  );
  // The palette remembers what was chosen. It drops what is gone on its own, but the scratch
  // folders' entries need not linger.
  for (const [provider, id] of [["projects", repo], ["files", repo], ["files", sandbox]]) await invoke("palette_forget", { provider, id }).catch(() => {});
  await page.reload();
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
