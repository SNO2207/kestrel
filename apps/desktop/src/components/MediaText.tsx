import { Show, createResource, createSignal } from "solid-js";

import { mediaApi, mediaKindOf } from "../lib/mediaApi";
import { notify } from "../state";
import { MediaIcon } from "./MediaIcon";

/**
 * Development builds: where Copy puts the text instead of the clipboard, so the end-to-end suite
 * can check it without touching the clipboard other tests may be using.
 */
let copySink: ((text: string) => void) | null = null;

/**
 * The text the media library recognised in an image, in the details pane, with a button that
 * copies it. Nothing shows for a file that is no image, or an image with no text read (yet).
 */
export function MediaText(props: { path: string; ext: string; mtime: number }) {
  const [text] = createResource(
    () => (mediaKindOf(props.ext) === "photo" ? `${props.path}|${props.mtime}` : null),
    () => mediaApi.text(props.path),
  );
  const [copied, setCopied] = createSignal(false);

  const copy = async (value: string) => {
    try {
      if (copySink) copySink(value);
      else await navigator.clipboard.writeText(value);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      notify(error);
    }
  };

  return (
    <Show when={!text.error && text()}>
      {(value) => (
        <section class="media-text" aria-label="Text in this image">
          <div class="media-text-head">
            <span>Text in this image</span>
            <button class="media-text-copy" onClick={() => void copy(value())} title="Copy the text">
              <MediaIcon name={copied() ? "check" : "copy"} size={14} />
              <span>{copied() ? "Copied" : "Copy"}</span>
            </button>
          </div>
          <pre class="media-text-body">{value()}</pre>
        </section>
      )}
    </Show>
  );
}

if (import.meta.env.DEV) {
  const copiedTexts: string[] = [];
  Object.assign(window, {
    __kestrelMediaText: {
      /** Copies go to a list from now on, and the list so far comes back. */
      capture: () => {
        copySink = (text) => copiedTexts.push(text);
        return copiedTexts;
      },
      release: () => {
        copySink = null;
        copiedTexts.length = 0;
      },
    },
  });
}
