// End-to-end test of the preview pane and Quick Look against the real app window.
//
// Everything happens inside a scratch folder this script creates under the temp directory and
// removes afterwards. Nothing is clicked that would leave the app: no web link is followed.
// Several files try to run script (an SVG, an HTML page, a Markdown file); had any of them
// run, it would post a marker to the app and ask the app to create "New folder" in the
// scratch folder, and the test checks that neither ever happens.
//
// Start the app (`node e2e/dev.mjs start`), then run: node e2e/preview.mjs
// Screenshots go to e2e/out/preview-*.png, in a dark and a light pass.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, rmSync, truncateSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { connectToKestrel, port } from "./connect.mjs";
import { pdf, png, wav } from "./fixtures.mjs";
import { devUrl } from "./slot.mjs";

const out = join(dirname(fileURLToPath(import.meta.url)), "out");
mkdirSync(out, { recursive: true });

// ---------- the scratch folder ----------

const sandbox = join(tmpdir(), `kestrel-preview-${process.pid}`);
rmSync(sandbox, { recursive: true, force: true });
mkdirSync(join(sandbox, "sub"), { recursive: true });
const parent = JSON.stringify(sandbox);
/**
 * Somewhere outside the app for content to try to take the window to. Nothing listens on the
 * discard port, so even a navigation that got through would reach nothing beyond this computer.
 */
const AWAY = "http://127.0.0.1:9/kestrel-e2e-away";
/** Script that would prove it ran: a message to the app, and a folder made through its IPC. */
const attack = (name) =>
  `top.postMessage('marker:${name}', '*'); window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke('new_folder', { parent: ${parent} });`;

const files = {
  "photo.png": png(160, 120, (x, y) => (x > 56 && x < 104 && y > 36 && y < 84 ? [240, 180, 41] : [40 + x, 60 + y, 140])),
  "drawing.svg": `<svg xmlns="http://www.w3.org/2000/svg" width="120" height="80" viewBox="0 0 120 80">
  <rect width="120" height="80" rx="10" fill="#6f93c4"/>
  <circle cx="60" cy="40" r="24" fill="#f0b429"/>
  <script><![CDATA[ ${attack("svg")} ]]></script>
</svg>
`,
  "notes.txt": "Kestrel preview test\nA plain text file, shown as it is.\n",
  "main.rs": `//! A small Rust file for syntax highlighting.

/// Greets someone.
fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

fn main() {
    let count = 42;
    println!("{} {count}", greet("Kestrel"));
}
`,
  "readme.md": `# Preview test

Some **bold** text, a [web link](https://example.com/docs) and a [relative link](other.md).

- one
- two

![local image](photo.png)

![remote image](https://example.com/tracker.png)

<img src="missing.png" onerror="window.__mdInjected = true; ${attack("markdown").replace(/"/g, "&quot;")}">

<img src="photo.png" alt="mapped image" width="160" height="120" usemap="#escape"><map name="escape"><area shape="rect" coords="0,0,160,120" href="${AWAY}" alt="away"></map>

<script>window.__mdInjected = true</script>

[a script link](javascript:window.__mdInjected=true)

\`\`\`rust
fn main() {}
\`\`\`
`,
  "data.json": '{"name":"Kestrel","big":12345678901234567890,"list":[1,2,3],"nested":{"ok":true,"empty":{}}}',
  "doc.pdf": pdf(["Kestrel PDF", "Rendered by the built-in viewer"]),
  "page.html": `<!doctype html>\n<title>Active content</title>\n<h1>Never rendered</h1>\n<script>${attack("html")}</script>\n`,
  "tone.wav": wav(1, 440),
  "sub/a.txt": "a".repeat(1000),
  "sub/b.bin": Buffer.alloc(2000, 7),
};
for (const [name, contents] of Object.entries(files)) writeFileSync(join(sandbox, name), contents);
// Larger than the protocol sends whole. Extending a file writes nothing, so this is quick.
writeFileSync(join(sandbox, "huge.png"), "");
truncateSync(join(sandbox, "huge.png"), 65 * 1024 * 1024);
const newFolderMade = () => readdirSync(sandbox).some((name) => name.startsWith("New folder"));

// ---------- the app ----------

const { browser, page } = await connectToKestrel();
await page.reload();

const panel = page.locator(".tab-panel.active");
const list = panel.locator(".list-viewport");
const pane = page.locator(".preview-pane");
const quickLook = page.locator(".quicklook");
const exact = (text) => new RegExp(`^${text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`);
const row = (name) => panel.locator(".row").filter({ has: page.locator(".label", { hasText: exact(name) }) });
const previewUrl = (path) => `http://preview.localhost/?p=${encodeURIComponent(path)}`;
const detail = (scope, label) => scope.locator(`.preview-detail[data-label="${label}"] dd`);
const session = await page.context().newCDPSession(page);
let theme = "dark";

/** Saves a screenshot of the window. Taken through the debugging protocol, which leaves the viewport alone. */
async function shot(name) {
  const { data } = await session.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(out, `preview-${name}-${theme}.png`), Buffer.from(data, "base64"));
}

/** Uncaught errors in the app's page; any at all fails the run. */
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(error.message));

/** Every preview.localhost response, to check statuses and range answers. */
const responses = [];
page.on("response", (response) => {
  if (response.url().startsWith("http://preview.localhost/")) responses.push(response);
});

/**
 * Asks the built-in PDF viewers in this slot's window what they show, and saves a capture of
 * each. The viewer is a separate target on the same debugging port, which window captures
 * do not include. Only called after `connectToKestrel` has confirmed the port is Kestrel's.
 */
async function pdfViewers(name) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const texts = [];
  for (const target of targets.filter((t) => t.url.startsWith("chrome-extension://") && t.url.includes("pdf"))) {
    const socket = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      socket.addEventListener("open", resolve);
      socket.addEventListener("error", reject);
    });
    let next = 1;
    const call = (method, params = {}) =>
      new Promise((resolve) => {
        const id = next++;
        const listener = (event) => {
          const message = JSON.parse(event.data);
          if (message.id !== id) return;
          socket.removeEventListener("message", listener);
          resolve(message.result);
        };
        socket.addEventListener("message", listener);
        socket.send(JSON.stringify({ id, method, params }));
      });
    const result = await call("Runtime.evaluate", { expression: "document.body.innerText", returnByValue: true });
    texts.push(result?.result?.value ?? "");
    const capture = await call("Page.captureScreenshot", { format: "png" });
    if (capture?.data) writeFileSync(join(out, `preview-${name}-viewer-${theme}.png`), Buffer.from(capture.data, "base64"));
    socket.close();
  }
  return texts;
}

async function waitForPdf(name) {
  for (let attempt = 0; attempt < 40; attempt++) {
    const texts = await pdfViewers(name);
    if (texts.some((text) => text.includes("PDF document load has completed") && /\bof 1\b/.test(text))) return;
    await page.waitForTimeout(250);
  }
  throw new Error("the PDF viewer never finished loading the document");
}

/** Waits until a preview (pane or Quick Look) shows the named item's content, and checks it. */
async function expectContent(scope, name) {
  await scope.locator(".preview-name", { hasText: exact(name) }).waitFor();
  switch (name) {
    case "photo.png": {
      const image = scope.locator("img.preview-image");
      await image.waitFor();
      await page.waitForFunction((el) => el.complete && el.naturalWidth === 160, await image.elementHandle());
      assert.equal(await detail(scope, "Dimensions").textContent(), "160 × 120 pixels");
      break;
    }
    case "drawing.svg": {
      const image = scope.locator("img.preview-image");
      await page.waitForFunction((el) => el.complete && el.naturalWidth === 120, await image.elementHandle());
      break;
    }
    case "huge.png":
      // Described, not loaded: the protocol would not send it whole.
      await scope.locator(".preview-message", { hasText: "This image is too large to preview." }).waitFor();
      await scope.locator(".preview-message button", { hasText: exact("Open") }).waitFor();
      assert.equal(await scope.locator("img.preview-image").count(), 0);
      assert.equal(await detail(scope, "Size").textContent(), "65.0 MB (68,157,440 bytes)");
      break;
    case "notes.txt":
      await scope.locator(".preview-code", { hasText: "Kestrel preview test" }).waitFor();
      break;
    case "main.rs":
      await scope.locator(".preview-code .hljs-keyword", { hasText: exact("fn") }).first().waitFor();
      await scope.locator(".preview-code .hljs-string").first().waitFor();
      break;
    case "readme.md": {
      const markdown = scope.locator(".preview-markdown");
      await markdown.locator("h1", { hasText: "Preview test" }).waitFor();
      await markdown.locator("pre code.hljs .hljs-keyword").first().waitFor();
      // The injected <img> arrives as real HTML, so the sanitiser has to act on it.
      assert.equal(await markdown.locator("img[src*='missing.png']").count(), 1, "the injected image is an element");
      assert.equal(await markdown.locator("[onerror], script, [href^='javascript']").count(), 0, "the injection was sanitised");
      // An image map's areas are links outside `linkTarget`'s reach; they must not survive.
      assert.equal(await markdown.locator("map, area, [usemap]").count(), 0, "the image map was removed");
      assert.match(await markdown.locator("img[alt='mapped image']").getAttribute("src"), /^http:\/\/preview\.localhost\/\?p=/);
      assert(!(await markdown.textContent()).includes("onerror"), "nothing of the handler is left, not even as text");
      assert.match(await markdown.locator("img[alt='local image']").getAttribute("src"), /^http:\/\/preview\.localhost\/\?p=/);
      assert.equal(await markdown.locator("img[alt='remote image']").getAttribute("src"), null, "remote images are not loaded");
      assert.equal(await markdown.locator("a", { hasText: "web link" }).getAttribute("target"), null);
      assert.equal(await page.evaluate(() => window.__mdInjected), undefined);
      break;
    }
    case "data.json": {
      const code = scope.locator(".preview-code");
      await code.locator(".hljs-attr").first().waitFor();
      const text = await code.textContent();
      assert(text.includes('  "big": 12345678901234567890,'), "pretty-printed, with the large number exact");
      assert(text.includes('"empty": {}'));
      break;
    }
    case "doc.pdf": {
      const frame = scope.locator("iframe.preview-pdf");
      await frame.waitFor();
      assert.match(await frame.getAttribute("src"), /^http:\/\/preview\.localhost\/\?p=/);
      assert.equal(await frame.getAttribute("sandbox"), null);
      break;
    }
    case "page.html": {
      const code = scope.locator(".preview-code");
      await code.locator(".hljs-tag").first().waitFor();
      assert((await code.textContent()).includes("<script>top.postMessage"), "the page is shown as its source");
      break;
    }
    case "tone.wav": {
      const audio = scope.locator("audio");
      await page.waitForFunction((el) => el.readyState >= 1, await audio.elementHandle());
      await detail(scope, "Length").filter({ hasText: "0:01" }).waitFor();
      break;
    }
    case "clip.webm": {
      const video = scope.locator("video.preview-video");
      await page.waitForFunction((el) => el.readyState >= 1 && el.videoWidth === 160, await video.elementHandle());
      break;
    }
    case "sub":
      await scope.locator(".preview-count", { hasText: "2 files" }).waitFor();
      await detail(scope, "Total size").filter({ hasText: "2.9 KB" }).waitFor({ timeout: 15_000 });
      break;
    default:
      throw new Error(`no expectation for ${name}`);
  }
}

/** Runs one check. A check that hangs fails after a while, so the clean-up below still runs. */
const step = async (title, run, limit = 240_000) => {
  let timer;
  const hung = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`timed out: ${title}`)), limit);
  });
  try {
    await Promise.race([run(), hung]);
  } finally {
    clearTimeout(timer);
  }
  console.log(`ok  ${title}`);
};

/** The order the list shows the sandbox in: folders first, then by name. */
const ORDER = ["sub", "clip.webm", "data.json", "doc.pdf", "drawing.svg", "huge.png", "main.rs", "notes.txt", "page.html", "photo.png", "readme.md", "tone.wav"];

try {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.evaluate(() => {
    window.__markers = [];
    window.addEventListener("message", (event) => window.__markers.push(String(event.data)));
  });

  await step("the index is ready and the scratch folder opens", async () => {
    await panel.locator(".row").first().waitFor();
    await page.locator(".index-state").filter({ hasText: "files indexed" }).waitFor({ timeout: 180_000 });
    // The pane remembers whether it was open; start from closed.
    if (await pane.isVisible()) await page.keyboard.press("Alt+p");
    await pane.waitFor({ state: "detached" });
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("photo.png").waitFor();
  });

  await step("a short video recorded in the web view joins the scratch folder", async () => {
    const base64 = await page.evaluate(async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 160;
      canvas.height = 90;
      const context = canvas.getContext("2d");
      const recorder = new MediaRecorder(canvas.captureStream(25), { mimeType: "video/webm" });
      const chunks = [];
      recorder.ondataavailable = (event) => chunks.push(event.data);
      const stopped = new Promise((resolve) => (recorder.onstop = resolve));
      recorder.start();
      const started = performance.now();
      await new Promise((resolve) => {
        const timer = setInterval(() => {
          const t = performance.now() - started;
          context.fillStyle = "#1b2129";
          context.fillRect(0, 0, 160, 90);
          context.fillStyle = "#f0b429";
          context.fillRect((t / 8) % 130, 30, 30, 30);
          if (t > 1200) {
            clearInterval(timer);
            resolve();
          }
        }, 40);
      });
      recorder.stop();
      await stopped;
      const bytes = new Uint8Array(await new Blob(chunks).arrayBuffer());
      let binary = "";
      for (const byte of bytes) binary += String.fromCharCode(byte);
      return btoa(binary);
    });
    writeFileSync(join(sandbox, "clip.webm"), Buffer.from(base64, "base64"));
    await row("clip.webm").waitFor({ timeout: 5000 });
  });

  await step("opening the subfolder once puts its size in the index", async () => {
    await row("sub").dblclick();
    await row("a.txt").waitFor();
    await page.keyboard.press("Alt+ArrowUp");
    await row("photo.png").waitFor();
  });

  await step("Alt+P opens the pane, which describes the folder while nothing is focused", async () => {
    await list.focus();
    await page.keyboard.press("Alt+p");
    await pane.waitFor();
    await pane.locator(".preview-name", { hasText: exact(sandbox.split("\\").pop()) }).waitFor();
    await detail(pane, "Contains").filter({ hasText: "1 folder, 11 files" }).waitFor();
    await shot("pane-folder");
  });

  await step("the folder's details follow its contents, without rebuilding the preview", async () => {
    const preview = await pane.locator(".preview-body").elementHandle();
    writeFileSync(join(sandbox, "extra.txt"), "extra");
    await detail(pane, "Contains").filter({ hasText: "1 folder, 12 files" }).waitFor({ timeout: 5000 });
    rmSync(join(sandbox, "extra.txt"));
    await detail(pane, "Contains").filter({ hasText: "1 folder, 11 files" }).waitFor({ timeout: 5000 });
    assert(await preview.evaluate((el) => el.isConnected), "the same preview stayed on screen");
  });

  await step("the toolbar button hides and shows the pane", async () => {
    const toggle = page.locator(".preview-toggle");
    assert.equal(await toggle.getAttribute("aria-pressed"), "true");
    await toggle.click();
    await pane.waitFor({ state: "detached" });
    assert.equal(await toggle.getAttribute("aria-pressed"), "false");
    await toggle.click();
    await pane.waitFor();
  });

  for (const name of ORDER) {
    await step(`the pane previews ${name}`, async () => {
      await row(name).click();
      await expectContent(pane, name);
      if (name === "doc.pdf") await waitForPdf("pane-doc.pdf");
      await shot(`pane-${name}`);
    });
  }

  await step("the pane follows the focus as the arrow keys move it", async () => {
    await row("main.rs").click();
    await expectContent(pane, "main.rs");
    await page.keyboard.press("ArrowDown");
    await expectContent(pane, "notes.txt");
  });

  await step("the pane catches up while the listing keeps refreshing", async () => {
    await row("notes.txt").click();
    await expectContent(pane, "notes.txt");
    await page.keyboard.press("ArrowDown");
    // Each refresh hands the pane the same item again; that must not hold back its loading.
    let refreshing = true;
    const burst = (async () => {
      const until = Date.now() + 2000;
      while (refreshing && Date.now() < until) {
        await page.keyboard.press("F5");
        await page.waitForTimeout(30);
      }
    })();
    try {
      await pane.locator(".preview-name", { hasText: exact("page.html") }).waitFor({ timeout: 1500 });
    } finally {
      refreshing = false;
      await burst;
    }
    await expectContent(pane, "page.html");
  });

  await step("a relative link in Markdown is not followed", async () => {
    await row("readme.md").click();
    await expectContent(pane, "readme.md");
    const before = page.url();
    await pane.locator(".preview-markdown a", { hasText: "relative link" }).click();
    await page.waitForTimeout(300);
    assert.equal(page.url(), before);
    assert.equal(await page.locator(".notice").count(), 0);
    // The image that carried an image map is only an image now.
    await pane.locator(".preview-markdown img[alt='mapped image']").click();
    await page.waitForTimeout(300);
    assert.equal(page.url(), before, "clicking the mapped image left the app");
    assert.equal(
      await pane.locator(".preview-markdown a", { hasText: "web link" }).getAttribute("title"),
      "https://example.com/docs",
      "a web link names where it goes",
    );
  });

  await step("Space opens Quick Look, and the arrows step through every item without closing it", async () => {
    await row("sub").click();
    await page.keyboard.press("Space");
    await quickLook.waitFor();
    await expectContent(quickLook, "sub");
    await shot("quicklook-sub");
    for (const name of ORDER.slice(1)) {
      await page.keyboard.press("ArrowRight");
      await expectContent(quickLook, name);
      if (name === "doc.pdf") await waitForPdf("quicklook-doc.pdf");
      await shot(`quicklook-${name}`);
    }
    await page.keyboard.press("ArrowLeft");
    await expectContent(quickLook, "readme.md");
    await page.keyboard.press("ArrowUp");
    await expectContent(quickLook, "photo.png");
    await page.keyboard.press("ArrowDown");
    await expectContent(quickLook, "readme.md");
    // The list followed along underneath.
    assert.deepEqual(await panel.locator(".row.focused .label").allTextContents(), ["readme.md"]);
  });

  await step("Space and Escape close Quick Look, and clicking outside it does too", async () => {
    await page.keyboard.press("Space");
    await quickLook.waitFor({ state: "detached" });
    assert(await list.evaluate((el) => el === document.activeElement), "the list has the focus back");
    await page.keyboard.press("Space");
    await quickLook.waitFor();
    await page.keyboard.press("Escape");
    await quickLook.waitFor({ state: "detached" });
    await page.keyboard.press("Space");
    await quickLook.waitFor();
    // Below the dialog, over the status bar, where a click does nothing else.
    const viewport = await page.evaluate(() => ({ width: innerWidth, height: innerHeight }));
    await page.mouse.click(40, viewport.height - 8);
    await quickLook.waitFor({ state: "detached" });
  });

  await step("dragging the pane's edge resizes it, and the width is remembered", async () => {
    const before = (await pane.boundingBox()).width;
    const handle = await page.locator(".preview-resizer").boundingBox();
    const x = handle.x + handle.width / 2;
    const y = handle.y + 200;
    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.mouse.move(x - 60, y, { steps: 4 });
    await page.mouse.move(x - 120, y, { steps: 4 });
    await page.mouse.up();
    const after = (await pane.boundingBox()).width;
    assert(Math.abs(after - before - 120) <= 2, `width went from ${before} to ${after}`);
    await page.reload();
    await pane.waitFor();
    assert(Math.abs((await pane.boundingBox()).width - after) <= 1, "the width survived a reload");
    await page.locator(".preview-resizer").dblclick();
    assert(Math.abs((await pane.boundingBox()).width - 380) <= 1, "double-click restores the default");
    await page.evaluate(() => {
      window.__markers = [];
      window.addEventListener("message", (event) => window.__markers.push(String(event.data)));
    });
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("photo.png").waitFor();
  });

  await step("the same previews in the light theme", async () => {
    theme = "light";
    await page.emulateMedia({ colorScheme: "light" });
    for (const name of ["photo.png", "main.rs", "readme.md", "data.json", "doc.pdf", "sub"]) {
      await row(name).click();
      await expectContent(pane, name);
      if (name === "doc.pdf") await waitForPdf("pane-doc.pdf");
      await shot(`pane-${name}`);
    }
    await row("photo.png").click();
    await page.keyboard.press("Space");
    await expectContent(quickLook, "photo.png");
    await shot("quicklook-photo.png");
    await page.keyboard.press("ArrowRight");
    await expectContent(quickLook, "readme.md");
    await shot("quicklook-readme.md");
    await page.keyboard.press("Escape");
    await quickLook.waitFor({ state: "detached" });
    theme = "dark";
    await page.emulateMedia({ colorScheme: "dark" });
  });

  // ---------- security ----------

  await step("the protocol answers media with ranges and refuses folders", async () => {
    const ranged = responses.filter((r) => r.status() === 206 && /(tone\.wav|clip\.webm)/.test(decodeURIComponent(r.url())));
    assert(ranged.length > 0, "audio and video were read with range requests");
    assert.match(ranged[0].headers()["content-range"] ?? "", /^bytes \d+-\d+\/\d+$/);
    const status = await page.evaluate(async (url) => {
      const image = new Image();
      const loaded = new Promise((resolve) => {
        image.onload = () => resolve("loaded");
        image.onerror = () => resolve("refused");
      });
      image.src = url;
      return loaded;
    }, previewUrl(join(sandbox, "sub")));
    assert.equal(status, "refused");
    await page.waitForTimeout(200);
    assert(responses.some((r) => r.status() === 403 && decodeURIComponent(r.url()).endsWith("\\sub")), "a folder is refused with 403");
  });

  await step("no previewed script ran: no message, no folder made through the IPC", async () => {
    assert.deepEqual(await page.evaluate(() => window.__markers), []);
    assert.equal(await page.evaluate(() => window.__mdInjected), undefined);
    assert(!newFolderMade(), "a script reached the IPC and made a folder");
  });

  await step("HTML and SVG opened as documents from preview.localhost run no script and cannot call the IPC", async () => {
    for (const name of ["page.html", "drawing.svg"]) {
      await page.evaluate((src) => {
        const frame = document.createElement("iframe");
        frame.className = "e2e-probe";
        frame.src = src;
        document.body.append(frame);
        return new Promise((resolve) => frame.addEventListener("load", resolve, { once: true }));
      }, previewUrl(join(sandbox, name)));
      const frame = page.frames().find((f) => decodeURIComponent(f.url()).endsWith(name));
      assert(frame, `the ${name} frame loaded`);
      const facts = await frame.evaluate(() => ({ origin: self.origin, type: document.contentType }));
      assert.equal(facts.origin, "null", `${name} is sandboxed, so its origin is opaque`);
      assert.equal(facts.type, name.endsWith(".html") ? "text/plain" : "image/svg+xml");
      // Code forced into the document through the debugging protocol, which the file's own
      // scripts could never do, still gets nowhere: Tauri refuses an opaque origin. Timers do
      // not run in a document without script, so the wait is kept out here.
      for (const command of ["index_status", "plugin:app|version", "new_folder"]) {
        const call = frame
          .evaluate(
            ({ command, parent }) =>
              window.__TAURI_INTERNALS__.invoke(command, { parent }).then(
                (value) => `answered ${JSON.stringify(value)}`,
                (error) => `refused: ${error}`,
              ),
            { command, parent: sandbox },
          )
          .catch((error) => `refused: ${error.message}`);
        const outcome = await Promise.race([call, new Promise((resolve) => setTimeout(() => resolve("no answer"), 4000))]);
        assert(!outcome.startsWith("answered"), `${name} reached ${command}: ${outcome}`);
      }
    }
    await page.waitForTimeout(500);
    assert.deepEqual(await page.evaluate(() => window.__markers), []);
    assert(!newFolderMade());
    await page.evaluate(() => document.querySelectorAll(".e2e-probe").forEach((frame) => frame.remove()));
  });

  await step("the PDF frame, the one unsandboxed preview document, cannot reach the app's commands or page", async () => {
    await row("doc.pdf").click();
    await expectContent(pane, "doc.pdf");
    await waitForPdf("pane-doc.pdf");
    const frame = page.frames().find((f) => f.url().startsWith("http://preview.localhost/") && decodeURIComponent(f.url()).includes("doc.pdf"));
    assert(frame, "the PDF frame is there");
    const probe = await frame.evaluate(async (parent) => {
      const settle = (promise) =>
        Promise.race([promise, new Promise((resolve) => setTimeout(() => resolve("no answer"), 3000))]).then(
          (value) => `answered ${JSON.stringify(value)}`,
          (error) => `refused: ${error}`,
        );
      let page;
      try {
        page = window.parent.document.title;
      } catch (error) {
        page = `blocked: ${error.name}`;
      }
      const internals = window.__TAURI_INTERNALS__;
      return {
        origin: self.origin,
        page,
        status: internals ? await settle(internals.invoke("index_status")) : "no bridge",
        write: internals ? await settle(internals.invoke("new_folder", { parent })) : "no bridge",
        // A file that does not exist, so even a failure of the guard would open nothing.
        open: internals ? await settle(internals.invoke("open_path", { path: `${parent}\\missing.txt` })) : "no bridge",
      };
    }, sandbox);
    assert.equal(probe.origin, "http://preview.localhost");
    assert.equal(probe.page, "blocked: SecurityError");
    for (const call of [probe.status, probe.write, probe.open]) {
      assert.match(call, /^refused: Commands are not available to http:\/\/preview\.localhost/, JSON.stringify(probe));
    }
    assert(!newFolderMade());
    // The app's own page still may.
    assert.equal(await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("index_status").then((s) => typeof s.entries)), "number");
  });

  await step("only web and mail links are handed to the shell", async () => {
    const refused = await page.evaluate(() =>
      window.__TAURI_INTERNALS__.invoke("open_link", { url: "file:///C:/Windows/System32/calc.exe" }).then(
        () => "opened",
        (error) => String(error),
      ),
    );
    assert.equal(refused, "Only web and mail links can be opened");
  });

  await step("an item that is gone is explained in words", async () => {
    const missing = await page.evaluate(
      (path) => window.__TAURI_INTERNALS__.invoke("preview_details", { path }).then(() => "found", (error) => String(error)),
      join(sandbox, "gone.txt"),
    );
    assert.equal(missing, "It no longer exists.");
  });

  await step("the window refuses to leave the app's page", async () => {
    const home = page.url();
    await page.evaluate((away) => {
      location.href = away;
    }, AWAY);
    await page.waitForTimeout(1500);
    assert.equal(page.url(), home, "the window navigated away");
    assert.equal(await page.title(), "Kestrel");
    assert.equal(await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("index_status").then((s) => typeof s.entries)), "number");
  });

  await step("Markdown on a share cannot name another server", async () => {
    // Worked out in the app's own page, from its own modules, without loading anything: the
    // rendered HTML is only parsed, never shown.
    const result = await page.evaluate(async () => {
      const { resolveRelative } = await import("/src/lib/preview.ts");
      const { renderMarkdown } = await import("/src/lib/markdown.ts");
      const folder = "\\\\fileserver\\team\\docs";
      const html = await renderMarkdown("![escape](../../../attacker.example/share/pixel.png)\n\n![inside](../images/ok.png)", folder);
      const doc = new DOMParser().parseFromString(html, "text/html");
      return {
        escape: resolveRelative(folder, "../../../attacker.example/share/pixel.png"),
        webdav: resolveRelative(folder, "../../../attacker.example@SSL@443/s/x.png"),
        otherShare: resolveRelative(folder, "../../other/x.png"),
        inside: resolveRelative(folder, "../images/ok.png"),
        drive: resolveRelative("C:\\docs", "../../x.png"),
        escapeSrc: doc.querySelector("img[alt='escape']")?.getAttribute("src") ?? null,
        insideSrc: doc.querySelector("img[alt='inside']")?.getAttribute("src") ?? null,
      };
    });
    assert.deepEqual(
      { ...result, insideSrc: decodeURIComponent(result.insideSrc ?? "") },
      {
        escape: null,
        webdav: null,
        otherShare: null,
        inside: "\\\\fileserver\\team\\images\\ok.png",
        drive: null,
        escapeSrc: null,
        insideSrc: "http://preview.localhost/?p=\\\\fileserver\\team\\images\\ok.png&v=0",
      },
    );
  });

  await step("an online-only image in Markdown is not downloaded; the image itself asks first", async () => {
    const folder = join(sandbox, "cloud");
    mkdirSync(folder);
    // The second spelling opens the same file: Windows drops a trailing dot.
    writeFileSync(join(folder, "notes.md"), "# Cloud notes\n\n![cloud image](picture.png)\n\n![respelled](picture.png.)\n");
    writeFileSync(join(folder, "picture.png"), files["photo.png"]);
    // The attribute a cloud placeholder whose contents are elsewhere carries.
    execFileSync("attrib", ["+O", join(folder, "picture.png")]);
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(folder);
    await page.keyboard.press("Enter");
    await row("notes.md").click();
    const markdown = pane.locator(".preview-markdown");
    await markdown.locator("h1", { hasText: "Cloud notes" }).waitFor();
    for (const alt of ["cloud image", "respelled"]) {
      const image = markdown.locator(`img[alt='${alt}']`);
      await page.waitForFunction((el) => el.complete, await image.elementHandle());
      assert.equal(await image.evaluate((el) => el.naturalWidth), 0, `the online-only image was read as ${alt}`);
    }
    await page.waitForTimeout(200);
    assert(
      responses.some((r) => r.status() === 403 && decodeURIComponent(r.url()).includes("\\cloud\\picture.png")),
      "the protocol refused it",
    );
    await shot("pane-online-only-markdown");

    await row("picture.png").click();
    await pane.locator(".preview-message", { hasText: "stored online only" }).waitFor();
    assert.equal(await pane.locator("img.preview-image").count(), 0);
    await shot("pane-online-only");
    await pane.locator(".preview-action", { hasText: "Download and preview" }).click();
    const shown = pane.locator("img.preview-image");
    await shown.waitFor();
    await page.waitForFunction((el) => el.complete && el.naturalWidth === 160, await shown.elementHandle());
    assert.match(await shown.getAttribute("src"), /&download=1$/);
    await page.keyboard.press("Control+l");
    await page.locator(".address-input").fill(sandbox);
    await page.keyboard.press("Enter");
    await row("photo.png").waitFor();
  });

  await step("the page raised no uncaught errors", async () => {
    assert.deepEqual(pageErrors, []);
  });

  console.log("\nall checks passed; screenshots are in", out);
} catch (error) {
  await shot("failure").catch(() => {});
  if (pageErrors.length > 0) console.error("page errors:", pageErrors);
  throw error;
} finally {
  // Should something have taken the window away from the app, bring it back first.
  if (!page.url().startsWith(devUrl)) await page.goto(devUrl).catch(() => {});
  await page.emulateMedia({ colorScheme: null }).catch(() => {});
  await page.keyboard.press("Escape").catch(() => {});
  // Leave the pane closed and the app somewhere that still exists, then remove the folder.
  if (await pane.isVisible().catch(() => false)) await page.keyboard.press("Alt+p").catch(() => {});
  await page.keyboard.press("Control+l").catch(() => {});
  await page.locator(".address-input").fill("C:\\").catch(() => {});
  await page.keyboard.press("Enter").catch(() => {});
  await page.waitForTimeout(300);
  await session.detach().catch(() => {});
  await browser.close();
  rmSync(sandbox, { recursive: true, force: true });
}
