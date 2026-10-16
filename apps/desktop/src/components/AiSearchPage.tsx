/**
 * The AI search page (a special location: type "AI search" in the address bar, or open it from
 * the palette or the describe bar): the two models with what each is, its licence, source and
 * exact size, a Download button that says all that (nothing is downloaded until it is clicked),
 * the download's progress with Cancel, Delete, the self-test's results, and what describing
 * in the background is doing, with Pause and the folders it looks in.
 */
import { For, Index, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { archiveApi } from "../lib/archivesApi";
import { api } from "../lib/api";
import { formatCount, formatDate, formatSize } from "../lib/format";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { semanticApi, type ModelView, type SelfTestReport } from "../lib/semanticApi";
import { refreshSemantic, semanticState, watchSemantic } from "../semantic";
import { MEDIA_LIBRARY } from "../media";
import { navigate, notify, places } from "../state";
import { confirmAction } from "./ConfirmDialog";
import { Icon } from "./Icon";

const bytes = new Intl.NumberFormat();

function speed(perSecond: number): string {
  return perSecond > 0 ? `${formatSize(Math.round(perSecond))}/s` : "";
}

function SelfTest(props: { report: SelfTestReport; model: string; busy: boolean; onRun: () => void }) {
  return (
    <div class="ai-selftest" data-passed={props.report.passed ? "yes" : "no"}>
      <div class="ai-selftest-head">
        <span class="ai-selftest-verdict" classList={{ passed: props.report.passed }}>
          <Icon name={props.report.passed ? "check" : "close"} size={14} />
          {props.report.passed ? "Self-test passed" : "Self-test failed"}
        </span>
        <Show when={props.report.imagesPerSecond > 0}>
          <span class="ai-muted ai-throughput">
            {props.report.imagesPerSecond >= 10 ? Math.round(props.report.imagesPerSecond) : props.report.imagesPerSecond.toFixed(1)} pictures a second on{" "}
            {props.report.device.gpu ? props.report.device.name : "the processor"}
          </span>
        </Show>
        <button class="ai-button ai-run-selftest" disabled={props.busy} onClick={() => props.onRun()}>
          {props.busy ? "Testing…" : "Run self-test"}
        </button>
      </div>
      <Show when={props.report.device.note}>
        <p class="ai-note">{props.report.device.note}</p>
      </Show>
      <Show when={props.report.error}>
        <p class="ai-error">{props.report.error}</p>
      </Show>
      <Show when={props.report.checks.length > 0}>
        <table class="ai-checks">
          <thead>
            <tr>
              <th>Matched</th>
              <th>Should find</th>
              <th>Found</th>
              <th class="ai-num">Scores</th>
            </tr>
          </thead>
          <tbody>
            <For each={props.report.checks}>
              {(check) => (
                <tr classList={{ failed: !check.passed }}>
                  <td>{check.subject}</td>
                  <td>{check.expected}</td>
                  <td>
                    <span class="ai-check-mark" classList={{ passed: check.passed }}>
                      <Icon name={check.passed ? "check" : "close"} size={12} />
                    </span>
                    {check.best}
                  </td>
                  <td class="ai-num" title="Its own pair's similarity, then the best other one's">
                    {check.expectedScore.toFixed(3)} / {check.runnerUp.toFixed(3)}
                  </td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
}

function ModelCard(props: { model: ModelView; onChanged: () => void }) {
  const [testing, setTesting] = createSignal(false);
  const download = () => props.model.download;
  const downloading = () => download()?.state === "downloading" || download()?.state === "testing";
  /** A download that stopped part way: its files so far are kept, to resume or discard. */
  const stopped = () => download()?.state === "cancelled" || download()?.state === "failed";
  const short = () => props.model.revision.slice(0, 8);

  const start = () => void semanticApi.download(props.model.id).then(props.onChanged, notify);
  const cancel = () => void semanticApi.cancelDownload(props.model.id).catch(notify);
  const runTest = async () => {
    setTesting(true);
    try {
      await semanticApi.selfTest(props.model.id);
    } catch (error) {
      notify(error);
    } finally {
      setTesting(false);
      props.onChanged();
    }
  };
  const remove = async () => {
    const yes = await confirmAction({
      title: `Delete the ${props.model.title} model?`,
      message: `This deletes its ${formatSize(props.model.totalSize)} of files and the descriptions made with it from this PC, for good (they don't go to the Recycle Bin). You can download it again.`,
      action: "Delete",
      danger: true,
    });
    if (yes) await semanticApi.remove(props.model.id).then(props.onChanged, notify);
  };
  const discard = async () => {
    const yes = await confirmAction({
      title: `Discard the ${props.model.title} model's download?`,
      message: `This deletes the ${formatSize(download()?.done ?? 0)} that arrived, for good; downloading again starts from the beginning.`,
      action: "Discard",
      danger: true,
    });
    if (yes) await semanticApi.remove(props.model.id).then(props.onChanged, notify);
  };

  return (
    <section class="ai-model" data-model={props.model.id} classList={{ active: props.model.active }} aria-label={`${props.model.title} model`}>
      <header class="ai-model-head">
        <h2>{props.model.title}</h2>
        <span class="ai-model-sub">
          SigLIP 2, {props.model.imageSize} × {props.model.imageSize} pixels
        </span>
        <Show when={props.model.active}>
          <span class="ai-badge ai-in-use">In use</span>
        </Show>
        <Show when={props.model.installed && !props.model.active}>
          <span class="ai-badge">Downloaded</span>
        </Show>
      </header>
      <p class="ai-summary">{props.model.summary}</p>
      <dl class="ai-facts">
        <dt>Licence</dt>
        <dd class="ai-licence">
          {props.model.licence}, by {props.model.author}; converted to ONNX by {props.model.convertedBy}
        </dd>
        <dt>Source</dt>
        <dd class="ai-source">
          <a
            href={props.model.page}
            title={props.model.page}
            onClick={(event) => {
              event.preventDefault();
              void api.openLink(props.model.page).catch(notify);
            }}
          >
            huggingface.co/{props.model.repo}
          </a>{" "}
          <span class="ai-muted">at revision {short()}</span>
        </dd>
        <dt>Size</dt>
        <dd class="ai-size" data-bytes={props.model.totalSize}>
          {bytes.format(props.model.totalSize)} bytes ({formatSize(props.model.totalSize)}) in {props.model.files.length} files
          <details class="ai-files">
            <summary>Files and checksums</summary>
            <ul>
              <For each={props.model.files}>
                {(file) => (
                  <li>
                    <span class="ai-file-path">{file.path}</span>
                    <span class="ai-muted">{bytes.format(file.size)} bytes</span>
                    <code class="ai-sha" title="SHA-256">{file.sha256}</code>
                  </li>
                )}
              </For>
            </ul>
          </details>
        </dd>
        <dt>Runs on</dt>
        <dd>This PC's graphics card through Windows ML (DirectML), or its processor when there is no suitable card. Nothing leaves this PC.</dd>
      </dl>

      <Show when={downloading() && download()}>
        {(progress) => (
          <div class="ai-progress" role="progressbar" aria-valuemin={0} aria-valuemax={progress().total} aria-valuenow={progress().done}>
            <div class="ai-bar">
              <div class="ai-bar-fill" style={{ width: `${(progress().total > 0 ? progress().done / progress().total : 0) * 100}%` }} />
            </div>
            <div class="ai-progress-text">
              <Show when={progress().state === "downloading"} fallback={<span>{progress().message ?? "Testing the model…"}</span>}>
                <span>
                  {formatSize(progress().done)} of {formatSize(progress().total)}
                </span>
                <span class="ai-muted">{speed(progress().bytesPerSecond)}</span>
                <span class="ai-muted ai-progress-file">{progress().file}</span>
              </Show>
              <Show when={progress().state === "downloading"}>
                <button class="ai-button ai-cancel" onClick={cancel}>
                  Cancel
                </button>
              </Show>
            </div>
          </div>
        )}
      </Show>
      <Show when={!downloading() && download()?.message}>
        <p class="ai-message" classList={{ "ai-error": download()?.state === "failed" }} role={download()?.state === "failed" ? "alert" : undefined}>
          {download()?.message}
        </p>
      </Show>

      <div class="ai-actions">
        <Show
          when={props.model.installed}
          fallback={
            <Show when={!downloading()}>
              <button class="ai-button primary ai-download" onClick={start}>
                <Icon name="downloads" size={14} />
                {stopped() ? "Resume download" : "Download"} from Hugging Face ({formatSize(props.model.totalSize)})
              </button>
              <Show when={stopped()}>
                <button class="ai-button ai-discard" onClick={() => void discard()}>
                  Discard what arrived
                </button>
              </Show>
            </Show>
          }
        >
          <Show when={!props.model.active}>
            <button class="ai-button primary ai-use" onClick={() => void semanticApi.setActive(props.model.id).then(props.onChanged, notify)}>
              Use this model
            </button>
          </Show>
          <Show when={!props.model.selftest}>
            <button class="ai-button ai-run-selftest" disabled={testing() || downloading()} onClick={() => void runTest()}>
              {testing() || downloading() ? "Testing…" : "Run self-test"}
            </button>
          </Show>
          <button class="ai-button ai-delete" disabled={downloading()} onClick={() => void remove()}>
            Delete
          </button>
          <Show when={props.model.installedAt}>
            {(at) => <span class="ai-muted ai-installed">Downloaded {formatDate(at())}</span>}
          </Show>
        </Show>
      </div>

      <Show when={props.model.installed && props.model.selftest}>
        {(report) => <SelfTest report={report()} model={props.model.id} busy={testing() || downloading()} onRun={() => void runTest()} />}
      </Show>
    </section>
  );
}

export function AiSearchPage(props: LocationPageProps) {
  let page!: HTMLDivElement;
  onMount(() => {
    watchSemantic();
    props.focusRef(() => page.focus());
    if (props.active) takeFocusIfFree(page);
  });
  // Refresh (F5) and coming back to the tab ask again.
  createEffect(on(() => props.revision, () => void refreshSemantic(), { defer: true }));
  onCleanup(() => setLocationStatus(props.tabId, null));

  const state = semanticState;
  const active = () => state()?.models.find((model) => model.id === state()?.active) ?? null;
  createEffect(() => {
    const now = state();
    if (!now) return;
    const parts = [active() ? `${active()?.title} model in use` : "No model downloaded"];
    if (active()) parts.push(`${formatCount(now.indexing.files)} described`);
    setLocationStatus(props.tabId, parts);
  });

  const indexingLine = () => {
    const now = state();
    if (!now) return "";
    const index = now.indexing;
    switch (index.state) {
      case "off":
        return "Download a model to start.";
      case "loading":
        return "Getting the model ready…";
      case "paused":
        return "Paused.";
      case "waiting":
        return `Waiting: ${index.error ?? "busy"}`;
      case "failed":
        return index.error ?? "It stopped.";
      case "working":
        return `Describing: ${formatCount(index.done)} of ${formatCount(index.media)}`;
      default:
        return index.media > 0 ? "Up to date." : "No photos or videos in these folders yet.";
    }
  };

  const addFolder = async () => {
    const chosen = await archiveApi.pickFolder(places().home).catch(() => null);
    if (!chosen || !state()) return;
    await semanticApi.setScopes([...(state()?.scopes ?? []), chosen]).catch(notify);
  };
  const removeFolder = (path: string) => void semanticApi.setScopes((state()?.scopes ?? []).filter((scope) => scope !== path)).catch(notify);

  return (
    <div class="ai-page" ref={page} tabindex="-1">
      <header class="ai-intro">
        <h1>
          <Icon name="sparkle" size={18} /> AI search
        </h1>
        <p>
          Find photos and videos by describing them: turn on <b>Describe</b> in the search box (or start a search with <code>~</code>) and type what
          you are looking for, such as “dog on a sofa” or “receipt”. Right-click a photo for <b>Find similar</b>. The models run on this PC; no
          picture and no description leaves it.
        </p>
        <Show when={state()?.localOnly}>
          <p class="ai-note">This copy of Kestrel is a test instance: it downloads only from this computer.</p>
        </Show>
      </header>

      <section class="ai-status" aria-label="Describing" data-state={state()?.indexing.state ?? "off"}>
        <div class="ai-status-head">
          <h2>Describing photos and videos</h2>
          <Show when={state()?.active}>
            <button class="ai-button ai-pause" onClick={() => void semanticApi.setPaused(!state()?.paused).catch(notify)}>
              {state()?.paused ? "Resume" : "Pause"}
            </button>
          </Show>
        </div>
        <p class="ai-status-line">
          <span class="ai-indexing">{indexingLine()}</span>
          <Show when={active()}>
            <span class="ai-muted ai-described">
              {formatCount(state()?.indexing.files ?? 0)} described with the {active()?.title} model
              <Show when={(state()?.indexing.unreadable ?? 0) > 0}>, {formatCount(state()?.indexing.unreadable ?? 0)} couldn't be read</Show>
            </span>
          </Show>
        </p>
        <Show when={state()?.runtime.device}>
          {(device) => (
            <p class="ai-muted ai-device">
              Running on {device().gpu ? device().name : "the processor"}
              <Show when={device().note}>. {device().note}</Show>
            </p>
          )}
        </Show>
        <Show when={state()?.indexing.state === "working" && state()?.indexing.current}>
          <p class="ai-muted ai-current" title={state()?.indexing.current ?? ""}>
            {state()?.indexing.current}
          </p>
        </Show>
        <div class="ai-scopes">
          <span class="ai-scopes-label">The media library's folders{state()?.defaultScopes ? " (Windows' own)" : ""}:</span>
          <ul>
            <For each={state()?.scopes ?? []}>
              {(scope) => (
                <li>
                  <Icon name="folder" size={14} />
                  <span class="ai-scope-path">{scope}</span>
                  <button class="ai-scope-remove" title="Stop looking in this folder" aria-label={`Stop looking in ${scope}`} onClick={() => removeFolder(scope)}>
                    <Icon name="close" size={12} />
                  </button>
                </li>
              )}
            </For>
          </ul>
          <div class="ai-scope-actions">
            <button class="ai-button ai-add-folder" onClick={() => void addFolder()}>
              <Icon name="plus" size={12} /> Add folder…
            </button>
            <Show when={!state()?.defaultScopes}>
              <button class="ai-button" onClick={() => void semanticApi.setScopes(null).catch(notify)}>
                Use the default folders
              </button>
            </Show>
            <button class="ai-button ai-library-link" onClick={() => navigate(props.tabId, MEDIA_LIBRARY)}>
              Media library settings
            </button>
          </div>
          <p class="ai-muted ai-scope-note">
            Describing is a step of the media library's background reading, with its details, thumbnails and text: one Pause holds all of
            it. Files kept only in the cloud (OneDrive's online-only files) are skipped, never downloaded.
          </p>
        </div>
      </section>

      <h2 class="ai-models-title">Models</h2>
      <p class="ai-muted ai-models-note">Nothing is downloaded until you click Download. Each file is checked against its published SHA-256 before it is used.</p>
      <div class="ai-models">
        {/* By position: each state update replaces the objects, and a card must not remount. */}
        <Index each={state()?.models ?? []}>{(model) => <ModelCard model={model()} onChanged={() => void refreshSemantic()} />}</Index>
      </div>
    </div>
  );
}
