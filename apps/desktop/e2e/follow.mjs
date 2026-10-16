// What a suite's page does, followed from the moment the suite connects, for a failing step to
// print (steps.mjs): the app's commands still waiting for an answer and those answered last, and
// where the keyboard went. `connectToKestrel()` starts following the page it returns; the step
// helper starts following any other page it is given (after a restart, a new connection).

/** How many of the commands answered last a failure prints. */
const ANSWERED_TAIL = 12;
/** The app's commands (Tauri's `ipc.localhost` requests) each followed page has sent. */
const commandsOf = new WeakMap();

/** Starts following `page`, unless it is followed already. */
export async function follow(page) {
  if (!page || commandsOf.has(page)) return;
  const commands = { waiting: new Map(), answered: [] };
  commandsOf.set(page, commands);
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
  // The keyboard, in every document the page loads from now on (from its first script, so the
  // keys of a step that runs just after a reload are there too), and in the one it shows now.
  await page.addInitScript(followFocusInPage).catch(() => {});
  page.on("load", () => void page.evaluate(followFocusInPage).catch(() => {}));
  await page.evaluate(followFocusInPage).catch(() => {});
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
  add(document.readyState === "loading" ? "the page started loading" : "followed from here");
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
export async function focusLines(page) {
  const log = await page.evaluate(() => window.__e2eFocusLog ?? null);
  if (!log) return ["keyboard: not followed in this page load"];
  const now = Date.now();
  return [`keyboard, last ${log.length} moves and shortcut keys:`, ...log.map(([at, text]) => `  ${now - at} ms ago: ${text}`)];
}

/** The commands the page is still waiting for, and those answered last, as lines. */
export function commandLines(page) {
  const commands = commandsOf.get(page);
  if (!commands) return ["commands: not followed on this page"];
  const now = Date.now();
  const waiting = [...commands.waiting.values()].map(({ name, at }) => `${name} for ${now - at} ms`);
  return [
    `commands waiting for the app: ${waiting.length ? waiting.join(", ") : "none"}`,
    `commands answered last: ${commands.answered.length ? commands.answered.join(", ") : "none"}`,
  ];
}
