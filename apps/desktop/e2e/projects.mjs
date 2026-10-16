// End-to-end test of the projects hub and the git badges of folder views.
//
// Everything happens in a scratch folder under %TEMP% that this script creates and removes:
// a repository made with the git command line (one commit ahead of and one behind a bare
// remote, with staged, modified, untracked and ignored files, a nested node_modules and an
// embedded repository), a repository in the middle of a conflicted merge, and a project
// without git. Nothing outside the sandbox is written to.
//
// The temp folder lies under AppData, which project discovery leaves out, so the suite turns
// on the development hook first (dev builds only): it lets projects be found in the sandbox,
// and records what Run and the launchers would start instead of starting it. Opening real
// programs (VS Code, Windows Terminal, the browser) is a manual check.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/projects.mjs`. Screenshots go to e2e/out/projects, dark and light.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "projects");
mkdirSync(out, { recursive: true });

// ---------- the fixture ----------

const sandboxName = `kestrel-e2e-projects-${process.pid}`;
const sandbox = join(tmpdir(), sandboxName);
const work = join(sandbox, "work");
const app = join(work, "fixture-app");
const plain = join(work, "fixture-plain");
const merge = join(work, "fixture-merge");
const late = join(work, "fixture-late");
const outside = join(sandbox, "outside");
const OUTSIDE_FOLDERS = 20;
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(work, { recursive: true });
const gitConfig = join(sandbox, "gitconfig");
writeFileSync(gitConfig, "");

/** Runs git for the fixture, isolated from this machine's git configuration. */
function git(cwd, ...args) {
  return execFileSync("git", ["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", ...args], {
    cwd,
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

function write(path, text) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

function makeFixture() {
  const remote = join(sandbox, "remote.git");
  git(sandbox, "init", "--bare", "-q", remote);

  write(join(app, "Cargo.toml"), '[package]\nname = "fixture-app"\nversion = "0.1.0"\n');
  write(join(app, "src", "main.rs"), 'fn main() {\n    println!("hello");\n}\n');
  write(join(app, "README.md"), "# Fixture app\n\nA project made by the **projects** suite.\n");
  write(join(app, "package.json"), JSON.stringify({ name: "fixture-app", scripts: { dev: "vite", build: "tsc -b" } }, null, 2));
  write(join(app, "App.sln"), "Microsoft Visual Studio Solution File, Format Version 12.00\n");
  write(join(app, ".gitignore"), "node_modules/\n*.log\ntarget/\n");
  // An embedded repository, recorded as a gitlink as a submodule is.
  const inner = join(app, "libs", "inner");
  write(join(inner, "Cargo.toml"), '[package]\nname = "inner"\n');
  git(inner, "init", "-q");
  git(inner, "add", ".");
  git(inner, "commit", "-q", "-m", "Inner commit");
  git(app, "init", "-q");
  git(app, "add", ".");
  git(app, "-c", "advice.addEmbeddedRepo=false", "add", "libs/inner");
  git(app, "commit", "-q", "-m", "Initial commit");
  git(app, "remote", "add", "origin", remote);
  git(app, "push", "-q", "-u", "origin", "main");

  // Another clone pushes a commit the app has not seen; the app commits on its own and fetches.
  git(sandbox, "clone", "-q", remote, "other");
  write(join(sandbox, "other", "CHANGELOG.md"), "Changes\n");
  git(join(sandbox, "other"), "add", ".");
  git(join(sandbox, "other"), "commit", "-q", "-m", "Commit from elsewhere");
  git(join(sandbox, "other"), "push", "-q");
  write(join(app, "src", "lib.rs"), "pub fn lib() {}\n");
  git(app, "add", ".");
  git(app, "commit", "-q", "-m", "Add the library");
  git(app, "fetch", "-q");
  git(app, "remote", "set-url", "origin", "git@github.com:kestrel-test/fixture-app.git");

  // One staged, one modified, one untracked; ignored log, dependencies and build output.
  write(join(app, "src", "main.rs"), 'fn main() {\n    println!("changed");\n}\n');
  write(join(app, "src", "staged.rs"), "pub fn staged() {}\n");
  git(app, "add", "src/staged.rs");
  write(join(app, "notes.txt"), "untracked\n");
  write(join(app, "debug.log"), "ignored\n");
  // Big enough to occupy clusters of their own (tiny files live inside the file table).
  write(join(app, "node_modules", "dep", "index.js"), `module.exports = ${JSON.stringify("x".repeat(9000))};\n`);
  write(join(app, "node_modules", "dep", "package.json"), '{"name":"dep"}');
  write(join(app, "target", "debug", "fixture-app.exe"), "x".repeat(40_000));

  // A merge that stopped on a conflict.
  write(join(merge, "notes.md"), "line\n");
  git(merge, "init", "-q");
  git(merge, "add", ".");
  git(merge, "commit", "-q", "-m", "Base");
  git(merge, "checkout", "-q", "-b", "theirs");
  write(join(merge, "notes.md"), "theirs\n");
  git(merge, "commit", "-q", "-am", "Theirs");
  git(merge, "checkout", "-q", "main");
  write(join(merge, "notes.md"), "ours\n");
  git(merge, "commit", "-q", "-am", "Ours");
  assert.throws(() => git(merge, "merge", "-q", "theirs"), "the merge stops on a conflict");

  // A project without git.
  write(join(plain, "pyproject.toml"), '[project]\nname = "fixture-plain"\n');
  write(join(plain, "main.py"), "print('hi')\n");
  write(join(plain, "justfile"), "fmt:\n    ruff format\n");

  // Plain folders outside any repository, for browsing through.
  for (let n = 0; n < OUTSIDE_FOLDERS; n++) {
    for (let f = 0; f < 5; f++) write(join(outside, `folder-${n}`, `file-${n}-${f}.txt`), "x\n");
  }
  for (let dir = sandbox; dirname(dir) !== dir; dir = dirname(dir)) {
    assert(!existsSync(join(dir, ".git")), `the sandbox must not be inside a repository (${dir})`);
  }
}

makeFixture();

// ---------- the window ----------

const { browser, page } = await connectToKestrel();
const session = await page.context().newCDPSession(page);
const STORAGE_KEYS = ["kestrel.projects", "kestrel.views"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.evaluate(() => localStorage.removeItem("kestrel.projects"));
await page.reload();

const invoke = (command, args = {}) =>
  page.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const exactly = (text) => new RegExp(`^${escape(text)}$`);
const hub = page.locator(".projects-hub");
const card = (name) => page.locator(".project-card").filter({ has: page.locator(".project-name", { hasText: exactly(name) }) });
const detail = page.locator(".project-detail");
const fact = (name) => detail.locator(`.fact[data-fact="${name}"] dd`);
const panel = page.locator(".tab-panel.active");
const item = (name) => panel.locator(".row, .view-item").filter({ has: page.locator(".label", { hasText: exactly(name) }) });
const step = async (title, run) => {
  const started = Date.now();
  await run();
  console.log(`ok  ${title} (${Date.now() - started} ms)`);
};

/** Captured through the debugging protocol, in both themes. */
async function shotBoth(name) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(150);
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${name}-${scheme}.png`), Buffer.from(data, "base64"));
  }
  await page.emulateMedia({ colorScheme: null });
}

async function openHub() {
  if (!(await hub.count())) await page.keyboard.press("Control+Shift+P");
  await hub.waitFor();
  await page.locator(".projects-filter").fill(sandboxName);
}

/** Launches recorded by the development hook since the last call. */
async function launches(count = 1) {
  for (let attempt = 0; attempt < 100; attempt++) {
    const records = await invoke("projects_probe_take");
    if (records.length >= count) return records;
    await page.waitForTimeout(50);
  }
  throw new Error("nothing was launched");
}

async function openFolder(path, expectName) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await item(expectName).first().waitFor();
}

let cleanedUp = false;
async function resetProjectData() {
  for (const root of [app, plain, merge, late]) {
    await invoke("project_set_status", { root, status: "active" }).catch(() => {});
    await invoke("project_set_tags", { root, tags: [] }).catch(() => {});
    await invoke("project_set_notes", { root, notes: "" }).catch(() => {});
    await invoke("project_set_pinned", { root, pinned: false }).catch(() => {});
    await invoke("project_set_dismissed", { root, dismissed: false }).catch(() => {});
  }
}

try {
  await panel.locator(".row").first().waitFor();
  await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
  await invoke("projects_probe", { on: true, allow: [work] });

  await step("Ctrl+Shift+P opens the hub, and the fixture projects appear in it", async () => {
    await page.keyboard.press("Control+Shift+P");
    await hub.waitFor();
    assert.equal(await page.locator(".projects-entry.current").count(), 1, "the sidebar entry shows the hub is open");
    assert.equal(await page.locator(".sidebar .place.current").count(), 0, "no folder is current while the hub covers the list");
    await page.locator(".projects-filter").fill(sandboxName);
    for (const name of ["fixture-app", "fixture-plain", "fixture-merge"]) await card(name).waitFor({ timeout: 60_000 });
    assert.equal(await page.locator(".project-card").count(), 3, "only the sandbox's projects, nothing nested in them");
    assert.match(await page.locator(".projects-count").innerText(), /^3 projects/);
  });

  await step("cards show each project's facts", async () => {
    const c = card("fixture-app");
    await c.locator(".project-branch").waitFor({ timeout: 30_000 });
    assert.equal(await c.locator(".project-path").innerText(), app);
    assert.equal(await c.locator(".project-status").innerText(), "Active");
    const stack = await c.locator(".project-stack .chip").allInnerTexts();
    assert(stack.includes("Rust") && stack.includes("Node.js"), `stack: ${stack}`);
    assert.equal(await c.locator(".project-branch").innerText(), "main");
    assert.equal(await c.locator(".project-sync").innerText(), "1 ahead, 1 behind");
    assert.equal(await c.locator(".project-changes").innerText(), "1 staged, 1 modified, 1 untracked");
    assert.equal(await c.locator(".project-commit-subject").innerText(), "Add the library");
    assert.match(await c.locator(".project-commit-by").innerText(), /^Kestrel Tester, (just now|\d+ minutes? ago)$/);
    assert.match(await c.locator(".project-activity").innerText(), /^Active (just now|\d+ minutes? ago)$/);
    assert.match(await c.locator(".project-size").innerText(), /^\d+(\.\d)? (KB|MB)$/);
    assert.match(await c.locator(".project-reclaimable").innerText(), /^\d+(\.\d)? KB reclaimable$/);

    const p = card("fixture-plain");
    assert.deepEqual(await p.locator(".project-stack .chip").allInnerTexts(), ["Python"]);
    assert.equal(await p.locator(".project-git").count(), 0, "no git line without git");

    const m = card("fixture-merge");
    await m.locator(".project-changes").waitFor({ timeout: 30_000 });
    assert.equal(await m.locator(".project-changes").innerText(), "1 conflicted");
    assert.equal(await m.locator(".project-sync").innerText(), "Not on a remote");
    await shotBoth("1-cards");
  });

  await step("the detail panel shows every fact, the README, scripts, activity and recent files", async () => {
    await card("fixture-app").click();
    await detail.locator(".heatmap").waitFor({ timeout: 20_000 });
    assert.equal(await detail.locator("h2").innerText(), "fixture-app");
    assert.match(await fact("branch").innerText(), /^main tracking origin\/main\s+1 ahead, 1 behind$/);
    assert.equal(await fact("changes").innerText(), "1 staged, 1 modified, 1 untracked");
    assert.match(await fact("commit").innerText(), /^Add the library\s+Kestrel Tester, /);
    assert.equal(await fact("remote").innerText(), "github.com/kestrel-test/fixture-app");
    assert.match(await fact("activity").innerText(), /\(just now|minutes? ago\)/);
    assert.match(await fact("size").innerText(), /^\d+(\.\d)? (KB|MB)$/);
    const reclaimable = await fact("reclaimable").innerText();
    assert.match(reclaimable, /in build output/);
    assert.match(reclaimable, /target/);
    assert.match(reclaimable, /node_modules/);
    const languages = await fact("stack").locator(".language").allInnerTexts();
    assert.deepEqual(languages, ["Rust 100%"]);

    assert.equal(await detail.locator(".project-readme .preview-markdown h1").innerText(), "Fixture app");
    assert.equal(await detail.locator(".project-readme .preview-markdown strong").innerText(), "projects");
    const scripts = await detail.locator(".script").evaluateAll((items) => items.map((li) => li.dataset.command));
    assert.deepEqual(scripts, ["npm run dev", "npm run build", "cargo build", "cargo test", "cargo run"]);

    const today = await detail.locator(".heatmap-cell[data-commits]:not([data-commits='0'])").evaluateAll((cells) =>
      cells.map((cell) => [Number(cell.dataset.commits), Number(cell.dataset.files)]),
    );
    assert.equal(today.length, 1, "all the activity is today");
    assert.equal(today[0][0], 2, "two commits on this branch");
    assert(today[0][1] >= 8, `files changed today: ${today[0][1]}`);
    assert.match(await detail.locator(".heatmap-caption").innerText(), /^2 commits and \d+ file changes in the last year/);

    const recent = await detail.locator(".recent-file").allInnerTexts();
    assert(recent.includes("notes.txt") && recent.includes("src\\main.rs"), `recent: ${recent}`);
    assert(!recent.some((name) => name.endsWith(".log") || name.startsWith("node_modules") || name.startsWith("target")), `recent: ${recent}`);
    await shotBoth("2-detail");
    await detail.evaluate((el) => (el.scrollTop = el.scrollHeight));
    await shotBoth("3-detail-end");
    await detail.evaluate((el) => (el.scrollTop = 0));
  });

  await step("Run starts a script in a terminal in the project folder (recorded by the hook)", async () => {
    await invoke("projects_probe_take");
    await detail.locator(".script", { has: page.locator(".script-name", { hasText: exactly("build") }) }).first().locator(".script-run").click();
    const [record] = await launches();
    assert.equal(record.dir, app);
    const quoted = app.includes(" ") ? `"${app}"` : app;
    // Windows Terminal when installed, otherwise a console window.
    if (/wt\.exe$/i.test(record.file)) assert.equal(record.params, `-d ${quoted} cmd /k npm run build`);
    else assert.equal(record.params, "/k npm run build");
  });

  await step("launchers open the project elsewhere (recorded by the hook)", async () => {
    const available = await detail.locator(".launcher").evaluateAll((buttons) => buttons.map((b) => b.dataset.launcher));
    for (const expected of ["terminal", "kestrel", "explorer", "visual-studio", "web"]) {
      assert(available.includes(expected), `launchers: ${available}`);
    }
    const quoted = app.includes(" ") ? `"${app}"` : app;
    const expectations = {
      vscode: (r) => /Code\.exe$/i.test(r.file) && r.params === quoted,
      "visual-studio": (r) => r.file === join(app, "App.sln"),
      terminal: (r) => (/wt\.exe$/i.test(r.file) ? r.params === `-d ${quoted}` : r.file === "cmd.exe"),
      // Claude Code from the PATH only, never a claude.cmd in the project folder.
      claude: (r) => r.params.endsWith("/k set NoDefaultCurrentDirectoryInExePath=1& claude& set NoDefaultCurrentDirectoryInExePath="),
      explorer: (r) => r.file === "explorer.exe" && r.params === quoted,
      web: (r) => r.file === "https://github.com/kestrel-test/fixture-app",
    };
    for (const [launcher, check] of Object.entries(expectations)) {
      if (!available.includes(launcher)) {
        console.log(`    (${launcher} is not installed here, so it is not offered)`);
        continue;
      }
      await detail.locator(`.launcher[data-launcher="${launcher}"]`).click();
      const [record] = await launches();
      assert(check(record), `${launcher}: ${JSON.stringify(record)}`);
      assert.equal(record.dir, app);
    }
  });

  await step("status, tags and notes are kept across a reload", async () => {
    await detail.locator(".project-status-select").selectOption("paused");
    await detail.locator(".tag-input").fill("client");
    await detail.locator(".tag-input").press("Enter");
    await detail.locator(".tag", { hasText: "client" }).waitFor();
    await detail.locator(".pin-toggle").click();
    await detail.locator(".pin-toggle[aria-pressed='true']").waitFor();
    const notes = "# Plan\n\nShip the **fixture** first.";
    await detail.locator(".notes-input").fill(`${notes}x`);
    // Backspace edits the notes; it must not also go back in the folder hidden underneath.
    await detail.locator(".notes-input").press("Backspace");
    assert.equal(await detail.locator(".notes-input").inputValue(), notes);
    assert.equal(await hub.count(), 1, "the hub stays open");
    await detail.locator(".notes-state", { hasText: /^Saved$/ }).waitFor({ timeout: 5000 });
    assert.equal(await card("fixture-app").locator(".project-status").innerText(), "Paused");
    assert.deepEqual(await card("fixture-app").locator(".tag").allInnerTexts(), ["client"]);

    await page.reload();
    await panel.locator(".row").first().waitFor();
    await openHub();
    await card("fixture-app").waitFor({ timeout: 30_000 });
    assert.equal(await card("fixture-app").locator(".project-status").innerText(), "Paused");
    assert.equal(await card("fixture-app").locator(".project-pin").count(), 1);
    assert.equal(await page.locator(".project-card").first().locator(".project-name").innerText(), "fixture-app", "pinned first");
    await card("fixture-app").click();
    await detail.locator(".notes-input").waitFor();
    assert.equal(await detail.locator(".notes-input").inputValue(), notes);
    assert.equal(await detail.locator(".project-status-select").inputValue(), "paused");
    assert.deepEqual(await detail.locator(".tag-editor .tag").allInnerTexts(), ["client"]);
    await detail.getByRole("button", { name: "Preview" }).click();
    assert.equal(await detail.locator(".notes-preview strong").innerText(), "fixture");
    await shotBoth("4-notes");
    await detail.getByRole("button", { name: "Write" }).click();
  });

  await step("filters narrow the projects", async () => {
    const names = () => page.locator(".project-card .project-name").allInnerTexts();
    const chip = (label) => page.locator(".filter-chip", { hasText: label });
    await chip("Has changes").click();
    assert.deepEqual((await names()).sort(), ["fixture-app", "fixture-merge"]);
    await chip("Has changes").click();
    await chip("Not pushed").click();
    // The merge repository has commits and no remote; the app is a commit ahead.
    assert.deepEqual((await names()).sort(), ["fixture-app", "fixture-merge"]);
    await chip("Not pushed").click();
    await chip("Untouched for 90 days").click();
    await page.locator(".projects-empty", { hasText: "No projects match these filters." }).waitFor();
    await chip("Untouched for 90 days").click();
    await page.locator(".projects-filters select[aria-label='Status']").selectOption("paused");
    assert.deepEqual(await names(), ["fixture-app"]);
    await page.locator(".projects-filters select[aria-label='Status']").selectOption("");
    await page.locator(".projects-filters select[aria-label='Stack']").selectOption("Python");
    assert.deepEqual(await names(), ["fixture-plain"]);
    await page.locator(".projects-filters select[aria-label='Stack']").selectOption("");
    await page.locator(".projects-filters select[aria-label='Tag']").selectOption("client");
    assert.deepEqual(await names(), ["fixture-app"]);
    // Clearing takes the text away too.
    await page.locator(".projects-clear").click();
    assert.equal(await page.locator(".projects-filter").inputValue(), "");
    await page.locator(".projects-filter").fill(sandboxName);
    assert.equal((await names()).length, 3);
    await page.locator(".projects-filter").fill(`${sandboxName} python`);
    assert.deepEqual(await names(), ["fixture-plain"]);
    await page.locator(".projects-filter").fill(sandboxName);
  });

  await step("the table layout shows the same facts, and sorting by name orders it", async () => {
    await page.locator(".segmented button", { hasText: "Table" }).click();
    await page.locator(".project-table").waitFor();
    await page.locator(".projects-sort select").selectOption("name");
    const rows = page.locator(".project-row");
    assert.deepEqual(await rows.locator(".project-name").allInnerTexts(), ["fixture-app", "fixture-merge", "fixture-plain"].map((n) => n));
    const appRow = rows.filter({ has: page.locator(".project-name", { hasText: "fixture-app" }) });
    assert.equal(await appRow.locator(".project-changes").innerText(), "1 staged, 1 modified, 1 untracked");
    assert.equal(await appRow.locator(".project-branch").innerText(), "main");
    await shotBoth("5-table");
    await page.locator(".segmented button", { hasText: "Cards" }).click();
    await page.locator(".projects-sort select").selectOption("activity");
  });

  await step("Not a project hides a folder until it is restored", async () => {
    await card("fixture-plain").click();
    await detail.locator(".dismiss-button").click();
    await card("fixture-plain").waitFor({ state: "detached" });
    await page.locator(".projects-link", { hasText: /^Show dismissed folders \(\d+\)$/ }).click();
    await card("fixture-plain").locator(".project-restore").click();
    await page.locator(".projects-link", { hasText: "Back to projects" }).click();
    await card("fixture-plain").waitFor();
  });

  await step("a repository created while the hub is open appears without a restart", async () => {
    write(join(late, "go.mod"), "module example.com/late\n");
    git(late, "init", "-q");
    await card("fixture-late").waitFor({ timeout: 60_000 });
    assert.deepEqual(await card("fixture-late").locator(".project-stack .chip").allInnerTexts(), ["Go"]);
  });

  await step("a recently changed file opens in its folder, selected, and closes the hub", async () => {
    await card("fixture-app").click();
    await detail.locator(".recent-file", { hasText: exactly("notes.txt") }).click();
    await hub.waitFor({ state: "detached" });
    await panel.locator(".row.selected .label", { hasText: exactly("notes.txt") }).waitFor();
  });

  await step("folder views show git badges, and dim what git ignores", async () => {
    await openFolder(app, "notes.txt");
    const glyph = (name) => item(name).locator(".git-glyph");
    await glyph("src").waitFor({ timeout: 15_000 });
    assert.equal(await glyph("src").getAttribute("data-change"), "modified", "a folder shows the loudest change inside");
    assert.equal(await glyph("notes.txt").getAttribute("data-change"), "untracked");
    assert.equal(await glyph("README.md").count(), 0);
    for (const ignored of ["debug.log", "node_modules", "target"]) {
      await item(ignored).and(page.locator(".dimmed")).waitFor({ timeout: 10_000 });
      assert.equal(await glyph(ignored).count(), 0);
    }
    assert.equal(await item("README.md").and(page.locator(".dimmed")).count(), 0);
    await shotBoth("6-badges-details");

    await openFolder(join(app, "src"), "main.rs");
    await glyph("main.rs").waitFor({ timeout: 15_000 });
    assert.equal(await glyph("main.rs").getAttribute("data-change"), "modified");
    assert.equal(await glyph("staged.rs").getAttribute("data-change"), "added");
    assert.equal(await glyph("lib.rs").count(), 0);

    await openFolder(merge, "notes.md");
    await glyph("notes.md").waitFor({ timeout: 15_000 });
    assert.equal(await glyph("notes.md").getAttribute("data-change"), "conflicted");
  });

  await step("badges work in the icon views and follow changes made by other programs", async () => {
    await openFolder(app, "notes.txt");
    await panel.locator(".list-viewport").focus();
    await page.keyboard.press("Control+Shift+Digit3");
    await panel.locator('.filelist[data-view="medium"]').waitFor();
    const glyph = (name) => item(name).locator(".git-glyph");
    await glyph("notes.txt").waitFor({ timeout: 15_000 });
    assert.equal(await glyph("src").getAttribute("data-change"), "modified");
    await item("node_modules").and(page.locator(".view-item.dimmed")).waitFor();
    assert.equal(await glyph("README.md").count(), 0);
    // Another program edits a clean file: the badge appears once git has been asked again.
    write(join(app, "README.md"), "# Fixture app\n\nEdited elsewhere.\n");
    await glyph("README.md").waitFor({ timeout: 30_000 });
    assert.equal(await glyph("README.md").getAttribute("data-change"), "modified");
    await shotBoth("7-badges-icons");
    await page.keyboard.press("Control+Shift+Digit6");
    await panel.locator('.filelist[data-view="details"]').waitFor();
  });

  await step("badge answers are kept only for the folders shown lately, and only in repositories", async () => {
    const kept = () => page.evaluate(() => window.__kestrelGitBadges.kept());
    const glyph = (name) => item(name).locator(".git-glyph");
    await openFolder(app, "notes.txt");
    await glyph("src").waitFor({ timeout: 15_000 });
    assert(await kept() > 0, "the repository's answers are kept while it is shown");
    // Twenty folders outside any repository, one after another, as a long session would.
    for (let n = 0; n < OUTSIDE_FOLDERS; n++) await openFolder(join(outside, `folder-${n}`), `file-${n}-0.txt`);
    await page.waitForTimeout(500);
    assert.equal(await kept(), 0, "nothing kept for folders outside a repository, or for the repository left long ago");
    await openFolder(app, "notes.txt");
    await glyph("src").waitFor({ timeout: 15_000 });
    assert.equal(await glyph("notes.txt").getAttribute("data-change"), "untracked", "badges come back when it is shown again");
  });

  await step("the hub closes when another place is opened", async () => {
    await openHub();
    // The first place: your user folder.
    await page.locator(".sidebar .place").first().click();
    await hub.waitFor({ state: "detached" });
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  if (!cleanedUp) {
    cleanedUp = true;
    await resetProjectData().catch(() => {});
    await invoke("projects_probe", { on: false, allow: [] }).catch(() => {});
    await page
      .evaluate((saved) => {
        for (const [key, value] of saved) {
          if (value === null) localStorage.removeItem(key);
          else localStorage.setItem(key, value);
        }
      }, savedStorage)
      .catch(() => {});
    await page.emulateMedia({ colorScheme: null }).catch(() => {});
    await page.reload().catch(() => {});
    await browser.close();
    rmSync(sandbox, { recursive: true, force: true });
  }
}
