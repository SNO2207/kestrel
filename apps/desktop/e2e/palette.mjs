// End-to-end test of the command palette, the quick launcher and running in the background.
//
// Files: everything happens in a scratch folder this script creates under the temp directory
// and removes afterwards. One file in it is a tiny script that, when opened "with its default
// app" as the palette opens files, writes a marker file beside itself and exits; a console
// window flashes while it runs. Nothing else is opened.
//
// The quick launcher is shown through a development-only command, never the real shortcut,
// which test instances leave unregistered (KESTREL_GLOBAL_HOTKEY=off, set by dev.mjs). One
// step registers an unusual shortcut unique to this slot (Ctrl+Alt+Shift+F13 and up), checks
// that the settings dialog reports it registered, then unregisters it; another has a separate
// PowerShell process hold that shortcut, to see the clash reported. "Keep running in the tray"
// is switched on for two steps and off again; "Start with Windows" is never switched on. One
// step starts the slot's executable a second time, which must show this instance and exit.
//
// The window's view settings (local storage) are saved first and put back at the end.
//
// Start the app with `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then run
// `KESTREL_SLOT=<n> node e2e/palette.mjs`. Screenshots go to e2e/out/palette, dark and light.

import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";
import { appExe, dataDir, devUrl, slot } from "./slot.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out", "palette");
mkdirSync(out, { recursive: true });

// ---------- the scratch folder ----------

/** A word found nowhere else on the drive, so every search below has exactly the hits planned. */
const tag = `kpal${Date.now().toString(36)}`;
const sandbox = join(tmpdir(), `kestrel-e2e-palette-${process.pid}`);
const PROJECTS = `${tag} Projects`;
const OTHER = `Other ${tag}`;
const INNER = `inner-${tag}.md`;
const NOTES = `${tag}-alpha notes.txt`;
const OPENER = `${tag}-opener.cmd`;
const RECENT_ONE = `${tag} recent one`;
const RECENT_TWO = `${tag} recent two`;
const marker = join(sandbox, "opened.txt");

rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, PROJECTS, "Deep"), { recursive: true });
for (const folder of [OTHER, RECENT_ONE, RECENT_TWO]) mkdirSync(join(sandbox, folder));
writeFileSync(join(sandbox, NOTES), "alpha");
writeFileSync(join(sandbox, PROJECTS, INNER), "# inner\n");
writeFileSync(join(sandbox, PROJECTS, "Deep", "bottom.txt"), "deep");
// Opened with its default app (Windows runs it), it leaves a marker beside itself and exits.
writeFileSync(join(sandbox, OPENER), '@echo opened> "%~dp0opened.txt"\r\n');

/** The slot's own unusual shortcut: Ctrl+Alt+Shift with F13 to F24, which keyboards rarely have. */
const SLOT_SHORTCUT = `Ctrl+Alt${slot < 12 ? "+Shift" : ""}+F${13 + (slot % 12)}`;
const SLOT_VK = 0x7c + (slot % 12); // VK_F13 and up
const SLOT_MODS = 0x4000 | 0x1 | 0x2 | (slot < 12 ? 0x4 : 0); // MOD_NOREPEAT | ALT | CONTROL | SHIFT

// ---------- the windows ----------

const { browser, page } = await connectToKestrel();
const STORAGE_KEYS = ["kestrel.views", "kestrel.view-options", "search-scope", "preview-open"];
const savedStorage = await page.evaluate((keys) => keys.map((key) => [key, localStorage.getItem(key)]), STORAGE_KEYS);
await page.reload();

/** The quick launcher's page, held to the same standard as the main window: this slot's, by title. */
async function quickPage() {
  for (const candidate of browser.contexts().flatMap((context) => context.pages())) {
    if (candidate.url().startsWith(`${devUrl}/quick.html`) && (await candidate.title()) === "Kestrel quick launcher") return candidate;
  }
  throw new Error(`No quick launcher page at ${devUrl}/quick.html`);
}
const quick = await quickPage();
await quick.reload();

const invoke = (target, command, args = {}) =>
  target.evaluate(([command, args]) => window.__TAURI_INTERNALS__.invoke(command, args), [command, args]);
const windows = () => invoke(page, "launcher_dev_windows");
const until = async (check, what, timeout = 5000) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await check()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting until ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
};

const sessions = new Map();
/** Captured through the debugging protocol, which does not resize the window. */
const shot = async (target, name) => {
  if (!sessions.has(target)) sessions.set(target, await target.context().newCDPSession(target));
  const { data } = await sessions.get(target).send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `${name}.png`), Buffer.from(data, "base64"));
};
const shotBoth = async (target, name) => {
  for (const scheme of ["dark", "light"]) {
    await target.emulateMedia({ colorScheme: scheme });
    await target.waitForTimeout(150);
    await shot(target, `${name}-${scheme}`);
  }
  await target.emulateMedia({ colorScheme: null });
};

const step = (await import("./steps.mjs")).stepper("palette", () => page, { timed: true });

// ---------- the palette, in either window ----------

function paletteIn(target) {
  const root = target.locator(".palette");
  const input = root.locator("input");
  const items = root.locator(".palette-item");
  const settled = () => root.locator('.palette-list[aria-busy="false"]').waitFor();
  return {
    root,
    input,
    items,
    settled,
    /** The titles shown once the answer for what was typed is in. */
    titles: async () => (await settled(), items.locator(".palette-title").allTextContents()),
    providers: async () => (await settled(), items.evaluateAll((els) => els.map((el) => el.dataset.provider))),
    selectedTitle: async () => root.locator(".palette-item.active .palette-title").textContent(),
    /** Types `text` afresh, keystroke by keystroke, and waits for its answer. */
    type: async (text) => {
      await input.fill("");
      await input.focus();
      await target.keyboard.type(text, { delay: 8 });
      await settled();
    },
    /**
     * Searches until `title` is among the hits: a new file reaches the index within a moment,
     * through the drive's change journal, and the palette asks again with each keystroke.
     */
    find: async (text, title, timeout = 10_000) => {
      const deadline = Date.now() + timeout;
      for (;;) {
        await input.fill("");
        await input.fill(text);
        await settled();
        const found = await items.locator(".palette-title").allTextContents();
        if (found.includes(title)) return found;
        if (Date.now() > deadline) throw new Error(`"${title}" not found for "${text}"; found ${JSON.stringify(found)}`);
        await target.waitForTimeout(250);
      }
    },
  };
}

const main = paletteIn(page);
const panel = page.locator(".tab-panel.active");
const tabCount = () => page.locator(".tab").count();
const activeTabTitle = () => page.locator(".tab.active .tab-title").textContent();
const crumbs = () => page.locator(".crumb").allTextContents();
const selectedLabels = () => panel.locator(".row.selected .label").allTextContents();
const listHasFocus = () => page.evaluate(() => document.activeElement?.classList.contains("list-viewport") ?? false);

const openPalette = async () => {
  if ((await main.root.count()) === 0) await page.keyboard.press("Control+k");
  await main.input.waitFor();
};
const closePalette = async () => {
  if ((await main.root.count()) > 0) await page.keyboard.press("Escape");
  await main.root.waitFor({ state: "detached" });
};
const goTo = async (path) => {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
  await page.waitForFunction((path) => {
    const tab = document.querySelector(".tab.active");
    return tab?.getAttribute("title")?.toLowerCase() === path.toLowerCase();
  }, path);
};
const activePath = () => page.locator(".tab.active").getAttribute("title");

let holder = null;

try {
  await step("the window is ready", async () => {
    await panel.locator(".row").first().waitFor();
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    await goTo(sandbox);
    await panel.locator(".row .label", { hasText: PROJECTS }).waitFor();
    assert.equal(await tabCount(), 1, "the suite starts with one tab");
  });

  await step("Ctrl+K opens the palette over the window, listing actions with their shortcuts", async () => {
    await page.keyboard.press("Control+k");
    await main.input.waitFor();
    assert(await main.input.evaluate((input) => input === document.activeElement), "the input has the keyboard");
    const titles = await main.titles();
    assert(titles.includes("New tab") && titles.includes("Large icons") && titles.includes("Show hidden items"), titles.join(", "));
    const newTab = main.items.filter({ has: page.locator(".palette-title", { hasText: /^New tab$/ }) });
    assert.equal(await newTab.locator(".palette-hint").textContent(), "Ctrl+T");
    await shotBoth(page, "1-actions");
  });

  await step("Escape closes it and the files have the keyboard again", async () => {
    await page.keyboard.press("Escape");
    await main.root.waitFor({ state: "detached" });
    assert(await listHasFocus());
    await page.keyboard.press("Control+k");
    await main.input.waitFor();
    await page.keyboard.press("Control+k");
    await main.root.waitFor({ state: "detached" });
  });

  await step("> narrows to actions, matched fuzzily with the matched letters marked", async () => {
    await openPalette();
    await main.type(">nwtb");
    assert.equal((await main.titles())[0], "New tab");
    assert((await main.providers()).every((provider) => provider === "actions"));
    const marked = await main.items.first().locator("mark").allTextContents();
    assert.equal(marked.join(""), "Nwtb", `marked: ${JSON.stringify(marked)}`);
    await shotBoth(page, "2-fuzzy-action");
  });

  await step("Enter runs an action: a new tab, then closing it", async () => {
    await page.keyboard.press("Enter");
    await main.root.waitFor({ state: "detached" });
    await until(async () => (await tabCount()) === 2, "a second tab opens");
    await openPalette();
    await main.type(">close tab");
    assert.equal(await main.selectedTitle(), "Close tab");
    await page.keyboard.press("Enter");
    await until(async () => (await tabCount()) === 1, "the tab closes");
  });

  await step("view actions change the view, and the arrow keys pick among results", async () => {
    await openPalette();
    await main.type(">icons");
    const titles = await main.titles();
    assert(titles.slice(0, 4).every((title) => /icons$/.test(title)), titles.join(", "));
    // Down moves the selection, up from the first wraps to the last.
    await page.keyboard.press("ArrowDown");
    assert.equal(await main.selectedTitle(), titles[1]);
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("ArrowUp");
    assert.equal(await main.selectedTitle(), titles[titles.length - 1]);
    await main.type(">large icons");
    await page.keyboard.press("Enter");
    await panel.locator('.filelist[data-view="large"]').waitFor();
    await openPalette();
    await main.type(">details");
    await page.keyboard.press("Enter");
    await panel.locator('.filelist[data-view="details"]').waitFor();
  });

  await step("actions on the selection act on the list: select all, then rename", async () => {
    await page.keyboard.press("Escape").catch(() => {});
    await panel.locator(".row .label", { hasText: NOTES }).click();
    await openPalette();
    await main.type(">select all");
    await page.keyboard.press("Enter");
    await until(async () => /\d+ selected/.test(await page.locator(".statusbar").innerText()), "everything is selected");
    assert.match(await page.locator(".statusbar").innerText(), /6 selected/);
    await panel.locator(".row .label", { hasText: NOTES }).click();
    await openPalette();
    await main.type(">rename");
    assert.equal(await main.selectedTitle(), "Rename");
    await page.keyboard.press("Enter");
    const input = panel.locator("input.rename");
    await input.waitFor();
    assert.equal(await input.inputValue(), NOTES);
    await page.keyboard.press("Escape");
    await input.waitFor({ state: "detached" });
  });

  await step("files come from the index, ranked by a fuzzy match on the name", async () => {
    await openPalette();
    const found = await main.find(`${tag}alphnot`, NOTES);
    assert.equal(found[0], NOTES, `the fuzzy match comes first: ${found.join(", ")}`);
    const first = main.items.first();
    assert.equal(await first.getAttribute("data-provider"), "files");
    assert.equal(await first.locator(".palette-subtitle").textContent(), sandbox);
    assert.equal(await first.locator(".palette-kind").textContent(), "File");
    assert((await first.locator("mark").count()) > 0, "matched letters are marked");
    await shotBoth(page, "3-files");
  });

  await step("Enter opens a folder in the current tab", async () => {
    await main.find(`${tag} projects`, PROJECTS);
    assert.equal(await main.selectedTitle(), PROJECTS);
    assert.equal(await main.items.first().locator(".palette-kind").textContent(), "Folder");
    await page.keyboard.press("Enter");
    await main.root.waitFor({ state: "detached" });
    await panel.locator(".row .label", { hasText: INNER }).waitFor();
    assert.equal((await activePath()).toLowerCase(), join(sandbox, PROJECTS).toLowerCase());
    assert.equal(await tabCount(), 1);
    assert(await listHasFocus(), "the keyboard is back on the files");
  });

  await step("Ctrl+Enter opens a folder in a new tab", async () => {
    await openPalette();
    await main.find(`other ${tag}`, OTHER);
    await page.keyboard.press("Control+Enter");
    await until(async () => (await tabCount()) === 2, "a new tab opens");
    await until(async () => (await activeTabTitle()) === OTHER, "it shows the folder");
    await page.keyboard.press("Control+w");
    await until(async () => (await tabCount()) === 1, "the tab closes");
  });

  await step("Shift+Enter shows a file in its folder, selected", async () => {
    await goTo(sandbox);
    await openPalette();
    await main.find(`inner ${tag}`, INNER);
    await page.keyboard.press("Shift+Enter");
    await until(async () => (await activePath())?.toLowerCase() === join(sandbox, PROJECTS).toLowerCase(), "its folder opens");
    await until(async () => JSON.stringify(await selectedLabels()) === JSON.stringify([INNER]), "the file is selected");
  });

  await step("/ lists a typed folder's contents, and Tab completes a path", async () => {
    await openPalette();
    await main.type(`/${sandbox}\\`);
    let titles = await main.titles();
    assert.equal(titles[0], sandbox, "the folder itself comes first");
    assert.equal(await main.items.first().locator(".palette-subtitle").textContent(), "This folder");
    for (const name of [PROJECTS, OTHER, NOTES]) assert(titles.includes(name), `${name} in ${titles.join(", ")}`);
    await shotBoth(page, "4-path");
    await main.type(`/${sandbox}\\${tag} proj`);
    assert.equal((await main.titles())[0], PROJECTS);
    await page.keyboard.press("Tab");
    assert.equal(await main.input.inputValue(), `/${sandbox}\\${PROJECTS}\\`);
    await main.settled();
    titles = await main.titles();
    assert.deepEqual(titles, [join(sandbox, PROJECTS), "Deep", INNER], "folders first, then files");
    await page.keyboard.press("ArrowDown");
    assert.equal(await main.selectedTitle(), "Deep");
    await page.keyboard.press("Enter");
    await until(async () => (await activePath())?.toLowerCase() === join(sandbox, PROJECTS, "Deep").toLowerCase(), "Deep opens");
  });

  await step("@ and # narrow to projects and tags, or say there are none yet", async () => {
    for (const [prefix, empty] of [["@", "No projects yet."], ["#", "No tags yet."]]) {
      await openPalette();
      await main.type(prefix);
      const providers = await main.providers();
      if (providers.length === 0) assert.equal(await main.root.locator(".palette-empty").textContent(), empty);
      else assert(providers.every((provider) => provider === (prefix === "@" ? "projects" : "tags")), providers.join(", "));
    }
    await closePalette();
  });

  await step("what was chosen before ranks higher", async () => {
    await openPalette();
    await main.find(`${tag} recent`, RECENT_TWO);
    assert.deepEqual((await main.titles()).slice(0, 2), [RECENT_ONE, RECENT_TWO], "equal matches, by name");
    await page.keyboard.press("ArrowDown");
    assert.equal(await main.selectedTitle(), RECENT_TWO);
    await page.keyboard.press("Enter");
    await until(async () => (await activeTabTitle()) === RECENT_TWO, "the folder opens");
    await openPalette();
    await main.type(`${tag} recent`);
    assert.deepEqual((await main.titles()).slice(0, 2), [RECENT_TWO, RECENT_ONE], "the one used comes first now");
    // An empty palette lists it among what was used lately, ahead of actions never used.
    await main.type("");
    const titles = await main.titles();
    assert(titles.indexOf(RECENT_TWO) >= 0 && titles.indexOf(RECENT_TWO) < titles.indexOf("Next tab"), titles.join(", "));
    await shotBoth(page, "5-recent");
    await closePalette();
  });

  await step("Enter opens a file with its default app", async () => {
    await openPalette();
    await main.find(`${tag} opener`, OPENER);
    await page.keyboard.press("Enter");
    await until(() => existsSync(marker), "the file's app ran", 10_000);
    rmSync(marker, { force: true });
  });

  await step("typing then Enter at once opens the first hit for what was typed", async () => {
    await goTo(sandbox);
    await openPalette();
    await main.input.fill("");
    await main.input.focus();
    // No wait for the answer between the last key and Enter.
    await page.keyboard.type(`${tag} other`, { delay: 0 });
    await page.keyboard.press("Enter");
    await until(async () => (await activeTabTitle()) === OTHER, "the folder typed opens");
  });

  await step("a query that reaches the backend after the next one neither cancels it nor is shown", async () => {
    // The backend runs queries on a pool of threads, so under load the query for one key can
    // arrive after the next key's (places.mjs once waited in vain for "this pc"). The hook
    // holds the query for the shorter text back until the longer one's has started, and that
    // one then waits until the held one has been through, as the pool once had them.
    await closePalette();
    await openPalette();
    await main.settled();
    const shorter = `${tag} othe`;
    assert.equal(await invoke(page, "palette_dev_late", { hold: shorter }), "armed");
    await main.input.fill(shorter);
    await until(async () => (await invoke(page, "palette_dev_late")) === "holding", "the query for the shorter text is held back");
    await page.keyboard.type("r");
    await main.settled();
    assert.equal(await invoke(page, "palette_dev_late"), "off", "the held query has been through");
    // The answer for what was typed, with its matched letters marked: not the list shown before.
    assert.equal(await main.input.inputValue(), `${tag} other`);
    assert.equal((await main.titles())[0], OTHER);
    assert((await main.items.first().locator("mark").count()) > 0, "the first hit is the answer's, its letters marked");
    await closePalette();
  });

  // ---------- the quick launcher ----------

  const launcher = paletteIn(quick);
  const summon = async () => {
    await invoke(page, "launcher_dev_summon");
    await until(async () => (await windows()).quick_visible, "the quick launcher shows");
    await launcher.input.waitFor();
    await until(() => launcher.input.evaluate((input) => input === document.activeElement && input.value === ""), "it starts afresh, focused");
  };
  const hidden = () => until(async () => !(await windows()).quick_visible, "the quick launcher hides");

  await step("the quick launcher shows through the development hook, and Escape hides it", async () => {
    await summon();
    // On a monitor, clear of the taskbar, centred across it.
    const { quick_bounds: [x, y, width, height], work_areas: areas } = await windows();
    const area = areas.find(([left, top, w, h]) => x >= left && y >= top && x + width <= left + w && y + height <= top + h);
    assert(area, `the launcher at ${[x, y, width, height]} lies within a work area of ${JSON.stringify(areas)}`);
    assert(Math.abs(x + width / 2 - (area[0] + area[2] / 2)) <= 8, "centred across the monitor");
    const titles = await launcher.titles();
    assert(titles.includes("Show Kestrel"), titles.join(", "));
    assert(!titles.includes("Close tab") && !titles.includes("Delete"), "actions on the window's files are left out");
    await shotBoth(quick, "5-quick-launcher");
    await quick.keyboard.press("Escape");
    await hidden();
  });

  await step("the quick launcher follows options changed in the main window", async () => {
    await openPalette();
    await main.type(">show hidden items");
    await page.keyboard.press("Enter");
    await main.root.waitFor({ state: "detached" });
    try {
      await summon();
      await launcher.type(">hidden items");
      assert.equal((await launcher.titles())[0], "Hide hidden items");
      await quick.keyboard.press("Escape");
      await hidden();
    } finally {
      await openPalette();
      await main.type(">hide hidden items");
      await page.keyboard.press("Enter");
      await main.root.waitFor({ state: "detached" });
    }
  });

  await step("the quick launcher opens a folder in Kestrel's window and hides", async () => {
    await goTo(sandbox);
    await summon();
    await launcher.find(`${tag} projects`, PROJECTS);
    await shotBoth(quick, "6-quick-folder");
    await quick.keyboard.press("Enter");
    await hidden();
    await until(async () => (await activePath())?.toLowerCase() === join(sandbox, PROJECTS).toLowerCase(), "the main window shows the folder");
    assert((await windows()).main_visible);
  });

  await step("the quick launcher runs actions in Kestrel's window", async () => {
    await summon();
    await launcher.type(">new tab");
    await quick.keyboard.press("Enter");
    await hidden();
    await until(async () => (await tabCount()) === 2, "the main window has a new tab");
    await page.keyboard.press("Control+w");
    await until(async () => (await tabCount()) === 1, "the tab closes");
  });

  await step("the quick launcher opens a file itself", async () => {
    await summon();
    await launcher.find(`${tag} opener`, OPENER);
    await quick.keyboard.press("Enter");
    await hidden();
    await until(() => existsSync(marker), "the file's app ran", 10_000);
  });

  // ---------- the shortcut and the settings dialog ----------

  const openSettings = async () => {
    await openPalette();
    await main.type(">quick launcher settings");
    await page.keyboard.press("Enter");
    await page.locator(".launcher-dialog").waitFor();
    await page.locator(".launcher-status").first().waitFor();
  };
  const closeSettings = async () => {
    await page.locator(".launcher-dialog .dialog-footer .dialog-button").click();
    await page.locator(".launcher-dialog").waitFor({ state: "detached" });
  };
  const status = () => page.locator("#launcher-shortcut-status");

  await step("with KESTREL_GLOBAL_HOTKEY=off the dialog says the shortcut is off here", async () => {
    const settings = await invoke(page, "launcher_settings");
    assert.equal(settings.hotkey.state, "disabled", JSON.stringify(settings.hotkey));
    assert.equal(settings.start_with_windows, false, "never switched on by tests");
    await openSettings();
    assert.equal(await status().getAttribute("data-state"), "disabled");
    assert.match(await status().textContent(), /KESTREL_GLOBAL_HOTKEY=off/);
    assert.equal(await page.locator(".launcher-check input").nth(1).isChecked(), false);
    await closeSettings();
  });

  await step("the dialog records a new shortcut from the keys pressed, refuses one without Ctrl, Alt or Win, and stores it", async () => {
    const before = (await invoke(page, "launcher_settings")).hotkey.shortcut;
    await openSettings();
    const field = page.locator(".shortcut-field");
    const use = page.locator(".launcher-row .dialog-button.primary");
    await field.click();
    await page.locator(".shortcut-prompt").waitFor();
    await page.keyboard.press("k");
    assert.equal(await field.textContent(), "K");
    await use.click();
    await page.locator('.launcher-status.problem[role="alert"]', { hasText: "needs Ctrl, Alt or Win" }).waitFor();
    await field.click();
    await page.keyboard.press("Control+Alt+Shift+KeyJ");
    assert.equal(await field.textContent(), "Ctrl+Alt+Shift+J");
    await use.click();
    await until(async () => (await invoke(page, "launcher_settings")).hotkey.shortcut === "Ctrl+Alt+Shift+J", "the shortcut is stored");
    assert.equal(await page.locator('.launcher-status.problem[role="alert"]').count(), 0, "the earlier refusal is gone");
    // This instance registers nothing, whatever is chosen.
    assert.equal(await status().getAttribute("data-state"), "disabled");
    await page.locator(".link-button", { hasText: "Use Ctrl+Alt+Space" }).click();
    await until(async () => (await invoke(page, "launcher_settings")).hotkey.shortcut === "Ctrl+Alt+Space", "the default is back");
    assert.equal(await field.textContent(), "Ctrl+Alt+Space");
    await closeSettings();
    if (before !== "Ctrl+Alt+Space") await invoke(page, "set_launcher_hotkey", { shortcut: before });
  });

  await step(`a shortcut unique to this slot (${SLOT_SHORTCUT}) registers and is reported, then unregisters`, async () => {
    const registered = await invoke(page, "launcher_dev_hotkey", { shortcut: SLOT_SHORTCUT });
    assert.equal(registered.state, "registered", JSON.stringify(registered));
    assert.equal(registered.registered, SLOT_SHORTCUT);
    await openSettings();
    assert.equal(await status().getAttribute("data-state"), "registered");
    assert.match(await status().textContent(), new RegExp(`Press ${SLOT_SHORTCUT.replace(/\+/g, "\\+")} anywhere in Windows`));
    await shotBoth(page, "7-settings-registered");
    await closeSettings();
    const released = await invoke(page, "launcher_dev_hotkey", { shortcut: null });
    assert.equal(released.registered, null);
    assert.equal(released.state, "disabled");
  });

  await step("a shortcut another program holds is reported as taken, not fatal", async () => {
    // A separate process registers the slot's shortcut first.
    const script = [
      "Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class Hold { [DllImport(\"user32.dll\")] public static extern bool RegisterHotKey(IntPtr w, int id, uint mods, uint vk); }'",
      `$held = [Hold]::RegisterHotKey([IntPtr]::Zero, 1, ${SLOT_MODS}, ${SLOT_VK})`,
      "Write-Output \"held $held\"",
      "Start-Sleep -Seconds 120",
    ].join("; ");
    holder = spawn("powershell", ["-NoProfile", "-NonInteractive", "-Command", script], { stdio: ["ignore", "pipe", "inherit"] });
    let said = "";
    holder.stdout.on("data", (chunk) => (said += chunk));
    await until(() => said.includes("held"), "the other process holds the keys", 60_000);
    assert.match(said, /held True/, "the other process got the shortcut");
    const clash = await invoke(page, "launcher_dev_hotkey", { shortcut: SLOT_SHORTCUT });
    assert.equal(clash.state, "failed", JSON.stringify(clash));
    assert.match(clash.message, /in use by another app/);
    await openSettings();
    assert.equal(await status().getAttribute("data-state"), "failed");
    await shotBoth(page, "8-settings-taken");
    await closeSettings();
    holder.kill();
    holder = null;
    const cleared = await invoke(page, "launcher_dev_hotkey", { shortcut: null });
    assert.equal(cleared.state, "disabled");
  });

  // A script cannot click in the notification area; a development hook does what each menu
  // item's click does instead.
  const trayItem = (item) => invoke(page, "launcher_dev_tray", { item });

  await step("the tray icon's menu shows the quick launcher and opens the settings", async () => {
    assert.equal((await windows()).tray, true, "the tray icon is there");
    await trayItem("tray-quick");
    await until(async () => (await windows()).quick_visible, "the quick launcher shows");
    await quick.keyboard.press("Escape");
    await hidden();
    await trayItem("tray-settings");
    await page.locator(".launcher-dialog").waitFor();
    await closeSettings();
  });

  await step("the tray option and its menu tick follow each other", async () => {
    let state = await windows();
    assert.equal(state.tray_keep_ticked, false, "off by default");
    await trayItem("tray-keep");
    await until(async () => (await invoke(page, "launcher_settings")).keep_in_tray, "the menu turns the option on");
    assert.equal((await windows()).tray_keep_ticked, true);
    await openSettings();
    const keep = page.locator(".launcher-check input").first();
    assert.equal(await keep.isChecked(), true, "the dialog shows it on");
    await keep.uncheck();
    await until(async () => (await windows()).tray_keep_ticked === false, "the dialog turns the menu's tick off");
    assert.equal((await invoke(page, "launcher_settings")).keep_in_tray, false);
    await closeSettings();
  });

  await step("with the tray option on, closing the window hides it, and the tray brings it back", async () => {
    await openSettings();
    const keep = page.locator(".launcher-check input").first();
    await keep.check();
    await until(async () => (await invoke(page, "launcher_settings")).keep_in_tray, "the option is saved");
    await closeSettings();
    // Only now is it safe to press the window's close button: it hides rather than quits.
    await page.locator(".window-controls .close").click();
    await until(async () => !(await windows()).main_visible, "the window hides");
    assert.equal(await tabCount(), 1, "the app is still running, with its tab");
    await trayItem("tray-show");
    await until(async () => (await windows()).main_visible, "the window shows again");
    await invoke(page, "set_keep_in_tray", { keep: false });
    assert.equal((await invoke(page, "launcher_settings")).keep_in_tray, false);
  });

  await step("launching Kestrel again shows the one in the tray instead of starting a second", async () => {
    await invoke(page, "set_keep_in_tray", { keep: true });
    await page.locator(".window-controls .close").click();
    await until(async () => !(await windows()).main_visible, "the window hides");
    // What clicking its Start menu entry does: the same executable, on the same data folder.
    const started = Date.now();
    const second = spawnSync(appExe, [], {
      env: { ...process.env, ...(dataDir ? { KESTREL_DATA_DIR: dataDir } : {}), KESTREL_GLOBAL_HOTKEY: "off" },
      timeout: 30_000,
      windowsHide: true,
    });
    assert.equal(second.status, 0, `the second process stands down at once (${second.error ?? second.status})`);
    assert(Date.now() - started < 10_000, `it took ${Date.now() - started} ms`);
    await until(async () => (await windows()).main_visible, "the running window shows");
    assert.equal(await tabCount(), 1, "it is the same app, with its tab");
    await invoke(page, "set_keep_in_tray", { keep: false });
  });

  console.log("\nall checks passed; screenshots are in", out);
} finally {
  holder?.kill();
  await invoke(page, "palette_dev_late", { release: true }).catch(() => {});
  await invoke(page, "launcher_dev_hotkey", { shortcut: null }).catch(() => {});
  await invoke(page, "set_keep_in_tray", { keep: false }).catch(() => {});
  await invoke(page, "show_main_window").catch(() => {});
  await invoke(page, "hide_quick_window").catch(() => {});
  for (const target of [page, quick]) await target.emulateMedia({ colorScheme: null }).catch(() => {});
  for (const session of sessions.values()) await session.detach().catch(() => {});
  await page
    .evaluate((saved) => {
      for (const [key, value] of saved) {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, value);
      }
    }, savedStorage)
    .catch(() => {});
  await page.reload().catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
