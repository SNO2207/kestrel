import { For, Show, createResource, createSignal, onMount } from "solid-js";

import { formatCount } from "../lib/format";
import { takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { mediaApi, type MediaStatus } from "../lib/mediaApi";
import { GALLERY, STAGE_LABELS, followMedia, handled, mediaProgress, progressLine, setMediaPaused } from "../media";
import { navigate, notify } from "../state";
import { MediaIcon } from "./MediaIcon";

const languageNames = (() => {
  try {
    return new Intl.DisplayNames(undefined, { type: "language" });
  } catch {
    return null;
  }
})();

const languageName = (tag: string) => {
  try {
    return languageNames?.of(tag) ?? tag;
  } catch {
    return tag;
  }
};

/** The search clauses the library answers, with what each finds. */
const CLAUSES: [string, string][] = [
  ["kind:photo", "photos"],
  ["kind:video", "videos"],
  ["taken:2024-07", "taken in July 2024 (dates as dm: reads them)"],
  ["width:>1920", "wider than 1920 pixels (height: alike)"],
  ["camera:pixel", "taken with a camera whose make or model says Pixel"],
  ["duration:>1m", "videos longer than a minute"],
  ["geo:yes", "with a GPS position"],
  ['text:"order 42"', "with these words in the picture"],
];

/**
 * The media library's settings page (a special location, under the Gallery): which folders it
 * reads, where the background pass stands and what it passed over and why, pausing and
 * resuming it, and the languages it reads text in.
 */
export function MediaLibraryPage(props: LocationPageProps) {
  let page!: HTMLDivElement;
  const [typed, setTyped] = createSignal("");
  const [status, { mutate }] = createResource(() => props.revision, () => mediaApi.status());

  followMedia();
  onMount(() => {
    props.focusRef(() => page.focus());
    if (props.active) queueMicrotask(() => takeFocusIfFree(page));
  });

  const progress = () => mediaProgress() ?? status()?.progress ?? null;
  const scopes = () => status()?.scopes ?? [];

  const change = async (next: string[] | null) => {
    try {
      mutate(await mediaApi.setScopes(next));
    } catch (error) {
      notify(error);
    }
  };

  const add = async (path: string) => {
    const folder = path.trim().replace(/^"|"$/g, "");
    if (!folder) return;
    if (scopes().some((scope) => scope.path.toLowerCase() === folder.toLowerCase())) return setTyped("");
    const before = scopes().length;
    await change([...scopes().map((scope) => scope.path), folder]);
    if (scopes().length === before) notify(`${folder} isn't a folder that can be added, or a folder already in the list holds it.`, "info");
    setTyped("");
  };

  const choose = async () => {
    const picked = await mediaApi.pickFolder(scopes()[0]?.path ?? "").catch(notify);
    if (picked) await add(picked);
  };

  const remove = (path: string) => void change(scopes().map((scope) => scope.path).filter((each) => each !== path));

  const stageLine = (name: string) => {
    const count = progress()?.stages.find((stage) => stage.name === name);
    if (!count) return "";
    const parts = [`${formatCount(count.done)} done`];
    if (count.failed > 0) parts.push(`${formatCount(count.failed)} couldn't be`);
    return parts.join(", ");
  };

  const sameAsDefaults = (current: MediaStatus | undefined) =>
    !!current && !current.custom;

  return (
    <div ref={page} class="media-library" tabindex="-1" aria-label="Media library">
      <header class="media-library-head">
        <div>
          <h1>Media library</h1>
          <p>
            Kestrel reads the photos and videos in these folders in the background, at low priority: their details, a thumbnail
            for the Gallery, and any text in images, so searches can find them. Files are never changed, and files kept online
            only are left alone.
          </p>
        </div>
        <button class="media-button" onClick={() => navigate(props.tabId, GALLERY)}>
          <MediaIcon name="gallery" />
          <span>Open Gallery</span>
        </button>
      </header>

      <section class="media-section" aria-label="Status">
        <h2>Status</h2>
        <Show when={progress()} fallback={<p class="media-muted">Reading the library's state…</p>}>
          {(p) => (
            <>
              <div class="media-status large" data-state={p().state}>
                <span class="media-status-dot" aria-hidden="true" />
                <span class="media-status-text">{progressLine(p())}</span>
                <Show when={p().state !== "starting"}>
                  <button
                    class="media-status-action"
                    data-action={p().state === "paused" ? "resume" : "pause"}
                    onClick={() => void setMediaPaused(p().state !== "paused")}
                  >
                    <MediaIcon name={p().state === "paused" ? "play" : "pause"} size={14} />
                    <span>{p().state === "paused" ? "Resume" : "Pause"}</span>
                  </button>
                </Show>
              </div>
              <dl class="media-counts">
                <div data-count="photos">
                  <dt>Photos</dt>
                  <dd>{formatCount(p().photos)}</dd>
                </div>
                <div data-count="videos">
                  <dt>Videos</dt>
                  <dd>{formatCount(p().videos)}</dd>
                </div>
                <div data-count="done">
                  <dt>Read</dt>
                  <dd>{formatCount(p().done)}</dd>
                </div>
                <div data-count="queued">
                  <dt>Waiting to be read</dt>
                  <dd>{formatCount(p().queued)}</dd>
                </div>
                <div data-count="skipped">
                  <dt>Skipped</dt>
                  <dd>{formatCount(handled(p()) - p().done)}</dd>
                </div>
              </dl>
              <Show when={p().working.length > 0}>
                <p class="media-muted media-working" title={p().working.join("\n")}>
                  Reading {p().working[0]}
                </p>
              </Show>
              <p class="media-note">
                Reading pauses by itself while a game or another app runs full screen, and while battery saver is on.
              </p>
              <h3>Steps</h3>
              <ul class="media-stages">
                <For each={p().stages}>
                  {(stage) => (
                    <li data-stage={stage.name}>
                      <span>{STAGE_LABELS[stage.name] ?? stage.name}</span>
                      <span class="media-muted">{stageLine(stage.name)}</span>
                    </li>
                  )}
                </For>
              </ul>
              <Show when={p().skipped.length > 0}>
                <h3>Skipped, and why</h3>
                <ul class="media-skipped">
                  <For each={p().skipped}>
                    {(skip) => (
                      <li title={skip.example}>
                        <span class="media-skip-reason">{skip.reason}</span>
                        <span class="media-skip-count">{formatCount(skip.count)}</span>
                        <span class="media-muted media-skip-example">For example {skip.example}</span>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </>
          )}
        </Show>
      </section>

      <section class="media-section" aria-label="Folders">
        <h2>Folders</h2>
        <p class="media-muted">
          {status()?.sandboxed
            ? "A scratch folder an end-to-end test chose, in place of the library's folders until Kestrel starts again."
            : sameAsDefaults(status())
              ? "Windows' own folders for your pictures, videos, desktop, downloads and documents, wherever they are kept (OneDrive included)."
              : "Folders you chose."}
        </p>
        <ul class="media-scopes">
          <For each={scopes()} fallback={<li class="media-muted">No folders. Add one below.</li>}>
            {(scope) => (
              <li data-scope={scope.path}>
                <MediaIcon name="folder" />
                <span class="media-scope-path" classList={{ missing: !scope.exists }} title={scope.exists ? scope.path : `${scope.path} (not found)`}>
                  {scope.path}
                </span>
                <button class="media-link" onClick={() => remove(scope.path)} aria-label={`Remove ${scope.path}`}>
                  Remove
                </button>
              </li>
            )}
          </For>
        </ul>
        <form
          class="media-add"
          onSubmit={(event) => {
            event.preventDefault();
            void add(typed());
          }}
        >
          <input
            class="media-input"
            placeholder="Add a folder by its path"
            aria-label="Folder to add"
            spellcheck={false}
            value={typed()}
            onInput={(event) => setTyped(event.currentTarget.value)}
          />
          <button class="media-button" type="submit" disabled={typed().trim() === ""}>
            Add
          </button>
          <button class="media-button" type="button" onClick={() => void choose()}>
            Browse…
          </button>
          <Show when={status()?.custom}>
            <button class="media-link" type="button" onClick={() => void change(null)}>
              Use the default folders
            </button>
          </Show>
        </form>
      </section>

      <section class="media-section" aria-label="Text recognition">
        <h2>Text in images</h2>
        <Show
          when={(status()?.languages.length ?? 0) > 0}
          fallback={
            <p class="media-muted">
              No text recognition language is installed for your Windows languages. Add one in Settings, Time and language,
              Language and region, with its optical character recognition.
            </p>
          }
        >
          <p class="media-muted">Read in {status()!.languages.map(languageName).join(", ")}. The details pane shows an image's text.</p>
        </Show>
      </section>

      <section class="media-section" aria-label="Searching">
        <h2>Searching</h2>
        <p class="media-muted">These work in the search box, with anything else it takes.</p>
        <dl class="media-clauses">
          <For each={CLAUSES}>
            {([clause, meaning]) => (
              <div>
                <dt>
                  <code>{clause}</code>
                </dt>
                <dd>{meaning}</dd>
              </div>
            )}
          </For>
        </dl>
      </section>
      <Show when={status.error}>
        <p class="media-muted">{String(status.error)}</p>
      </Show>
    </div>
  );
}
