import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { createStore } from "solid-js/store";

import { api, type RenameCase, type RenamePart, type RenamePreview, type RenameRules, type Row } from "../lib/api";
import { isSearch, notify, refresh, selectWhenListed, selectedRows, tabs } from "../state";

/** Items one batch rename takes at most. */
const MAX_ITEMS = 10_000;
/** Preview rows drawn at once; the summary still counts every item. */
const MAX_SHOWN = 400;
/** Typing pauses this long before the preview is recomputed. */
const PREVIEW_DELAY_MS = 60;

const BLANK: RenameRules = {
  find: "",
  replace: "",
  regex: false,
  matchCase: false,
  prefix: "",
  suffix: "",
  numbering: "",
  start: 1,
  case: "keep",
  part: "name",
};

const CASES: { value: RenameCase; label: string }[] = [
  { value: "keep", label: "Keep as it is" },
  { value: "lower", label: "lowercase" },
  { value: "upper", label: "UPPERCASE" },
  { value: "title", label: "Title Case" },
];

const PARTS: { value: RenamePart; label: string }[] = [
  { value: "name", label: "Name" },
  { value: "extension", label: "Extension" },
  { value: "both", label: "Name and extension" },
];

const [target, setTarget] = createSignal<{ tabId: number; rows: Row[] } | null>(null);

/** Opens the batch rename dialog for the tab's selected items, if there are several. */
export async function openBatchRename(tabId: number) {
  const count = tabs.find((tab) => tab.id === tabId)?.selected ?? 0;
  if (count > MAX_ITEMS) {
    notify(`Batch rename takes up to ${MAX_ITEMS.toLocaleString()} items at a time; ${count.toLocaleString()} are selected.`);
    return;
  }
  const rows = await selectedRows(tabId, MAX_ITEMS);
  if (rows.length > 1) setTarget({ tabId, rows });
}

/** The batch rename dialog, shown while one is open. */
export function BatchRenameDialog(props: { onClose: () => void }) {
  const close = () => {
    setTarget(null);
    props.onClose();
  };
  return (
    <Show when={target()}>{(open) => <Dialog tabId={open().tabId} rows={open().rows} onClose={close} />}</Show>
  );
}

function Dialog(props: { tabId: number; rows: Row[]; onClose: () => void }) {
  let panel!: HTMLDivElement;
  let findInput!: HTMLInputElement;
  const [rules, setRules] = createStore<RenameRules>({ ...BLANK });
  const [previews, setPreviews] = createSignal<RenamePreview[]>([]);
  /** The preview matches the rules as they are now. */
  const [fresh, setFresh] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const paths = createMemo(() => props.rows.map((row) => row.path));

  let request = 0;
  createEffect(() => {
    const snapshot: RenameRules = { ...rules };
    const items = paths();
    const id = ++request;
    setFresh(false);
    const timer = window.setTimeout(async () => {
      try {
        const result = await api.previewRenames(items, snapshot);
        if (id !== request) return;
        setPreviews(result);
        setError(null);
        setFresh(true);
      } catch (problem) {
        if (id === request) setError(String(problem));
      }
    }, PREVIEW_DELAY_MS);
    onCleanup(() => clearTimeout(timer));
  });

  const changed = () => previews().filter((preview) => preview.new !== preview.old).length;
  const problems = () => previews().filter((preview) => preview.problem !== null).length;
  const shown = () => previews().slice(0, MAX_SHOWN);
  const canApply = () => fresh() && !busy() && !error() && changed() > 0 && problems() === 0;

  const summary = () => {
    const failure = error();
    if (failure) return failure;
    const bad = problems();
    if (bad > 0) return bad === 1 ? "1 new name can't be used." : `${bad} new names can't be used.`;
    if (changed() === 0) return "No names change yet.";
    return `${changed()} of ${previews().length} names change.`;
  };

  const apply = async () => {
    if (!canApply()) return;
    // Read now: once the dialog closes, its props can no longer be read.
    const tabId = props.tabId;
    setBusy(true);
    try {
      const names = await api.batchRename(paths(), { ...rules });
      const tab = tabs.find((candidate) => candidate.id === tabId);
      if (tab && !isSearch(tab)) selectWhenListed(tabId, names);
      refresh(tabId);
      notify(names.length === 1 ? "Renamed 1 item" : `Renamed ${names.length} items`, "info");
      props.onClose();
    } catch (problem) {
      setError(String(problem));
      setBusy(false);
    }
  };

  /** Keeps Tab inside the dialog, so the keyboard can't reach the list behind it. */
  const trapTab = (event: KeyboardEvent) => {
    const focusable = [...panel.querySelectorAll<HTMLElement>("input, select, button:not(:disabled)")];
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // The app's own shortcuts must not act on the list behind the dialog.
    event.stopPropagation();
    if (event.key === "Tab") {
      trapTab(event);
    } else if (event.key === "Escape") {
      event.preventDefault();
      props.onClose();
    } else if (event.key === "Enter" && !(event.target instanceof HTMLButtonElement)) {
      event.preventDefault();
      void apply();
    }
  };

  onMount(() => findInput.focus());

  const text = (field: "find" | "replace" | "prefix" | "suffix" | "numbering", label: string, placeholder = "") => (
    <label class="dialog-field">
      <span>{label}</span>
      <input
        ref={field === "find" ? (input) => (findInput = input) : undefined}
        spellcheck={false}
        placeholder={placeholder}
        value={rules[field]}
        onInput={(event) => setRules(field, event.currentTarget.value)}
      />
    </label>
  );

  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div
        ref={panel}
        class="dialog rename-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="rename-dialog-title"
        onKeyDown={onKeyDown}
      >
        <h2 id="rename-dialog-title" class="dialog-title">
          Rename {props.rows.length} items
        </h2>

        <div class="rename-rules">
          {text("find", "Find")}
          {text("replace", "Replace with")}
          <div class="rename-options">
            <label class="dialog-check">
              <input type="checkbox" checked={rules.regex} onChange={(event) => setRules("regex", event.currentTarget.checked)} />
              Regular expression
            </label>
            <label class="dialog-check">
              <input
                type="checkbox"
                checked={rules.matchCase}
                onChange={(event) => setRules("matchCase", event.currentTarget.checked)}
              />
              Match case
            </label>
          </div>
          {text("prefix", "Prefix")}
          {text("suffix", "Suffix")}
          {text("numbering", "Numbering", "Photo ###")}
          <label class="dialog-field">
            <span>Start at</span>
            <input
              type="number"
              min="0"
              step="1"
              value={rules.start}
              onInput={(event) => {
                const value = Math.floor(Number(event.currentTarget.value));
                setRules("start", Number.isFinite(value) ? Math.min(Math.max(0, value), 4_294_967_295) : 0);
              }}
            />
          </label>
          <label class="dialog-field">
            <span>Case</span>
            <select value={rules.case} onChange={(event) => setRules("case", event.currentTarget.value as RenameCase)}>
              <For each={CASES}>{(option) => <option value={option.value}>{option.label}</option>}</For>
            </select>
          </label>
          <label class="dialog-field">
            <span>Apply to</span>
            <select value={rules.part} onChange={(event) => setRules("part", event.currentTarget.value as RenamePart)}>
              <For each={PARTS}>{(option) => <option value={option.value}>{option.label}</option>}</For>
            </select>
          </label>
        </div>

        <div class="rename-preview" role="table" aria-label="Preview">
          <div class="rename-preview-head" role="row">
            <span role="columnheader">Current name</span>
            <span role="columnheader">New name</span>
          </div>
          <div class="rename-preview-body">
            <For each={shown()}>
              {(preview) => (
                <div
                  class="rename-preview-row"
                  classList={{ unchanged: preview.new === preview.old, problem: preview.problem !== null }}
                  role="row"
                >
                  <span class="rename-old" role="cell" title={preview.old}>
                    {preview.old}
                  </span>
                  <span class="rename-new" role="cell" title={preview.problem ?? preview.new}>
                    <span class="rename-new-name">{preview.new}</span>
                    <Show when={preview.problem}>{(problem) => <span class="rename-problem">{problem()}</span>}</Show>
                  </span>
                </div>
              )}
            </For>
            <Show when={previews().length > MAX_SHOWN}>
              <div class="rename-more">and {previews().length - MAX_SHOWN} more</div>
            </Show>
          </div>
        </div>

        <div class="dialog-footer">
          <span class="rename-summary" classList={{ bad: problems() > 0 || error() !== null }} role="status">
            {summary()}
          </span>
          <button class="dialog-button" onClick={() => props.onClose()}>
            Cancel
          </button>
          <button class="dialog-button primary" disabled={!canApply()} onClick={() => void apply()}>
            Rename
          </button>
        </div>
      </div>
    </div>
  );
}
