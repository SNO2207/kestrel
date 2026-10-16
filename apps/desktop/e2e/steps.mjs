// Every suite's `step(title, run)`, and what a failing step leaves behind to see why it failed.
//
//   const step = (await import("./steps.mjs")).stepper("fileops", () => page);
//   await step("New folder (Ctrl+Shift+N)", async () => { ... });
//
// Each suite makes its `step` with that one line where its own definition used to be (imported
// there, not at the top, so the change sits in one place in every file). `page` is passed as a
// function because some suites connect again after restarting the app.
//
// A step that passes prints `ok  <title>`, with `timed` also how long it took. A step that fails
// prints `FAILED  <title>` and, before its error reaches the suite's `finally` blocks (which close
// menus and panes and go back to folders), describes the window as the step left it:
//   - a screenshot, e2e/out/failures/<suite>-<step>.png, taken through the debugging protocol
//     (Playwright's own screenshot resizes the viewport for an instant, which closes menus);
//   - the element that has the keyboard, and the open menus, dialogs and notices, and any
//     window of the app's own process the page can't show (a shell dialog waiting for an answer);
//   - the active tab's address, and how many rows each list on screen draws and how many of them
//     have no `data-path` yet (their page of rows is still on its way);
//   - the app's commands the page still waits for, and those answered last, and the last moves of
//     the keyboard and shortcut keys (both followed from the suite's first step);
//   - the page's last 40 console messages (a message repeated back to back counts once), over the
//     page's whole life rather than since the suite connected: a new debugging session hears again
//     everything the page has logged.
// Then the step's own error goes on. Nothing here may hide that error: every part of the
// description has a time limit, and a part that fails says so and lets the others run.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

import { otherWindows } from "./windows.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const failures = join(here, "out", "failures");
/** How many of the page's console messages a failure prints. */
const CONSOLE_TAIL = 40;
/** How long each part of a failure's description may take (a page that hangs never answers). */
const PART_LIMIT_MS = 8000;

/**
 * Makes a suite's `step(title, run)`. `suite` names the screenshots; `pageOf()` returns the page to
 * describe when a step fails. Options: `timed` adds the time a passing step took to its line;
 * `limit` (ms) fails a step that has not finished by then with "timed out: <title>".
 */
export function stepper(suite, pageOf, { timed = false, limit = 0 } = {}) {
  return async (title, run) => {
    const started = Date.now();
    watchCommands(pageOf);
    try {
      await (limit ? withinLimit(run(), limit, `timed out: ${title}`) : run());
    } catch (error) {
      console.log(`FAILED  ${title}`);
      await describeFailure(suite, title, pageOf);
      throw error;
    }
    console.log(timed ? `ok  ${title} (${Date.now() - started} ms)` : `ok  ${title}`);
  };
}

/** How many of the commands answered last a failure prints. */
const ANSWERED_TAIL = 12;
/** The app's commands (Tauri's `ipc.localhost` requests) each page has sent, from the first step on. */
const commandsOf = new WeakMap();

/**
 * Starts following the commands the page sends the app, if it isn't followed yet: a step that hangs
 * is often waiting for one that never comes back.
 */
function watchCommands(pageOf) {
  let page;
  try {
    page = pageOf();
  } catch {
    return;
  }
  if (!page || commandsOf.has(page)) return;
  const commands = { waiting: new Map(), answered: [] };
  commandsOf.set(page, commands);
  // And where the keyboard goes, from now and after every reload.
  const followFocus = () => page.evaluate(followFocusInPage).catch(() => {});
  page.on("load", followFocus);
  void followFocus();
  const nameOf = (request) => {
    const match = /^https?:\/\/ipc\.localhost\/([^?#]+)/.exec(request.url());
    return match ? decodeURIComponent(match[1]) : null;
  };
  page.on("request", (request) => {
    const name = nameOf(request);
    if (name) commands.waiting.set(request, { name, at: Date.now() });
  });
  const done = (outcome) => (request) => {
    const sent = commands.waiting.get(request);
    if (!sent) return;
    commands.waiting.delete(request);
    commands.answered.push(`${sent.name} ${outcome} after ${Date.now() - sent.at} ms`);
    if (commands.answered.length > ANSWERED_TAIL) commands.answered.shift();
  };
  page.on("requestfinished", done("answered"));
  page.on("requestfailed", done("failed"));
}

/**
 * Runs in the page: keeps the last 30 moves of the keyboard (the element it went to, or that it
 * left for nothing, and the window losing or getting it) and shortcut keys (with Ctrl or Alt, or
 * named keys such as F2 and Escape; never typed text), for a failure to print.
 */
function followFocusInPage() {
  if (window.__e2eFocusLog) return;
  const log = (window.__e2eFocusLog = []);
  const add = (text) => {
    log.push([Date.now(), text]);
    if (log.length > 30) log.shift();
  };
  const describe = (element) => {
    if (!element || !element.tagName) return "nothing";
    let name = element.tagName.toLowerCase();
    if (typeof element.className === "string" && element.className.trim()) name += `.${element.className.trim().split(/\s+/).join(".")}`;
    const label = element.getAttribute?.("aria-label");
    return label ? `${name}[aria-label="${label}"]` : name;
  };
  document.addEventListener("focusin", (event) => add(`keyboard to ${describe(event.target)}`), true);
  document.addEventListener("focusout", (event) => event.relatedTarget === null && add(`keyboard left ${describe(event.target)} for nothing`), true);
  window.addEventListener("blur", (event) => event.target === window && add("the window lost the keyboard"));
  window.addEventListener("focus", (event) => event.target === window && add("the window got the keyboard"));
  window.addEventListener(
    "keydown",
    (event) => {
      if (!event.ctrlKey && !event.altKey && event.key.length === 1) return;
      const key = `${event.ctrlKey ? "Ctrl+" : ""}${event.altKey ? "Alt+" : ""}${event.shiftKey && event.key.length > 1 ? "Shift+" : ""}${event.key}`;
      add(`key ${key} on ${describe(event.target)}`);
    },
    true,
  );
}

/** The page's record of where the keyboard went, as lines, latest last. */
async function focusLines(page) {
  const log = await page.evaluate(() => window.__e2eFocusLog ?? null);
  if (!log) return ["keyboard: not followed in this page load"];
  const now = Date.now();
  return [`keyboard, last ${log.length} moves and shortcut keys:`, ...log.map(([at, text]) => `  ${now - at} ms ago: ${text}`)];
}

/** The commands the page is still waiting for, and those answered last, as lines. */
function commandLines(page) {
  const commands = commandsOf.get(page);
  if (!commands) return ["commands: not followed (no step ran on this page)"];
  const now = Date.now();
  const waiting = [...commands.waiting.values()].map(({ name, at }) => `${name} for ${now - at} ms`);
  return [
    `commands waiting for the app: ${waiting.length ? waiting.join(", ") : "none"}`,
    `commands answered last: ${commands.answered.length ? commands.answered.join(", ") : "none"}`,
  ];
}

/** `work`, or a rejection with `message` once `ms` have passed. */
async function withinLimit(work, ms, message) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms);
  });
  try {
    return await Promise.race([work, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** Prints what the window looked like when `title` failed, and saves its screenshot. */
async function describeFailure(suite, title, pageOf) {
  let page;
  try {
    page = pageOf();
    if (!page) throw new Error("the suite has no page yet");
  } catch (error) {
    console.log(`  (no page to describe: ${error.message})`);
    return;
  }
  const part = async (name, work) => {
    try {
      return await withinLimit(work(), PART_LIMIT_MS, `no answer within ${PART_LIMIT_MS / 1000} s`);
    } catch (error) {
      console.log(`  ${name}: unavailable (${String(error.message ?? error).split("\n")[0]})`);
      return undefined;
    }
  };

  // The screenshot first: everything after it reads the page without changing it.
  const file = join(failures, `${suite}-${fileName(title)}.png`);
  const saved = await part("screenshot", () => screenshot(page, file));
  if (saved) console.log(`  screenshot: ${relative(join(here, ".."), file)}`);

  const state = await part("window", () => page.evaluate(readWindow));
  if (state) for (const line of state) console.log(`  ${line}`);
  for (const line of commandLines(page)) console.log(`  ${line}`);
  const keyboard = await part("keyboard", () => focusLines(page));
  if (keyboard) for (const line of keyboard) console.log(`  ${line}`);

  // Windows of the app's that the page knows nothing of: the shell's dialogs (a copy waiting to
  // be told whether to replace a file), message boxes, the Windows menu's helper.
  const others = await part("other windows", () => otherWindows(PART_LIMIT_MS));
  if (others) console.log(`  other windows of the app: ${others.length ? others.join("; ") : "none"}`);

  const messages = await part("console", () => consoleMessages(page));
  if (messages) {
    const tail = messages.lines.slice(-CONSOLE_TAIL);
    console.log(`  console: last ${tail.length} lines of ${messages.total} messages`);
    for (const message of tail) console.log(`    ${message}`);
  }
}

/** A title as a file name: letters and digits, the rest as single dashes. */
function fileName(title) {
  return (
    title
      .replace(/[^A-Za-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 100) || "step"
  );
}

/** Saves the window as it is through the debugging protocol, which leaves the viewport alone. */
async function screenshot(page, file) {
  const session = await page.context().newCDPSession(page);
  try {
    const { data } = await session.send("Page.captureScreenshot", { format: "png" });
    mkdirSync(failures, { recursive: true });
    writeFileSync(file, Buffer.from(data, "base64"));
    return true;
  } finally {
    await session.detach().catch(() => {});
  }
}

/**
 * Every message the page has logged since it loaded, oldest first, as lines. A new session that
 * enables the Runtime domain is sent the page's stored console calls and uncaught exceptions
 * again, and one that enables the Log domain the browser's own entries (failed requests and the
 * like); both arrive before the answer to a later command.
 */
async function consoleMessages(page) {
  const session = await page.context().newCDPSession(page);
  const found = [];
  session.on("Runtime.consoleAPICalled", (event) => {
    found.push({ time: event.timestamp, kind: event.type, text: event.args.map(argumentText).join(" ") });
  });
  session.on("Runtime.exceptionThrown", ({ timestamp, exceptionDetails }) => {
    const thrown = exceptionDetails.exception?.description ?? exceptionDetails.text;
    found.push({ time: timestamp, kind: "uncaught", text: String(thrown).split("\n").slice(0, 3).join(" | ") });
  });
  session.on("Log.entryAdded", ({ entry }) => {
    const where = entry.url ? ` (${entry.url}${entry.lineNumber !== undefined ? `:${entry.lineNumber + 1}` : ""})` : "";
    found.push({ time: entry.timestamp, kind: `${entry.source} ${entry.level}`, text: `${entry.text}${where}` });
  });
  try {
    await session.send("Runtime.enable");
    await session.send("Log.enable");
    // One more round trip, so every message sent before it has been heard.
    await session.send("Runtime.evaluate", { expression: "0" });
  } finally {
    await session.detach().catch(() => {});
  }
  const seen = new Set();
  const unique = found
    .sort((a, b) => a.time - b.time)
    .filter(({ time, text }) => {
      // The same message from both domains is printed once.
      const key = `${Math.round(time)} ${text}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  // A message repeated back to back is one line with its count, so a burst of one warning
  // doesn't push everything else out of the tail.
  const runs = [];
  for (const message of unique) {
    const last = runs[runs.length - 1];
    if (last && last.kind === message.kind && last.text === message.text) {
      last.count++;
      last.until = message.time;
    } else {
      runs.push({ ...message, count: 1, until: message.time });
    }
  }
  return {
    total: unique.length,
    lines: runs.map(({ time, until, count, kind, text }) => {
      const when = count > 1 ? `${clock(time)} to ${clock(until)}` : clock(time);
      const shown = text.length > 400 ? `${text.slice(0, 400)}…` : text;
      return `${when} ${kind}: ${shown}${count > 1 ? ` (${count} times)` : ""}`;
    }),
  };
}

/** A console call's argument as the console would show it, briefly. */
function argumentText(argument) {
  if (argument.type === "string") return argument.value;
  if (argument.unserializableValue !== undefined) return argument.unserializableValue;
  if (argument.value !== undefined) return JSON.stringify(argument.value);
  return argument.description ?? argument.type;
}

/** A protocol timestamp (ms since 1970) as the local time of day, to the millisecond. */
function clock(time) {
  const at = new Date(time);
  const two = (n) => String(n).padStart(2, "0");
  return `${two(at.getHours())}:${two(at.getMinutes())}:${two(at.getSeconds())}.${String(at.getMilliseconds()).padStart(3, "0")}`;
}

/**
 * Runs in the page: the keyboard's element, menus, dialogs, notices, the active tab's address and
 * the rows of each list on screen, as lines.
 */
function readWindow() {
  const visible = (element) =>
    element.isConnected && (element.checkVisibility ? element.checkVisibility() : element.getClientRects().length > 0);
  const text = (element, length = 160) => {
    const words = (element.innerText ?? element.textContent ?? "").replace(/\s+/g, " ").trim();
    return words.length > length ? `${words.slice(0, length)}…` : words;
  };
  const describe = (element) => {
    if (!element) return "none";
    let name = element.tagName.toLowerCase();
    if (element.id) name += `#${element.id}`;
    for (const each of element.classList) name += `.${each}`;
    for (const attribute of ["role", "aria-label", "title", "data-path", "data-index", "data-tab-id", "data-panel-tab"]) {
      const value = element.getAttribute(attribute);
      if (value !== null) name += `[${attribute}=${JSON.stringify(value.length > 120 ? `${value.slice(0, 120)}…` : value)}]`;
    }
    return name;
  };
  /** The visible elements matching `selector`, leaving out any inside another one listed. */
  const outermost = (selector) => {
    const all = [...document.querySelectorAll(selector)].filter(visible);
    return all.filter((element) => !all.some((other) => other !== element && other.contains(element)));
  };
  const lines = [];

  const focused = document.activeElement;
  const around = focused?.closest?.(
    "[role=dialog], [role=alertdialog], .menu, .tab-panel, .sidebar, .toolbar, .titlebar, .commandbar, .statusbar, .preview-pane, .details-pane, .terminal-pane, .projects-hub, .disk-hub, .pane-bar",
  );
  let focus = describe(focused);
  if (focused && "value" in focused && typeof focused.value === "string") focus += ` value=${JSON.stringify(focused.value)}`;
  if (focused && focused.isContentEditable) focus += ` text=${JSON.stringify(text(focused, 80))}`;
  if (around && around !== focused) focus += ` inside ${describe(around)}`;
  lines.push(`focus: ${focus}${document.hasFocus() ? "" : " (the page itself does not have the keyboard)"}`);

  const menus = outermost(".menu, [role=menu]");
  lines.push(`menus: ${menus.length || "none"}`);
  for (const menu of menus) {
    const items = [...menu.querySelectorAll("[role=menuitem], [role=menuitemcheckbox], [role=menuitemradio], .menu-item")]
      .map((item) => text(item, 60))
      .filter(Boolean);
    lines.push(`  ${describe(menu)}: ${items.slice(0, 20).join(" | ")}${items.length > 20 ? ` | … (${items.length})` : ""}`);
  }

  const dialogs = outermost(
    "[role=dialog], [role=alertdialog], dialog[open], .dialog-backdrop, .quicklook-backdrop, .palette-backdrop, .goto-backdrop, .projects-hub, .disk-hub",
  );
  lines.push(`dialogs: ${dialogs.length || "none"}`);
  for (const dialog of dialogs) lines.push(`  ${describe(dialog)}: ${text(dialog)}`);

  const notices = outermost(".notice, [role=alert], [role=status], .drop-hint, .archive-jobs").filter((notice) => text(notice));
  lines.push(`notices: ${notices.length || "none"}`);
  for (const notice of notices) lines.push(`  ${describe(notice)}: ${text(notice)}`);

  const panels = [...document.querySelectorAll(".tab-panel")];
  const active = document.querySelector(".tab-panel.active");
  const tabOf = (panel) => document.querySelector(`[data-tab-id="${panel?.dataset.panelTab}"]`);
  const crumbs = [...document.querySelectorAll(".address .crumb")].map((crumb) => crumb.textContent?.trim()).join(" > ");
  const editing = document.querySelector(".address.editing input");
  const search = document.querySelector(".searchbox input");
  // Quoted as typed: JSON would double every backslash of a Windows path.
  const quote = (value) => `"${value}"`;
  lines.push(
    `active tab: ${active ? `#${active.dataset.panelTab}` : "none"} of ${panels.length}, ` +
      `address ${quote(tabOf(active)?.getAttribute("title") ?? "")}, crumbs ${quote(crumbs)}` +
      (editing ? `, address box ${quote(editing.value)}` : "") +
      (search?.value ? `, search box ${quote(search.value)}` : ""),
  );

  // The lists on screen: the active tab's, and with two panes the other pane's shown tab.
  for (const panel of panels.filter((each) => each.classList.contains("active") || each.classList.contains("current"))) {
    const items = [...panel.querySelectorAll(".row[data-index], .view-item[data-index]")];
    const waiting = items.filter((item) => !item.hasAttribute("data-path")).length;
    const list = panel.querySelector(".filelist");
    const error = panel.querySelector(".list-error");
    const parts = [
      `${items.length} rows drawn, ${waiting} without data-path`,
      panel.classList.contains("location-panel") ? "a location's page over the list" : null,
      list ? `list ${describe(list)}` : "no list",
      error ? `error ${JSON.stringify(text(error))}` : null,
    ].filter(Boolean);
    lines.push(`rows in tab #${panel.dataset.panelTab}${panel === active ? "" : " (other pane)"}: ${parts.join(", ")}`);
  }
  // Development builds count the lists' swaps to new listings (FileList.tsx), and hold new
  // listings at a gate while a test asks them to.
  const swaps = window.__kestrelSwaps;
  if (Array.isArray(swaps) && swaps.length) {
    const last = swaps[swaps.length - 1];
    lines.push(
      `last list swap: ${last.kind}, ${Date.now() - last.at} ms ago, after waiting ${Math.round(last.waited)} ms for its rows`,
    );
  }
  const held = window.__kestrelStage?.held?.();
  if (held) lines.push(`new listings held at the gate: ${held}`);
  const status = document.querySelector(".statusbar");
  if (status) lines.push(`status bar: ${JSON.stringify(text(status))}`);
  return lines;
}
