// Connects to the running Kestrel window through the web view's debugging port.
//
// The slot (KESTREL_SLOT, default 0) decides which instance: see slot.mjs. Whatever answers on
// the port, the tests only ever act on a page that is unmistakably that slot's Kestrel window,
// because other WebView2 apps on this machine expose debugging ports too.
//
// Kestrel can have several windows (Ctrl+N). The first is `main`; the others are `main-2`,
// `main-3`... and their pages say so in their address (`index.html?window=main-2`). Tests get
// the first window unless they ask for another.

import { chromium } from "playwright-core";

import { follow } from "./follow.mjs";
import { appUrl, cdpPort, slot } from "./slot.mjs";

export const port = cdpPort;

/** The label of the window a Kestrel page belongs to, from its address. */
export function windowOf(url) {
  try {
    return new URL(url).searchParams.get("window") ?? "main";
  } catch {
    return "main";
  }
}

/** Every page of the slot's Kestrel windows, with each one's window label. */
export async function kestrelPages(browser) {
  const found = [];
  for (const page of browser.contexts().flatMap((context) => context.pages())) {
    if (page.url().startsWith(appUrl) && (await page.title().catch(() => "")) === "Kestrel") {
      found.push({ label: windowOf(page.url()), page });
    }
  }
  return found;
}

/** Waits for the page of the window `label` to appear, as after Ctrl+N, and returns it. */
export async function waitForKestrelWindow(browser, label, timeout = 20_000) {
  const started = Date.now();
  while (Date.now() - started < timeout) {
    const match = (await kestrelPages(browser)).find((candidate) => candidate.label === label);
    if (match) return match.page;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`No Kestrel window ${label} appeared within ${timeout / 1000} s`);
}

/** Whether this process has already started its suite from the window's default settings. */
let freshStart = false;

/**
 * Starts a suite from the window's default settings: views, panes, search and folder options
 * live in the page's local storage and outlast the suite that changed them (a suite killed
 * before its `finally`, or a restore lost when the app is stopped right after it). One left
 * behind, such as search results in tiles or item check boxes on, broke later suites. Only the
 * first connection of a process does this: suites that restart the app and connect again
 * (layout, undo-restart) keep what they set. Never on slot 0, which is the owner's own app.
 */
async function startFresh(page) {
  if (freshStart || slot === 0) return;
  freshStart = true;
  await page.evaluate(() => localStorage.clear());
  await page.reload();
  // Window shortcuts are ignored until the first tab exists.
  await page.locator(".tab").first().waitFor();
}

/** Connects to the slot's Kestrel window `label`: the first window, `main`, unless given. */
export async function connectToKestrel(label = "main") {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  const match = (await kestrelPages(browser)).find((candidate) => candidate.label === label);
  if (match) {
    if (label === "main") await startFresh(match.page);
    // What the page does from now on, for a failing step to print (steps.mjs).
    await follow(match.page);
    return { browser, page: match.page };
  }
  await browser.close();
  throw new Error(
    `No Kestrel window ${label} at ${appUrl} on debugging port ${cdpPort}. Start one with ` +
      "`node e2e/dev.mjs start` (with the same KESTREL_SLOT and KESTREL_RELEASE).",
  );
}
