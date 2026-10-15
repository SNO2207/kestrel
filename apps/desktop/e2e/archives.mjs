// End-to-end test of archives against the real app window: compressing from the menu, browsing
// inside a zip and a folder in it, the preview and Quick Look of a file inside, Extract all
// through its dialog, undoing that extraction, and a zip-slip archive refused.
//
// Everything happens inside a scratch folder this script creates under the temp directory and
// removes afterwards. The clipboard is never touched: what Copy would carry out of an archive is
// asked of a development hook instead. Undoing the extraction sends the extracted folder (named
// kestrel-test-*) to the Recycle Bin, and the redo that follows brings it back, so the bin ends
// as it began. Needs a dev build (the hook): start the app with `node e2e/dev.mjs start`, then
// run: node e2e/archives.mjs. Screenshots go to e2e/out/archives-*.png, dark and light.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel } from "./connect.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

// ---------- the scratch folder ----------

const sandbox = join(tmpdir(), `kestrel-test-e2e-archives-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
const project = join(sandbox, "Project");
mkdirSync(join(project, "docs"), { recursive: true });
mkdirSync(join(project, "empty"));
writeFileSync(join(project, "notes.txt"), "Hello from inside the archive.\nSecond line.\n");
writeFileSync(join(project, "docs", "guide.md"), "# Guide\n\nRead me.\n");
writeFileSync(join(project, "café.txt"), "unicode name");
const extractedName = `kestrel-test-${process.pid}-extracted`;
const extracted = join(sandbox, extractedName);

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (bytes) => {
  let c = 0xffffffff;
  for (const byte of bytes) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

/** Traditional PKWARE (ZipCrypto) encryption of `data`, with its 12-byte header first. */
function zipCrypto(password, data, crc) {
  let keys = [0x12345678, 0x23456789, 0x34567890];
  const step = (crc32Value, byte) => (CRC_TABLE[(crc32Value ^ byte) & 0xff] ^ (crc32Value >>> 8)) >>> 0;
  const update = (byte) => {
    keys[0] = step(keys[0], byte);
    keys[1] = (Math.imul((keys[1] + (keys[0] & 0xff)) >>> 0, 134775813) + 1) >>> 0;
    keys[2] = step(keys[2], keys[1] >>> 24);
  };
  const stream = () => {
    const temp = (keys[2] | 2) & 0xffff;
    return ((temp * (temp ^ 1)) >>> 8) & 0xff;
  };
  for (const byte of Buffer.from(password, "utf8")) update(byte);
  const plain = Buffer.concat([Buffer.from([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, crc >>> 24]), data]);
  const out = Buffer.alloc(plain.length);
  plain.forEach((byte, at) => {
    out[at] = byte ^ stream();
    update(byte);
  });
  return out;
}

/**
 * A zip of stored entries with exactly these names, as a malicious archive has them; with a
 * password, encrypted with ZipCrypto.
 */
function rawZip(entries, password = null) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const [name, text] of entries) {
    const nameBytes = Buffer.from(name, "utf8");
    const plain = Buffer.from(text, "utf8");
    const crc = crc32(plain);
    const data = password === null ? plain : zipCrypto(password, plain, crc);
    const flags = password === null ? 0 : 1;
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(flags, 6);
    local.writeUInt16LE(0, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(data.length, 18);
    local.writeUInt32LE(plain.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(flags, 8);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(plain.length, 24);
    central.writeUInt16LE(nameBytes.length, 28);
    central.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, data);
    centrals.push(central, nameBytes);
    offset += local.length + nameBytes.length + data.length;
  }
  const directory = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(directory.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}
/** A RAR 5 archive of stored files and folders (a folder is a name ending in "/"). */
function rawRar(items) {
  const vint = (n) => {
    const out = [];
    do {
      let byte = n & 0x7f;
      n = Math.floor(n / 128);
      if (n > 0) byte |= 0x80;
      out.push(byte);
    } while (n > 0);
    return Buffer.from(out);
  };
  const u32 = (n) => {
    const bytes = Buffer.alloc(4);
    bytes.writeUInt32LE(n >>> 0);
    return bytes;
  };
  const block = (fields, data = Buffer.alloc(0)) => {
    const body = Buffer.concat(fields);
    const head = Buffer.concat([vint(body.length), body]);
    return Buffer.concat([u32(crc32(head)), head, data]);
  };
  const parts = [Buffer.from([0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00]), block([vint(1), vint(0), vint(0)])];
  for (const [name, text] of items) {
    const dir = name.endsWith("/");
    const data = Buffer.from(text ?? "", "utf8");
    const fields = [vint(2), vint(dir ? 0 : 2)];
    if (!dir) fields.push(vint(data.length));
    fields.push(vint(dir ? 0x3 : 0x6), vint(data.length), vint(dir ? 0x10 : 0x20), u32(1_700_000_000));
    if (!dir) fields.push(u32(crc32(data)));
    const nameBytes = Buffer.from(name.replace(/\/$/, ""), "utf8");
    fields.push(vint(0), vint(0), vint(nameBytes.length), nameBytes);
    parts.push(block(fields, dir ? undefined : data));
  }
  parts.push(block([vint(5), vint(0), vint(0)]));
  return Buffer.concat(parts);
}

writeFileSync(join(sandbox, "slip.zip"), rawZip([["good.txt", "good"], ["../evil.txt", "evil"]]));
writeFileSync(join(sandbox, "notes.rar"), rawRar([["letters/", null], ["letters/hello.txt", "Hello from a RAR archive.\n"], ["readme.txt", "top"]]));
writeFileSync(join(sandbox, "secret.zip"), rawZip([["secret.txt", "the secret"]], "correct horse"));
// Enough data that compressing it at the highest 7z level takes a few seconds.
mkdirSync(join(sandbox, "Big"));
writeFileSync(join(sandbox, "Big", "noise.bin"), randomBytes(24 << 20));

/** Every file and folder under `dir`, with each file's text, for comparing trees. */
function treeOf(dir, prefix = "") {
  const tree = {};
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) {
      tree[`${prefix}${name}/`] = null;
      Object.assign(tree, treeOf(path, `${prefix}${name}/`));
    } else {
      tree[`${prefix}${name}`] = readFileSync(path, "utf8");
    }
  }
  return tree;
}

/** What Windows' own tar.exe (libarchive) lists in an archive. */
function listed(archive) {
  return execFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe"), ["-tf", archive], { encoding: "utf8" })
    .split(/\r?\n/)
    .filter(Boolean);
}

/** Items in the Recycle Bin that were deleted from somewhere inside the scratch folder. */
function recycledFromSandbox() {
  const script =
    "$pattern = $args[0]; $bin = (New-Object -ComObject Shell.Application).NameSpace(10); " +
    "@($bin.Items() | Where-Object { [string]$_.ExtendedProperty('System.Recycle.DeletedFrom') -like $pattern }).Count";
  const output = execFileSync("powershell", ["-NoProfile", "-Command", `& { ${script} } '${sandbox.replace(/'/g, "''")}*'`], {
    encoding: "utf8",
  });
  return Number(output.trim());
}

// ---------- the app ----------

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const menu = page.locator(".menu");
const pane = page.locator(".preview-pane");
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: new RegExp(`^${escape(name)}$`) }) });
const selectedNames = () => panel.locator(".row.selected .label").allTextContents();
const crumbs = () => page.locator(".crumb").allTextContents();
const expectNotice = (text) => page.locator(".notice span", { hasText: text }).waitFor({ timeout: 20_000 });
const step = async (title, run) => {
  await run();
  console.log(`ok  ${title}`);
};

/** Captures the window through the debugging protocol, which leaves open menus open. */
async function capture(name) {
  const session = await page.context().newCDPSession(page);
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `archives-${name}.png`), Buffer.from(data, "base64"));
  await session.detach();
}

/** The same screenshot in the dark and the light theme. */
async function captureBoth(name, settle = 150) {
  for (const scheme of ["dark", "light"]) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.waitForTimeout(settle);
    await capture(`${name}-${scheme}`);
  }
  await page.emulateMedia({ colorScheme: null });
}

async function goTo(path) {
  await page.keyboard.press("Control+l");
  await page.locator(".address-input").fill(path);
  await page.keyboard.press("Enter");
}

async function menuItem(label) {
  const item = menu.locator(".menu-item", { hasText: new RegExp(`^${escape(label)}`) });
  await item.waitFor();
  return item;
}

const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(error.message));

try {
  await step("the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await goTo(sandbox);
    await row("Project").waitFor();
    await row("slip.zip").waitFor();
  });

  await step("Compress to ZIP file makes Project.zip, then renames it in place", async () => {
    await row("Project").click({ button: "right" });
    await menuItem("Compress to ZIP file");
    await menuItem("Compress to…");
    await captureBoth("1-menu");
    await (await menuItem("Compress to ZIP file")).click();
    const input = panel.locator("input.rename");
    await input.waitFor({ timeout: 20_000 });
    assert.equal(await input.inputValue(), "Project.zip");
    assert.equal(await input.evaluate((field) => field.value.slice(field.selectionStart, field.selectionEnd)), "Project");
    // Keep the name it was given.
    await page.keyboard.press("Escape");
    await row("Project.zip").waitFor();
    assert.deepEqual(await selectedNames(), ["Project.zip"]);
    const names = listed(join(sandbox, "Project.zip")).map((name) => name.replace(/\/$/, ""));
    for (const name of ["Project", "Project/empty", "Project/docs", "Project/docs/guide.md", "Project/notes.txt"]) {
      assert(names.includes(name), `${name} is in the archive: ${names.join(", ")}`);
    }
    assert(!readdirSync(sandbox).some((name) => name.includes(".partial")), "no partial archive is left");
  });

  await step("Compress to… makes a 7z archive with the format chosen", async () => {
    await row("Project").click({ button: "right" });
    await (await menuItem("Compress to…")).click();
    const dialog = page.locator(".archive-compress");
    await dialog.waitFor();
    const name = dialog.locator("input").first();
    // The dialog starts with the format used last; changing it changes the name's extension.
    const format = dialog.locator("select").first();
    await format.selectOption("zip");
    assert.equal(await name.inputValue(), "Project.zip");
    await format.selectOption("tar-gz");
    assert.equal(await name.inputValue(), "Project.tar.gz");
    await format.selectOption("7z");
    assert.equal(await name.inputValue(), "Project.7z");
    await captureBoth("2-compress-dialog");
    await dialog.locator("button", { hasText: /^Compress$/ }).click();
    await row("Project.7z").waitFor({ timeout: 20_000 });
    const names = listed(join(sandbox, "Project.7z")).map((name) => name.replace(/\/$/, ""));
    assert(names.includes("Project/docs/guide.md") && names.includes("Project/empty"), names.join(", "));
  });

  await step("a long compression shows its progress, and Cancel leaves nothing behind", async () => {
    await row("Big").click({ button: "right" });
    await (await menuItem("Compress to…")).click();
    const dialog = page.locator(".archive-compress");
    await dialog.waitFor();
    await dialog.locator("select").first().selectOption("7z");
    await dialog.locator("select").nth(1).selectOption("maximum");
    await dialog.locator("button", { hasText: /^Compress$/ }).click();
    const job = page.locator(".archive-job");
    await job.waitFor({ timeout: 10_000 });
    await job.locator(".archive-job-title", { hasText: "Compressing Big" }).waitFor();
    await page.waitForFunction(() => /of 24\.0 MB/.test(document.querySelector(".archive-job-detail")?.textContent ?? ""), null, { timeout: 10_000 });
    await captureBoth("2b-progress");
    await job.locator("button", { hasText: "Cancel" }).click();
    await job.waitFor({ state: "detached", timeout: 20_000 });
    await page.waitForTimeout(300);
    const left = readdirSync(sandbox).filter((name) => name.startsWith("Big") || name.includes(".partial"));
    assert.deepEqual(left, ["Big"], "no archive and no partial file");
  });

  await step("the zip opens as a folder, and the breadcrumb goes on past it", async () => {
    await row("Project.zip").dblclick();
    await page.locator(".crumb", { hasText: "Project.zip" }).waitFor();
    await row("Project").waitFor();
    await row("Project").dblclick();
    await row("notes.txt").waitFor();
    await row("docs").waitFor();
    await row("empty").waitFor();
    await row("café.txt").waitFor();
    const trail = await crumbs();
    assert.deepEqual(trail.slice(-2), ["Project.zip", "Project"]);
    // Folders first, then files, as in any folder; sizes come from the archive.
    const order = await panel.locator(".row .label").allTextContents();
    assert.deepEqual(order, ["docs", "empty", "café.txt", "notes.txt"]);
    await captureBoth("3-inside");
  });

  await step("a folder inside opens, and Back returns", async () => {
    await row("docs").dblclick();
    await row("guide.md").waitFor();
    assert.equal((await crumbs()).at(-1), "docs");
    await page.keyboard.press("Alt+ArrowLeft");
    await row("notes.txt").waitFor();
  });

  await step("the preview pane and Quick Look show a text file inside", async () => {
    await row("notes.txt").click();
    await page.keyboard.press("Alt+p");
    await pane.waitFor();
    await pane.locator(".preview-code", { hasText: "Hello from inside the archive." }).waitFor({ timeout: 20_000 });
    await pane.locator('.preview-detail[data-label="Size"] dd', { hasText: "44 B" }).waitFor();
    await captureBoth("4-preview", 300);
    await list.focus();
    await page.keyboard.press(" ");
    const look = page.locator(".quicklook");
    await look.locator(".preview-code", { hasText: "Second line." }).waitFor({ timeout: 20_000 });
    await page.keyboard.press("Escape");
    await look.waitFor({ state: "detached" });
    await page.keyboard.press("Alt+p");
    await pane.waitFor({ state: "detached" });
  });

  await step("items inside are read-only, and say so", async () => {
    await row("notes.txt").click();
    await list.focus();
    await page.keyboard.press("Delete");
    await expectNotice("Archives are read-only in Kestrel");
    await page.keyboard.press("F2");
    assert.equal(await panel.locator("input.rename").count(), 0, "no rename box inside an archive");
    await row("notes.txt").click({ button: "right" });
    const labels = await menu.locator(".menu-item .menu-label").allTextContents();
    assert.deepEqual(labels, ["Open", "Extract…", "Copy"]);
    await page.keyboard.press("Escape");
  });

  await step("Extract… takes the selected items out, keeping what is beneath them", async () => {
    await row("docs").click({ button: "right" });
    await (await menuItem("Extract…")).click();
    const dialog = page.locator(".archive-extract");
    await dialog.waitFor();
    assert.equal(await dialog.locator(".archive-title").textContent(), "Extract docs");
    const picked = join(sandbox, `kestrel-test-${process.pid}-picked`);
    await dialog.locator("#archive-destination").fill(picked);
    await dialog.locator('input[type="checkbox"]').uncheck();
    await dialog.locator("button", { hasText: /^Extract$/ }).click();
    await dialog.waitFor({ state: "detached" });
    for (let wait = 0; wait < 100 && !existsSync(join(picked, "docs", "guide.md")); wait++) await page.waitForTimeout(100);
    assert.deepEqual(treeOf(picked), { "docs/": null, "docs/guide.md": "# Guide\n\nRead me.\n" });
    // The tab stays in the archive.
    await row("notes.txt").waitFor();
  });

  await step("Copy and drags carry real files copied out, marked as the archive is", async () => {
    const inside = join(sandbox, "Project.zip", "Project", "notes.txt");
    const [file] = await page.evaluate((paths) => window.__kestrelArchives.materialize(paths), [inside]);
    assert.notEqual(file, inside);
    assert.equal(readFileSync(file, "utf8"), "Hello from inside the archive.\nSecond line.\n");
  });

  await step("Extract all asks where, shows the files and selects the new folder", async () => {
    await goTo(sandbox);
    await row("Project.zip").click({ button: "right" });
    await (await menuItem("Extract all…")).click();
    const dialog = page.locator(".archive-extract");
    await dialog.waitFor();
    const destination = dialog.locator("#archive-destination");
    assert.equal(await destination.inputValue(), join(sandbox, "Project"));
    await destination.fill(extracted);
    const show = dialog.locator('input[type="checkbox"]');
    await show.check();
    await captureBoth("5-extract-dialog");
    const tabsBefore = await page.locator(".tab").count();
    await dialog.locator("button", { hasText: /^Extract$/ }).click();
    await dialog.waitFor({ state: "detached" });
    // "Show extracted files" opens the destination in a new tab.
    await page.waitForFunction((count) => document.querySelectorAll(".tab").length === count + 1, tabsBefore, { timeout: 20_000 });
    await row("Project").waitFor();
    assert.equal((await crumbs()).at(-1), extractedName);
    // The same tree as the folder compressed, Unicode name and empty folder included.
    const expected = { "Project/": null };
    for (const [key, value] of Object.entries(treeOf(project))) expected[`Project/${key}`] = value;
    assert.deepEqual(treeOf(extracted), expected);
    await page.keyboard.press("Control+w");
    await page.waitForFunction((count) => document.querySelectorAll(".tab").length === count, tabsBefore);
    await row(extractedName).waitFor();
    await page.waitForFunction((name) => [...document.querySelectorAll(".tab-panel.active .row.selected .label")].some((label) => label.textContent === name), extractedName, { timeout: 10_000 });
  });

  await step("Undo sends the extraction to the Recycle Bin, and Redo brings it back", async () => {
    await list.focus();
    await page.keyboard.press("Control+z");
    await expectNotice("Undid extraction of Project.zip");
    await row(extractedName).waitFor({ state: "detached" });
    assert(!existsSync(extracted));
    assert.equal(recycledFromSandbox(), 1, "in the Recycle Bin, not erased");
    await captureBoth("6-undone");
    await page.keyboard.press("Control+y");
    await expectNotice("Redid extraction of Project.zip");
    await row(extractedName).waitFor();
    assert(existsSync(join(extracted, "Project", "notes.txt")));
    assert.equal(recycledFromSandbox(), 0, "the Recycle Bin ends as it began");
  });

  await step("an archive whose item would land outside is refused, and nothing is written", async () => {
    await row("slip.zip").click({ button: "right" });
    await (await menuItem("Extract all…")).click();
    const dialog = page.locator(".archive-extract");
    await dialog.waitFor();
    await dialog.locator("button", { hasText: /^Extract$/ }).click();
    await expectNotice("would be written outside the destination folder");
    const text = await page.locator(".notice span").textContent();
    assert.equal(text, "slip.zip wasn't extracted: its item \u201c../evil.txt\u201d would be written outside the destination folder.");
    await captureBoth("7-refused");
    assert(!existsSync(join(sandbox, "slip")), "not even the safe item was extracted");
    assert(!existsSync(join(sandbox, "evil.txt")) && !existsSync(join(tmpdir(), "evil.txt")));
    assert(!readdirSync(sandbox).some((name) => name.includes("(extracting")), "no staging folder is left");
  });

  await step("a RAR archive opens as a folder too, read through Windows' tar.exe", async () => {
    await row("notes.rar").dblclick();
    await row("letters").waitFor({ timeout: 20_000 });
    await row("readme.txt").waitFor();
    await row("letters").dblclick();
    await row("hello.txt").click();
    await page.keyboard.press(" ");
    const look = page.locator(".quicklook");
    await look.locator(".preview-code", { hasText: "Hello from a RAR archive." }).waitFor({ timeout: 20_000 });
    await page.keyboard.press("Escape");
    await look.waitFor({ state: "detached" });
    await goTo(sandbox);
    await row("notes.rar").waitFor();
  });

  await step("an encrypted zip asks for its password, and says when it is wrong", async () => {
    await row("secret.zip").click({ button: "right" });
    await (await menuItem("Extract here")).click();
    const dialog = page.locator(".archive-password");
    await dialog.waitFor({ timeout: 20_000 });
    await dialog.locator(".archive-intro", { hasText: "secret.zip is protected by a password." }).waitFor();
    const field = dialog.locator('input[type="password"]');
    await field.fill("wrong guess");
    await dialog.locator("button", { hasText: /^Continue$/ }).click();
    await dialog.locator(".archive-intro", { hasText: "That password doesn't open secret.zip. Try again." }).waitFor({ timeout: 20_000 });
    assert(!existsSync(join(sandbox, "secret.txt")), "nothing written with the wrong password");
    await field.fill("correct horse");
    await captureBoth("8-password");
    await dialog.locator("button", { hasText: /^Continue$/ }).click();
    await dialog.waitFor({ state: "detached" });
    await row("secret.txt").waitFor({ timeout: 20_000 });
    assert.equal(readFileSync(join(sandbox, "secret.txt"), "utf8"), "the secret");
  });

  assert.deepEqual(pageErrors, [], "the page threw no errors");
  console.log("\nall checks passed");
} finally {
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  // The dialogs remember their choices; put back the defaults this run changed.
  await page
    .evaluate(() =>
      window.__TAURI_INTERNALS__.invoke("archive_set_preferences", { preferences: { show_extracted: true, format: "zip", level: "normal" } }),
    )
    .catch(() => {});
  if (await pane.isVisible().catch(() => false)) await page.keyboard.press("Alt+p").catch(() => {});
  // Leave the app somewhere that still exists before removing the scratch folder.
  await goTo("C:\\").catch(() => {});
  await page.waitForTimeout(300);
  await browser.close();
  if (recycledFromSandbox() > 0) {
    console.error(`!!  items deleted from ${sandbox} are still in the Recycle Bin; restore them from there`);
  }
  rmSync(sandbox, { recursive: true, force: true });
}
