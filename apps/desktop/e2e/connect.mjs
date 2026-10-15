// Connects to the running Kestrel window through the web view's debugging port.
//
// The port defaults to 9333 rather than the usual 9222, because other WebView2 apps (other
// Tauri projects, for instance) commonly hold 9222. Whatever answers on the port, the tests
// only ever act on a page that is unmistakably Kestrel's dev window.

import { chromium } from "playwright-core";

export const port = process.env.KESTREL_CDP_PORT ?? "9333";

export async function connectToKestrel() {
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  for (const page of browser.contexts().flatMap((context) => context.pages())) {
    if (page.url().startsWith("http://localhost:1420") && (await page.title()) === "Kestrel") {
      return { browser, page };
    }
  }
  await browser.close();
  throw new Error(
    `No Kestrel window on port ${port}. Start the app with ` +
      `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=${port}, and check nothing else holds that port.`,
  );
}
