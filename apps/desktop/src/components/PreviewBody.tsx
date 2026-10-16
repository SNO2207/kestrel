import {
  For,
  Match,
  Show,
  Switch,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  on,
  onCleanup,
  type JSX,
} from "solid-js";

import { api, iconUrl, type PreviewDetails, type TextPreview } from "../lib/api";
import { formatCount, formatDate, formatSize } from "../lib/format";
import { highlight } from "../lib/highlight";
import { linkTarget, renderMarkdown } from "../lib/markdown";
import { markLine, revealLine, revealText } from "../lib/previewHit";
import {
  MAX_WHOLE,
  TEXT_LIMIT,
  folderOf,
  itemKey,
  itemVersion,
  kindOf,
  languageOf,
  prettyJson,
  previewUrl,
  type PreviewItem,
  type PreviewKind,
} from "../lib/preview";
import type { SurfaceKind } from "../lib/previewSurface";
import { notify } from "../state";
import { Icon } from "./Icon";
import { PreviewSurface } from "./PreviewSurface";

export type PreviewVariant = "pane" | "quicklook";

interface Props {
  item: PreviewItem | null;
  variant: PreviewVariant;
  /**
   * Milliseconds an item must stay put before it loads, so that holding an arrow key in the
   * list does not start a load for every row passed over.
   */
  settle?: number;
  onClose: () => void;
  closeLabel: string;
}

/** Shows an item: a header, the content as suits its type, and a block of details. */
export function PreviewBody(props: Props) {
  const [shown, setShown] = createSignal<PreviewItem | null>(null);
  /** The preview is built afresh only when this changes; see `itemKey`. */
  const shownKey = createMemo(() => itemKey(shown()));
  let timer: number | undefined;
  /** The item waiting out the settle time, and its latest copy. */
  let waiting = "";
  let latest: PreviewItem | null = null;
  createEffect(
    on(
      () => props.item,
      (item) => {
        const key = itemKey(item);
        latest = item;
        // A refresh hands over the same item as a new object; that must not restart the wait,
        // or a folder that refreshes often would keep the pane from ever catching up.
        if (key === waiting) return;
        clearTimeout(timer);
        waiting = "";
        const settle = props.settle ?? 0;
        if (key === shownKey() || settle <= 0 || shown() === null || item === null) {
          setShown(item);
          return;
        }
        waiting = key;
        timer = window.setTimeout(() => {
          waiting = "";
          setShown(latest);
        }, settle);
      },
    ),
  );
  onCleanup(() => clearTimeout(timer));

  return (
    <Show
      when={shownKey()}
      keyed
      fallback={
        <div class="preview-empty">
          <p>Select an item to preview it.</p>
        </div>
      }
    >
      {(_key: string) => (
        <ItemPreview item={shown()!} variant={props.variant} onClose={props.onClose} closeLabel={props.closeLabel} />
      )}
    </Show>
  );
}

/** A command's error as words; a rejected resource wraps the backend's message in an Error. */
const reason = (error: unknown) => (error instanceof Error ? error.message : String(error));

const ENCODING_LABELS: Record<TextPreview["encoding"], string> = {
  "utf-8": "UTF-8",
  "utf-16le": "UTF-16 LE",
  "utf-16be": "UTF-16 BE",
};

/**
 * One item's preview. It is created afresh for each item (see `itemKey`), so its state never
 * leaks across; a folder's details are read again in place when its size or time changes.
 */
function ItemPreview(props: { item: PreviewItem; variant: PreviewVariant; onClose: () => void; closeLabel: string }) {
  const kind = () => kindOf(props.item);
  // The source is a string, so a refresh that hands over an equal item fetches nothing.
  const [details] = createResource(
    () => itemVersion(props.item),
    () => api.previewDetails(props.item.path),
  );
  /** The details once loaded; reading a failed resource would throw. */
  const known = () => (details.error ? undefined : details());
  /** Online-only files are fetched from the cloud only when the user asks. */
  const [fetchAllowed, setFetchAllowed] = createSignal(false);
  const [measured, setMeasured] = createSignal<[number, number] | null>(null);
  const [duration, setDuration] = createSignal<number | null>(null);
  const [encoding, setEncoding] = createSignal<string | null>(null);

  const typeLabel = () => known()?.type_name ?? (props.item.dir ? "File folder" : "");

  return (
    <div class="preview-body" data-variant={props.variant} data-kind={kind()}>
      <header class="preview-head">
        <img class="preview-icon" src={iconUrl(props.item, 32)} width="32" height="32" alt="" draggable={false} />
        <div class="preview-title">
          <div class="preview-name" title={props.item.path}>
            {props.item.name}
          </div>
          <div class="preview-type">{typeLabel()}</div>
        </div>
        <button class="preview-close" aria-label={props.closeLabel} title={props.closeLabel} onClick={() => props.onClose()}>
          <Icon name="close" size={14} />
        </button>
      </header>

      <div class="preview-stage">
        <Switch>
          <Match when={details.error}>
            <Message>
              <p>Can't preview this item.</p>
              <p class="preview-reason">{reason(details.error)}</p>
            </Message>
          </Match>
          <Match when={known()?.online_only && !fetchAllowed()}>
            <Message>
              <p>This file is stored online only. Previewing it downloads it to this PC.</p>
              <button class="preview-action" onClick={() => setFetchAllowed(true)}>
                Download and preview
              </button>
            </Message>
          </Match>
          <Match when={known()}>
            {(loaded) => (
              <Content
                item={props.item}
                kind={kind()}
                variant={props.variant}
                details={loaded()}
                onMeasured={setMeasured}
                onDuration={setDuration}
                onEncoding={setEncoding}
              />
            )}
          </Match>
        </Switch>
      </div>

      <Show when={known()}>
        {(loaded) => (
          <DetailsList
            details={loaded()}
            dimensions={measured() ?? loaded().dimensions}
            duration={duration()}
            encoding={encoding()}
            compact={props.variant === "quicklook"}
          />
        )}
      </Show>
    </div>
  );
}

function Message(props: { children: JSX.Element }) {
  return <div class="preview-message">{props.children}</div>;
}

/** Opens the file in its default app, for files the preview cannot show. */
function OpenButton(props: { path: string }) {
  return (
    <button class="preview-action" onClick={() => void api.openPath(props.path).catch(notify)}>
      Open
    </button>
  );
}

interface ContentProps {
  item: PreviewItem;
  kind: PreviewKind;
  variant: PreviewVariant;
  details: PreviewDetails;
  onMeasured: (size: [number, number]) => void;
  onDuration: (seconds: number) => void;
  onEncoding: (label: string) => void;
}

/**
 * Reads an image's size, or finds that it can't be shown, without showing it: the preview's
 * own webview does that. An image runs no script wherever it is loaded, an SVG included.
 */
function probeImage(url: string, onMeasured: (size: [number, number]) => void, onFailed: () => void) {
  const image = new Image();
  image.onload = () => {
    if (image.naturalWidth > 0) onMeasured([image.naturalWidth, image.naturalHeight]);
  };
  image.onerror = onFailed;
  image.src = url;
  return () => {
    image.onload = image.onerror = null;
    image.src = "";
  };
}

/** Reads a video's or a sound's length and size from its metadata, without playing or showing it. */
function probeMedia(kind: "video" | "audio", url: string, props: ContentProps, onFailed: () => void) {
  const media = document.createElement(kind);
  media.preload = "metadata";
  media.muted = true;
  media.onloadedmetadata = () => {
    props.onDuration(media.duration);
    if (media instanceof HTMLVideoElement && media.videoWidth > 0) props.onMeasured([media.videoWidth, media.videoHeight]);
  };
  media.onerror = onFailed;
  media.src = url;
  return () => {
    media.onloadedmetadata = media.onerror = null;
    media.removeAttribute("src");
    media.load();
  };
}

function Content(props: ContentProps) {
  // Content is only shown for an online-only file once the user asked to download it.
  const url = () => previewUrl(props.item.path, props.item.mtime, props.details.online_only);
  const [failed, setFailed] = createSignal(false);
  const tooLarge = () => (props.details.size ?? 0) > MAX_WHOLE;
  /** Quick Look, shown over the pane, takes the preview's webview from it. */
  const priority = () => (props.variant === "quicklook" ? 2 : 1);
  const surface = (kind: SurfaceKind, className: string) => (
    <PreviewSurface
      kind={kind}
      path={props.item.path}
      version={props.item.mtime}
      download={props.details.online_only}
      priority={priority()}
      class={className}
      label={`Preview of ${props.item.name}`}
    />
  );

  // What the details list says about images, video and sound is read here; the content itself
  // shows in the preview's own webview.
  createEffect(
    on([() => props.kind, url, tooLarge], ([kind, address, large]) => {
      if (large) return;
      const fail = () => setFailed(true);
      const stop =
        kind === "image"
          ? probeImage(address, props.onMeasured, fail)
          : kind === "video" || kind === "audio"
            ? probeMedia(kind, address, props, fail)
            : null;
      if (stop) onCleanup(stop);
    }),
  );

  return (
    <Switch fallback={<TextContent {...props} />}>
      <Match when={failed()}>
        <Message>
          <p>This file can't be shown here. It may be damaged, or in a format the viewer doesn't support.</p>
        </Message>
      </Match>
      <Match when={props.kind === "folder"}>
        <FolderContent item={props.item} details={props.details} />
      </Match>
      <Match when={props.kind === "image" && tooLarge()}>
        <Message>
          <p>This image is too large to preview.</p>
          <OpenButton path={props.item.path} />
        </Message>
      </Match>
      <Match when={props.kind === "image"}>
        {/* Always an <img> in the viewer page: an SVG's scripts never run in one. */}
        <div class="preview-canvas">{surface("image", "preview-image-surface")}</div>
      </Match>
      <Match when={props.kind === "video"}>{surface("video", "preview-video")}</Match>
      <Match when={props.kind === "audio"}>
        <div class="preview-audio">
          <img src={iconUrl(props.item, 48)} width="48" height="48" alt="" draggable={false} />
          {surface("audio", "preview-audio-controls")}
        </div>
      </Match>
      <Match when={props.kind === "pdf" && tooLarge()}>
        <Message>
          <p>This PDF is too large to preview.</p>
          <OpenButton path={props.item.path} />
        </Message>
      </Match>
      <Match when={props.kind === "pdf"}>{surface("pdf", "preview-pdf")}</Match>
    </Switch>
  );
}

function FolderContent(props: { item: PreviewItem; details: PreviewDetails }) {
  return (
    <div class="preview-summary">
      <img src={iconUrl(props.item, 48)} width="48" height="48" alt="" draggable={false} />
      <Show when={props.details.folder?.readable} fallback={<p>This folder can't be opened.</p>}>
        <p class="preview-count">{itemCount(props.details)}</p>
        <p class="preview-total">{props.details.size === null ? "Size not indexed yet" : formatSize(props.details.size)}</p>
      </Show>
    </div>
  );
}

function itemCount(details: PreviewDetails): string {
  const folder = details.folder;
  if (!folder) return "";
  if (folder.files + folder.folders === 0) return "Empty folder";
  const parts: string[] = [];
  if (folder.folders > 0) parts.push(`${formatCount(folder.folders)} ${folder.folders === 1 ? "folder" : "folders"}`);
  if (folder.files > 0) parts.push(`${formatCount(folder.files)} ${folder.files === 1 ? "file" : "files"}`);
  return parts.join(", ");
}

/** Text, code, Markdown and JSON, and the probe for files of unknown type. */
function TextContent(props: ContentProps) {
  const [text] = createResource(() => props.item.path, (path) => api.previewText(path));
  const loaded = () => (text.error ? undefined : text());
  const [html, setHtml] = createSignal<string | null>(null);
  let markdown: HTMLDivElement | undefined;

  createEffect(() => {
    const preview = loaded();
    if (!preview || preview.binary) return;
    props.onEncoding(ENCODING_LABELS[preview.encoding] + (preview.lossy ? ", some bytes replaced" : ""));
    if (props.kind === "markdown") {
      void renderMarkdown(preview.text, folderOf(props.item.path)).then(setHtml);
      return;
    }
    const json = props.kind === "json";
    // Re-indenting would move the line a search inside files found (`props.item.hit`).
    const code = (json && !preview.truncated && !props.item.hit && prettyJson(preview.text)) || preview.text;
    const language = json ? "json" : (languageOf(props.item) ?? "plaintext");
    void highlight(code, language).then(setHtml);
  });

  // Code blocks in Markdown are highlighted once the document is on screen.
  createEffect(
    on(html, () => {
      if (props.kind !== "markdown" || !markdown) return;
      for (const block of markdown.querySelectorAll<HTMLElement>("pre > code[class*='language-']")) {
        const language = /language-([\w+-]+)/.exec(block.className)?.[1] ?? "plaintext";
        void highlight(block.textContent ?? "", language).then((rendered) => {
          block.innerHTML = rendered;
          block.classList.add("hljs");
        });
      }
    }),
  );

  const onClick = (event: MouseEvent) => {
    if (!markdown) return;
    const link = linkTarget(event, markdown);
    if (link) void api.openLink(link).catch(notify);
  };

  // A search inside files' result opens at its line (`lib/previewHit.ts`).
  let scroller: HTMLDivElement | undefined;
  createEffect(
    on([html, () => props.item.hit], ([shown, hit]) => {
      if (shown === null || !hit) return;
      queueMicrotask(() => {
        if (!scroller) return;
        if (markdown) revealText(scroller, markdown, hit);
        else revealLine(scroller, hit);
      });
    }),
  );

  return (
    <Switch>
      <Match when={text.error}>
        <Message>
          <p>Can't read this file.</p>
          <p class="preview-reason">{reason(text.error)}</p>
        </Message>
      </Match>
      <Match when={loaded()?.binary}>
        <Message>
          <p>There's no preview for this type of file.</p>
        </Message>
      </Match>
      <Match when={loaded()}>
        {(preview) => (
          <div class="preview-text" ref={scroller}>
            <Show when={preview().truncated}>
              <p class="preview-notice" role="note">
                Showing the first {formatSize(TEXT_LIMIT)} of {formatSize(preview().size)}.
              </p>
            </Show>
            <Show
              when={props.kind === "markdown"}
              fallback={
                <pre class="preview-code" classList={{ wrap: props.kind === "text" || props.kind === "other" }}>
                  {/* The plain text shows at once; the highlighted copy replaces it when ready. */}
                  <Show when={html() !== null} fallback={<code class="hljs">{preview().text}</code>}>
                    <code class="hljs" innerHTML={markLine(html() ?? "", props.item.hit?.line)} />
                  </Show>
                </pre>
              }
            >
              <div
                ref={markdown}
                class="preview-markdown"
                innerHTML={html() ?? ""}
                onClick={onClick}
                onAuxClick={(event) => event.preventDefault()}
              />
            </Show>
          </div>
        )}
      </Match>
    </Switch>
  );
}

function formatLength(seconds: number): string {
  const whole = Math.round(seconds);
  const [h, m, s] = [Math.floor(whole / 3600), Math.floor((whole % 3600) / 60), whole % 60];
  const two = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

function DetailsList(props: {
  details: PreviewDetails;
  dimensions: [number, number] | null;
  duration: number | null;
  encoding: string | null;
  compact: boolean;
}) {
  const rows = createMemo(() => {
    const d = props.details;
    const list: [string, string][] = [];
    if (d.folder) {
      if (d.folder.readable) list.push(["Contains", itemCount(d)]);
      list.push(["Total size", d.size === null ? "Not indexed yet" : formatSize(d.size)]);
    } else if (d.size !== null) {
      list.push(["Size", d.size >= 1024 ? `${formatSize(d.size)} (${formatCount(d.size)} bytes)` : formatSize(d.size)]);
    }
    if (props.dimensions) list.push(["Dimensions", `${props.dimensions[0]} × ${props.dimensions[1]} pixels`]);
    if (props.duration !== null && Number.isFinite(props.duration)) list.push(["Length", formatLength(props.duration)]);
    if (props.encoding) list.push(["Encoding", props.encoding]);
    if (d.modified) list.push(["Modified", formatDate(d.modified)]);
    if (!props.compact) {
      if (d.created) list.push(["Created", formatDate(d.created)]);
      if (d.attributes.length > 0) list.push(["Attributes", d.attributes.join(", ")]);
      if (d.path.length > 3) list.push(["Location", folderOf(d.path)]);
    }
    return list;
  });

  return (
    <dl class="preview-details" classList={{ compact: props.compact }}>
      <For each={rows()}>
        {([label, value]) => (
          <div class="preview-detail" data-label={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        )}
      </For>
    </dl>
  );
}
