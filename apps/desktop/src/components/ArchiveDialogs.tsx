import { For, Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";

import {
  KIND_EXTENSIONS,
  archiveNameFor,
  cancelJob,
  compress,
  compressRequest,
  extract,
  extractRequest,
  jobs,
  passwordRequest,
  preferences,
  setCompressRequest,
  setExtractRequest,
  setPreferences,
  startArchives,
  type CompressRequest,
  type ExtractRequest,
  type JobView,
  type PasswordRequest,
} from "../archives";
import { archiveApi, type ArchiveKind, type ArchiveLevel } from "../lib/archivesApi";
import { baseName, formatSize } from "../lib/format";

const KINDS: { value: ArchiveKind; label: string }[] = [
  { value: "zip", label: "ZIP (.zip)" },
  { value: "7z", label: "7z (.7z)" },
  { value: "tar-gz", label: "TAR.GZ (.tar.gz)" },
];

const LEVELS: { value: ArchiveLevel; label: string }[] = [
  { value: "store", label: "None, store only" },
  { value: "fast", label: "Fast" },
  { value: "normal", label: "Normal" },
  { value: "maximum", label: "Maximum" },
];

/** The archive dialogs: Extract, Compress to and the password prompt. */
export function ArchiveDialogs(props: { focusList: () => void }) {
  onMount(() => {
    const stop = startArchives();
    onCleanup(() => void stop.then((unlisten) => unlisten()));
  });
  const closeExtract = () => {
    setExtractRequest(null);
    props.focusList();
  };
  const closeCompress = () => {
    setCompressRequest(null);
    props.focusList();
  };
  return (
    <>
      <Show when={extractRequest()}>{(request) => <ExtractDialog request={request()} onClose={closeExtract} />}</Show>
      <Show when={compressRequest()}>{(request) => <CompressDialog request={request()} onClose={closeCompress} />}</Show>
      <Show when={passwordRequest()}>{(request) => <PasswordDialog request={request()} />}</Show>
    </>
  );
}

/**
 * A modal dialog: keys stay inside it (Tab cycles, Escape cancels, Enter confirms), and a
 * click on the backdrop cancels.
 */
function Dialog(props: { title: string; id: string; class?: string; onCancel: () => void; onConfirm: () => void; children: JSX.Element }) {
  let panel!: HTMLDivElement;
  const onKeyDown = (event: KeyboardEvent) => {
    // The app's own shortcuts must not act on the list behind the dialog.
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      props.onCancel();
    } else if (event.key === "Enter" && !(event.target instanceof HTMLButtonElement) && !(event.target instanceof HTMLSelectElement)) {
      event.preventDefault();
      props.onConfirm();
    } else if (event.key === "Tab") {
      const focusable = [...panel.querySelectorAll<HTMLElement>("input, select, button:not(:disabled)")];
      const [first, last] = [focusable[0], focusable[focusable.length - 1]];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
  };
  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onCancel();
      }}
    >
      <div
        ref={panel}
        class={`dialog archive-dialog ${props.class ?? ""}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={props.id}
        onKeyDown={onKeyDown}
      >
        <h2 id={props.id} class="dialog-title archive-title">
          {props.title}
        </h2>
        {props.children}
      </div>
    </div>
  );
}

/** A folder field with Windows' folder picker beside it. */
function FolderField(props: { id: string; label: string; value: string; onChange: (value: string) => void; ref?: (input: HTMLInputElement) => void }) {
  const browse = async () => {
    const chosen = await archiveApi.pickFolder(props.value).catch(() => null);
    if (chosen) props.onChange(chosen);
  };
  return (
    <div class="dialog-field">
      <label for={props.id}>{props.label}</label>
      <div class="archive-path-row">
        <input
          id={props.id}
          ref={props.ref}
          spellcheck={false}
          autocomplete="off"
          value={props.value}
          onInput={(event) => props.onChange(event.currentTarget.value)}
        />
        <button class="dialog-button" type="button" onClick={() => void browse()}>
          Browse…
        </button>
      </div>
    </div>
  );
}

function ExtractDialog(props: { request: ExtractRequest; onClose: () => void }) {
  let input: HTMLInputElement | undefined;
  const [destination, setDestination] = createSignal(props.request.destination);
  const [show, setShow] = createSignal(preferences().show_extracted);
  const items = props.request.items;
  const title = items === null ? `Extract ${props.request.name}` : items.length === 1 ? `Extract ${baseName(items[0])}` : `Extract ${items.length} items`;

  const confirm = () => {
    const folder = destination().trim();
    if (!folder) return;
    setPreferences({ ...preferences(), show_extracted: show() });
    const request = { ...props.request, destination: folder };
    props.onClose();
    void extract(request, show());
  };

  onMount(() => {
    input?.focus();
    input?.select();
  });

  return (
    <Dialog title={title} id="archive-extract-title" class="archive-extract" onCancel={props.onClose} onConfirm={confirm}>
      <div class="archive-body">
        <p class="archive-intro">
          {items === null ? `Choose where the files in ${props.request.name} go.` : `Choose where the items from ${props.request.name} go.`} A
          folder that doesn't exist yet is made.
        </p>
        <FolderField
          id="archive-destination"
          label="Files will be extracted to this folder"
          value={destination()}
          onChange={setDestination}
          ref={(element) => (input = element)}
        />
        <label class="dialog-check">
          <input type="checkbox" checked={show()} onChange={(event) => setShow(event.currentTarget.checked)} />
          Show extracted files when complete
        </label>
      </div>
      <div class="dialog-footer archive-footer">
        <button class="dialog-button" onClick={props.onClose}>
          Cancel
        </button>
        <button class="dialog-button primary" disabled={!destination().trim()} onClick={confirm}>
          Extract
        </button>
      </div>
    </Dialog>
  );
}

function CompressDialog(props: { request: CompressRequest; onClose: () => void }) {
  let input: HTMLInputElement | undefined;
  const [kind, setKind] = createSignal<ArchiveKind>(preferences().format);
  const [level, setLevel] = createSignal<ArchiveLevel>(preferences().level);
  const [name, setName] = createSignal(archiveNameFor(props.request.first, kind()));
  const [folder, setFolder] = createSignal(props.request.folder);
  const title = props.request.count === 1 ? `Compress ${props.request.first.name}` : `Compress ${props.request.count} items`;

  /** A new format changes the name's extension to match. */
  const changeKind = (next: ArchiveKind) => {
    const current = name();
    const old = KIND_EXTENSIONS[kind()];
    const stem = current.toLowerCase().endsWith(old) ? current.slice(0, current.length - old.length) : current;
    setKind(next);
    setName(`${stem}${KIND_EXTENSIONS[next]}`);
  };

  const confirm = () => {
    const chosen = name().trim();
    const where = folder().trim();
    if (!chosen || !where) return;
    setPreferences({ ...preferences(), format: kind(), level: level() });
    const request = props.request;
    props.onClose();
    void compress(request, chosen, kind(), level(), where, false);
  };

  onMount(() => {
    input?.focus();
    // The name without its extension, as when renaming.
    const ext = KIND_EXTENSIONS[kind()];
    input?.setSelectionRange(0, Math.max(0, name().length - ext.length));
  });

  return (
    <Dialog title={title} id="archive-compress-title" class="archive-compress" onCancel={props.onClose} onConfirm={confirm}>
      <div class="archive-body">
        <label class="dialog-field">
          <span>Archive name</span>
          <input ref={(element) => (input = element)} spellcheck={false} autocomplete="off" value={name()} onInput={(event) => setName(event.currentTarget.value)} />
        </label>
        <div class="archive-options">
          <label class="dialog-field">
            <span>Format</span>
            <select value={kind()} onChange={(event) => changeKind(event.currentTarget.value as ArchiveKind)}>
              <For each={KINDS}>{(option) => <option value={option.value}>{option.label}</option>}</For>
            </select>
          </label>
          <label class="dialog-field">
            <span>Compression</span>
            <select value={level()} onChange={(event) => setLevel(event.currentTarget.value as ArchiveLevel)}>
              <For each={LEVELS}>{(option) => <option value={option.value}>{option.label}</option>}</For>
            </select>
          </label>
        </div>
        <FolderField id="archive-folder" label="Save in" value={folder()} onChange={setFolder} />
      </div>
      <div class="dialog-footer archive-footer">
        <button class="dialog-button" onClick={props.onClose}>
          Cancel
        </button>
        <button class="dialog-button primary" disabled={!name().trim() || !folder().trim()} onClick={confirm}>
          Compress
        </button>
      </div>
    </Dialog>
  );
}

function PasswordDialog(props: { request: PasswordRequest }) {
  let input: HTMLInputElement | undefined;
  const [value, setValue] = createSignal("");
  const [visible, setVisible] = createSignal(false);
  const cancel = () => props.request.answer(null);
  const confirm = () => {
    if (value()) props.request.answer(value());
  };
  onMount(() => input?.focus());
  return (
    <Dialog title="Enter the password" id="archive-password-title" class="archive-password" onCancel={cancel} onConfirm={confirm}>
      <div class="archive-body">
        <p class="archive-intro" classList={{ "archive-wrong": props.request.wrong }} role={props.request.wrong ? "alert" : undefined}>
          {props.request.wrong
            ? `That password doesn't open ${props.request.name}. Try again.`
            : `${props.request.name} is protected by a password.`}
        </p>
        <label class="dialog-field">
          <span>Password</span>
          <input
            ref={(element) => (input = element)}
            type={visible() ? "text" : "password"}
            spellcheck={false}
            autocomplete="off"
            value={value()}
            onInput={(event) => setValue(event.currentTarget.value)}
          />
        </label>
        <label class="dialog-check">
          <input type="checkbox" checked={visible()} onChange={(event) => setVisible(event.currentTarget.checked)} />
          Show password
        </label>
        <p class="archive-note">Kestrel uses it for this one job and doesn't keep it.</p>
      </div>
      <div class="dialog-footer archive-footer">
        <button class="dialog-button" onClick={cancel}>
          Cancel
        </button>
        <button class="dialog-button primary" disabled={!value()} onClick={confirm}>
          Continue
        </button>
      </div>
    </Dialog>
  );
}

/** What a job is doing, in a line under its bar. */
function jobDetail(job: JobView): string {
  switch (job.phase) {
    case "starting":
      return "Starting…";
    case "reading":
      return "Reading the archive…";
    case "placing":
      return "Moving the files into place…";
    default: {
      const amount = job.total > 0 ? `${formatSize(job.done)} of ${formatSize(job.total)}` : formatSize(job.done);
      return job.item ? `${amount} · ${job.item}` : amount;
    }
  }
}

/** Running extractions and compressions, at the bottom right of the window, each with Cancel. */
export function ArchiveJobs() {
  const shown = () => jobs.filter((job) => job.shown);
  return (
    <Show when={shown().length > 0}>
      <div class="archive-jobs" role="status" aria-live="polite">
        <For each={shown()}>
          {(job) => {
            const measured = () => (job.phase === "extracting" || job.phase === "compressing") && job.total > 0;
            const share = () => Math.min(100, Math.round((job.done / Math.max(1, job.total)) * 100));
            return (
              <div class="archive-job" data-phase={job.phase}>
                <div class="archive-job-head">
                  <span class="archive-job-title" title={job.title}>
                    {job.title}
                  </span>
                  <button class="archive-job-cancel" disabled={job.cancelling} onClick={() => cancelJob(job.id)}>
                    {job.cancelling ? "Cancelling…" : "Cancel"}
                  </button>
                </div>
                <div
                  class="archive-bar"
                  classList={{ indeterminate: !measured() }}
                  role="progressbar"
                  aria-label={job.title}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={measured() ? share() : undefined}
                >
                  <span style={measured() ? { width: `${share()}%` } : undefined} />
                </div>
                <div class="archive-job-detail">{jobDetail(job)}</div>
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}
