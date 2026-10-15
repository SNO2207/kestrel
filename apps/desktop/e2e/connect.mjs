// Connects to the running Kestrel window through the web view's debugging port.
//
// The slot (KESTREL_SLOT, default 0) decides which instance: see slot.mjs. Whatever answers on
// the port, the tests only ever act on a page that is unmistakably that slot's Kestrel window,
// because other WebView2 apps on this machine expose debugging ports too.

import { chromium } from "playwright-core";

import { cdpPort, devUrl } from "./slot.mjs";

export const port = cdpPort;

export async function connectToKestrel() {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${cdpPort}`);
  for (const page of browser.contexts().flatMap((context) => context.pages())) {
    if (page.url().startsWith(devUrl) && (await page.title()) === "Kestrel") {
      return { browser, page };
    }
  }
  await browser.close();
  throw new Error(
    `No Kestrel window at ${devUrl} on debugging port ${cdpPort}. Start one with ` +
      "`node e2e/dev.mjs start` (with the same KESTREL_SLOT).",
  );
}
