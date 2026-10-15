import type { Config, DOMPurify } from "dompurify";

import { previewUrl, resolveRelative } from "./preview";

/**
 * Renders Markdown into HTML that is safe to insert into the app's own page.
 *
 * Marked does not sanitise, so its output goes through DOMPurify: no script, event handler,
 * `javascript:` URL, form, frame, image map or inline style survives. On top of that, nothing
 * is fetched from elsewhere: a relative image is shown through the `preview` protocol from the
 * document's drive or share (and not downloaded if it is online only), and any other image
 * source (the web, another drive's root, a share) is dropped, so opening a document cannot
 * reveal to anyone that it was opened. Links stay inert until clicked; see `linkTarget`.
 */
const CONFIG: Config = {
  FORBID_TAGS: [
    "style", "form", "input", "button", "textarea", "select", "option", "iframe", "frame", "frameset",
    "object", "embed", "link", "meta", "base", "svg", "math", "video", "audio", "source", "track", "picture",
    // An image map's areas are links laid invisibly over a picture; clicked, they would
    // navigate the app's window, and no document needs them.
    "map", "area",
  ],
  FORBID_ATTR: ["style", "srcset", "ping", "action", "formaction", "background", "usemap"],
  ALLOW_DATA_ATTR: false,
  // Prefixes ids and names, so content cannot clobber globals such as `document.forms`.
  SANITIZE_NAMED_PROPS: true,
};

const SAFE_DATA_IMAGE = /^data:image\/(png|gif|jpeg|webp|bmp);/i;

let purifier: Promise<{ purify: DOMPurify; parse: (source: string) => string }> | null = null;
/** The folder of the document being sanitised, read by the hook below. */
let documentFolder = "";

function tools() {
  purifier ??= Promise.all([import("marked"), import("dompurify")]).then(([{ Marked }, { default: createPurify }]) => {
    const purify = createPurify(window);
    purify.addHook("afterSanitizeAttributes", (node) => {
      if (node.tagName === "A") {
        node.removeAttribute("target");
        node.setAttribute("rel", "noreferrer");
        const href = node.getAttribute("href");
        if (href && /^(https?:|mailto:)/i.test(href)) node.setAttribute("title", href);
      }
      if (node.tagName === "IMG") {
        const src = node.getAttribute("src") ?? "";
        const local = resolveRelative(documentFolder, src);
        if (local) node.setAttribute("src", previewUrl(local));
        else if (!SAFE_DATA_IMAGE.test(src)) {
          node.removeAttribute("src");
          node.classList.add("blocked-image");
          node.setAttribute("title", "Images from elsewhere are not loaded in previews");
        }
        node.setAttribute("loading", "lazy");
        node.setAttribute("draggable", "false");
      }
    });
    const marked = new Marked({ gfm: true });
    return { purify, parse: (source: string) => marked.parse(source, { async: false }) };
  });
  return purifier;
}

/** Renders a Markdown document that lives in `folder`. */
export async function renderMarkdown(source: string, folder: string): Promise<string> {
  const { purify, parse } = await tools();
  documentFolder = folder;
  return purify.sanitize(parse(source), CONFIG);
}

/**
 * Handles a click inside rendered Markdown. Links do nothing until clicked: then a web or mail
 * link opens in the default app through the shell, an in-page link scrolls, and anything else
 * is ignored. Returns the link to open, if any.
 */
export function linkTarget(event: MouseEvent, container: HTMLElement): string | null {
  const link = (event.target as Element | null)?.closest?.("a[href], area[href]");
  if (!link || !container.contains(link)) return null;
  event.preventDefault();
  const href = link.getAttribute("href") ?? "";
  if (href.startsWith("#")) {
    const id = decodeURIComponent(href.slice(1));
    const target = container.querySelector(`[id="${CSS.escape(id)}"], [id="user-content-${CSS.escape(id)}"]`);
    target?.scrollIntoView({ block: "start" });
    return null;
  }
  return /^(https?:|mailto:)/i.test(href) ? href : null;
}
