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
import { appUrl, release } from "./slot.mjs";
import { webviews } from "./windows.mjs";

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
const record = (response) => {
  if (response.url().startsWith("http://preview.localhost/")) responses.push(response);
};
page.on("response", record);

// ---------- the preview's own webview ----------

/**
 * The preview's own webview, where images, video, sound and PDF documents show. It is a
 * second page on the same debugging port, created the first time something is shown in it.
 */
let viewPage = null;
async function previewView() {
  for (let attempt = 0; !viewPage && attempt < 150; attempt++) {
    viewPage = browser.contexts().flatMap((context) => context.pages()).find((other) => other !== page && /^(http:\/\/preview\.localhost\/|about:blank)/.test(other.url())) ?? null;
    if (viewPage) viewPage.on("response", record);
    else await page.waitForTimeout(100);
  }
  assert(viewPage, "the preview's webview is there");
  return viewPage;
}

/**
 * Runs `check` in the preview's webview until it returns something truthy. Its viewer page
 * runs no script, so it can't wait by itself; the debugging protocol still evaluates there.
 */
async function inView(check, arg, timeout = 15_000) {
  const view = await previewView();
  const until = Date.now() + timeout;
  let last;
  while (Date.now() < until) {
    last = await view.evaluate(check, arg).catch((error) => `error: ${error.message}`);
    if (last && !String(last).startsWith("error:")) return last;
    await page.waitForTimeout(100);
  }
  throw new Error(`the preview's webview never got there (${check}): ${last}`);
}

/** Waits until the preview's webview shows `name` (its address names the file). */
const viewShows = (name) =>
  inView((name) => decodeURIComponent(location.href).includes(`\\${name}&`) && location.href, name);

/** Checks that the preview's webview has the size of the scope's preview surface. */
async function expectPlaced(scope) {
  const box = await scope.locator(".preview-surface").boundingBox();
  assert(box, "the preview surface is on the page");
  const fits = ([width, height]) => Math.abs(width - box.width) <= 2 && Math.abs(height - box.height) <= 2;
  await inView(([width, height]) => Math.abs(innerWidth - width) <= 2 && Math.abs(innerHeight - height) <= 2, [box.width, box.height]).catch(async () => {
    const size = await (await previewView()).evaluate(() => [innerWidth, innerHeight]);
    assert(fits(size), `view ${size} over surface ${box.width}x${box.height}`);
  });
}

/**
 * The preview's webview as Windows shows it, which its page can't tell: whether it is shown,
 * and where, in CSS pixels of the app's page. Waits up to `timeout` for `wanted` to hold.
 */
async function nativeView(wanted, timeout = 10_000) {
  const until = Date.now() + timeout;
  let seen;
  do {
    const scale = await page.evaluate(() => devicePixelRatio);
    const [main, view] = webviews();
    seen = view && main ? { visible: view.visible, x: (view.left - main.left) / scale, y: (view.top - main.top) / scale, width: view.width / scale, height: view.height / scale } : null;
    if (seen && wanted(seen)) return seen;
  } while (Date.now() < until);
  throw new Error(`the preview's webview is not as expected: ${JSON.stringify(seen)}`);
}

/**
 * Saves the window as the user sees it while the preview's webview shows something: a capture
 * of the page does not include that webview, so its own capture is drawn in at its place.
 */
async function shotView(name) {
  const view = await previewView();
  const viewSession = await view.context().newCDPSession(view);
  const { data: content } = await viewSession.send("Page.captureScreenshot", { format: "png" });
  await viewSession.detach();
  const { data: window } = await session.send("Page.captureScreenshot", { format: "png" });
  // Quick Look's preview has the webview whenever it is open.
  const inQuickLook = (await page.locator(".quicklook .preview-surface").count()) > 0;
  const box = await page.locator(inQuickLook ? ".quicklook .preview-surface" : ".preview-pane .preview-surface").boundingBox();
  const combined = await page.evaluate(
    async ([window, content, box]) => {
      const load = (data) =>
        new Promise((resolve, reject) => {
          const image = new Image();
          image.onload = () => resolve(image);
          image.onerror = reject;
          image.src = `data:image/png;base64,${data}`;
        });
      const [whole, part] = await Promise.all([load(window), load(content)]);
      const canvas = document.createElement("canvas");
      [canvas.width, canvas.height] = [whole.width, whole.height];
      const context = canvas.getContext("2d");
      context.drawImage(whole, 0, 0);
      const scale = whole.width / innerWidth;
      context.drawImage(part, box.x * scale, box.y * scale, box.width * scale, box.height * scale);
      return canvas.toDataURL("image/png").split(",")[1];
    },
    [window, content, box],
  );
  writeFileSync(join(out, `preview-${name}-${theme}.png`), Buffer.from(combined, "base64"));
}

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
      // The image shows in the preview's own webview; the page only holds its place.
      await scope.locator(".preview-surface[data-kind='image']").waitFor();
      await viewShows(name);
      await inView(() => document.querySelector("img")?.complete && document.querySelector("img").naturalWidth === 160);
      await expectPlaced(scope);
      await detail(scope, "Dimensions").filter({ hasText: "160 × 120 pixels" }).waitFor();
      assert.equal(await scope.locator("img.preview-image, video, audio, iframe").count(), 0, "no content in the app's page");
      break;
    }
    case "drawing.svg": {
      await scope.locator(".preview-surface[data-kind='image']").waitFor();
      await viewShows(name);
      await inView(() => document.querySelector("img")?.complete && document.querySelector("img").naturalWidth === 120);
      await expectPlaced(scope);
      break;
    }
    case "huge.png":
      // Described, not loaded: the protocol would not send it whole.
      await scope.locator(".preview-message", { hasText: "This image is too large to preview." }).waitFor();
      await scope.locator(".preview-message button", { hasText: exact("Open") }).waitFor();
      assert.equal(await scope.locator(".preview-surface").count(), 0);
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
      await scope.locator(".preview-surface[data-kind='pdf']").waitFor();
      assert.match(await viewShows(name), /^http:\/\/preview\.localhost\/\?p=/, "the PDF is the webview's own page");
      await expectPlaced(scope);
      assert.equal(await scope.locator("iframe").count(), 0, "no frame in the app's page");
      break;
    }
    case "page.html": {
      const code = scope.locator(".preview-code");
      await code.locator(".hljs-tag").first().waitFor();
      assert((await code.textContent()).includes("<script>top.postMessage"), "the page is shown as its source");
      break;
    }
    case "tone.wav": {
      await scope.locator(".preview-surface[data-kind='audio']").waitFor();
      await viewShows(name);
      await inView(() => document.querySelector("audio")?.readyState >= 1);
      await expectPlaced(scope);
      await detail(scope, "Length").filter({ hasText: "0:01" }).waitFor();
      break;
    }
    case "clip.webm": {
      await scope.locator(".preview-surface[data-kind='video']").waitFor();
      await viewShows(name);
      await inView(() => document.querySelector("video")?.readyState >= 1 && document.querySelector("video").videoWidth === 160);
      await expectPlaced(scope);
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
const step = (await import("./steps.mjs")).stepper("preview", () => page, { limit: 240_000 });

/** The order the list shows the sandbox in: folders first, then by name. */
const ORDER = ["sub", "clip.webm", "data.json", "doc.pdf", "drawing.svg", "huge.png", "main.rs", "notes.txt", "page.html", "photo.png", "readme.md", "tone.wav"];
/** The items shown in the preview's own webview rather than in the app's page. */
const IN_VIEW = new Set(["clip.webm", "doc.pdf", "drawing.svg", "photo.png", "tone.wav"]);

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
      if (IN_VIEW.has(name)) await shotView(`pane-${name}`);
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
      if (IN_VIEW.has(name)) await shotView(`quicklook-${name}`);
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
    for (const name of ["photo.png", "main.rs", "readme.md", "data.json", "doc.pdf", "clip.webm", "sub"]) {
      await row(name).click();
      await expectContent(pane, name);
      if (name === "doc.pdf") await waitForPdf("pane-doc.pdf");
      await shot(`pane-${name}`);
      if (IN_VIEW.has(name)) await shotView(`pane-${name}`);
    }
    // The viewer page follows the theme too.
    await row("photo.png").click();
    await expectContent(pane, "photo.png");
    assert.equal(await inView(() => getComputedStyle(document.body).backgroundColor), "rgb(251, 252, 253)");
    await page.keyboard.press("Space");
    await expectContent(quickLook, "photo.png");
    await shot("quicklook-photo.png");
    await shotView("quicklook-photo.png");
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

  /**
   * Calls commands from inside the preview's webview, through the IPC bridge Tauri injects
   * there as everywhere: Tauri's own plugin commands, which the old in-page PDF frame could
   * still reach, and the app's commands. Every call must be refused.
   */
  const commandsFromView = async () => {
    const view = await previewView();
    // The wait is kept out here: timers don't run in the sandboxed viewer page.
    const call = (command, args) =>
      Promise.race([
        view
          .evaluate(
            ([command, args]) =>
              window.__TAURI_INTERNALS__
                ? window.__TAURI_INTERNALS__.invoke(command, args).then(
                    (value) => `answered ${JSON.stringify(value)}`,
                    (error) => `refused: ${error}`,
                  )
                : "no bridge",
            [command, args],
          )
          .catch((error) => `refused: ${error.message}`),
        new Promise((resolve) => setTimeout(() => resolve("no answer"), 4000)),
      ]);
    return {
      origin: await view.evaluate(() => self.origin),
      // Core plugin commands: granted to the app's webview by `core:default`.
      version: await call("plugin:app|version"),
      emit: await call("plugin:event|emit", { event: "kestrel-e2e-from-preview", payload: null }),
      title: await call("plugin:window|title", { label: "main" }),
      webviews: await call("plugin:webview|get_all_webviews"),
      // The app's own commands.
      status: await call("index_status"),
      write: await call("new_folder", { parent: sandbox }),
      // A file that does not exist, so even a failure of the guard would open nothing.
      open: await call("open_path", { path: `${sandbox}\\missing.txt` }),
    };
  };

  await step("the preview's own webview, where a PDF shows unsandboxed, can call no command at all", async () => {
    // The app's own page may call the same core command, so a refusal below is the webview's.
    const own = await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("plugin:app|version"));
    assert.equal(typeof own, "string");
    await row("doc.pdf").click();
    await expectContent(pane, "doc.pdf");
    await waitForPdf("pane-doc.pdf");
    const probe = await commandsFromView();
    assert.equal(probe.origin, "http://preview.localhost", "the PDF is not sandboxed, so its origin is not opaque");
    for (const command of ["version", "emit", "title", "webviews"]) {
      assert.match(probe[command], /^refused: .*not allowed/, `plugin command ${command}: ${JSON.stringify(probe)}`);
    }
    for (const command of ["status", "write", "open"]) {
      assert.match(probe[command], /^refused: Commands are not available to the preview view/, `${command}: ${JSON.stringify(probe)}`);
    }
    assert(!newFolderMade());
    // And from the viewer page images and media show in, which is sandboxed as well.
    await row("photo.png").click();
    await expectContent(pane, "photo.png");
    const fromImage = await commandsFromView();
    assert.equal(fromImage.origin, "null");
    for (const [command, outcome] of Object.entries(fromImage)) {
      if (command !== "origin") assert(!outcome.startsWith("answered"), `${command} answered in the image viewer: ${outcome}`);
    }
    assert(!newFolderMade());
    // The app's own page still may call its commands.
    assert.equal(await page.evaluate(() => window.__TAURI_INTERNALS__.invoke("index_status").then((s) => typeof s.entries)), "number");
  });

  await step("the preview's webview lies over the pane, steps aside for the app's menus, and empties when nothing is shown", async () => {
    await row("photo.png").click();
    await expectContent(pane, "photo.png");
    const surface = await pane.locator(".preview-surface").boundingBox();
    const near = (a, b) => Math.abs(a - b) <= 2;
    await nativeView((view) => view.visible && near(view.x, surface.x) && near(view.y, surface.y) && near(view.width, surface.width) && near(view.height, surface.height));
    // Right-clicked at the row's far end, the menu opens over the pane.
    const box = await row("photo.png").boundingBox();
    await row("photo.png").click({ button: "right", position: { x: box.width - 4, y: box.height / 2 } });
    await page.locator(".menu").waitFor();
    const menuBox = await page.locator(".menu").boundingBox();
    assert(menuBox.x + menuBox.width > surface.x, "the menu reaches over the preview");
    await nativeView((view) => !view.visible);
    await shot("pane-menu-over-view");
    await page.keyboard.press("Escape");
    await page.locator(".menu").waitFor({ state: "detached" });
    await nativeView((view) => view.visible);
    // Quick Look over the pane: its image takes the webview, and while it shows text instead,
    // the pane's image must not show through over the dialog.
    await list.focus();
    await page.keyboard.press("Space");
    await expectContent(quickLook, "photo.png");
    const dialog = await quickLook.locator(".preview-surface").boundingBox();
    await nativeView((view) => view.visible && near(view.width, dialog.width) && near(view.height, dialog.height));
    await page.keyboard.press("ArrowRight");
    await expectContent(quickLook, "readme.md");
    await nativeView((view) => !view.visible);
    await page.keyboard.press("Escape");
    await quickLook.waitFor({ state: "detached" });
    // The list followed Quick Look to readme.md, so the pane shows text now.
    await expectContent(pane, "readme.md");
    await nativeView((view) => !view.visible);
    await row("photo.png").click();
    await expectContent(pane, "photo.png");
    await nativeView((view) => view.visible && near(view.x, surface.x) && near(view.width, surface.width));
    await row("notes.txt").click();
    await expectContent(pane, "notes.txt");
    await nativeView((view) => !view.visible);
    await inView(() => location.href === "about:blank");
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

  // The check below imports the app's source modules from the dev server.
  if (release) console.log("--  Markdown on a share cannot name another server: skipped (needs a dev build)");
  else await step("Markdown on a share cannot name another server", async () => {
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
    assert.equal(await pane.locator(".preview-surface").count(), 0);
    await shot("pane-online-only");
    await pane.locator(".preview-action", { hasText: "Download and preview" }).click();
    await pane.locator(".preview-surface[data-kind='image']").waitFor();
    assert.match(await viewShows("picture.png"), /&download=1(&|$)/);
    await inView(() => document.querySelector("img")?.complete && document.querySelector("img").naturalWidth === 160);
    assert.match(await inView(() => document.querySelector("img").getAttribute("src")), /&download=1$/);
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
  if (!page.url().startsWith(appUrl)) await page.goto(appUrl).catch(() => {});
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
