import { Show, createSignal, onCleanup, onMount } from "solid-js";

import { parityApi } from "../lib/parityApi";
import { closeNewShortcut, createShortcut, shortcutFolder } from "../parity";

/**
 * New > Shortcut, as Explorer's Create Shortcut wizard asks it, on one page: what the shortcut
 * points at, and what to call it (the target's name unless changed).
 */
export function NewShortcutDialog(props: { onClose: () => void }) {
  return (
    <Show when={shortcutFolder()}>
      {(at) => (
        <Dialog
          folder={at().folder}
          onClose={() => {
            closeNewShortcut();
            props.onClose();
          }}
        />
      )}
    </Show>
  );
}

function Dialog(props: { folder: string; onClose: () => void }) {
  let target!: HTMLInputElement;
  const [location, setLocation] = createSignal("");
  const [name, setName] = createSignal("");
  /** The user typed a name of their own, which the target no longer changes. */
  const [named, setNamed] = createSignal(false);
  const [found, setFound] = createSignal<boolean | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  let asked = 0;

  onMount(() => {
    target.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        props.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const suggest = async (text: string) => {
    const request = ++asked;
    if (!text.trim()) {
      setFound(null);
      if (!named()) setName("");
      return;
    }
    const [suggested, exists] = await parityApi.shortcutSuggestion(text).catch(() => ["", false] as [string, boolean]);
    if (request !== asked) return;
    setFound(exists);
    if (!named()) setName(suggested);
  };

  const create = async () => {
    if (busy() || !location().trim()) return;
    setBusy(true);
    setError(null);
    try {
      await createShortcut(location(), name());
      props.onClose();
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <form
        class="dialog shortcut-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="new-shortcut-title"
        onSubmit={(event) => {
          event.preventDefault();
          void create();
        }}
      >
        <h2 id="new-shortcut-title" class="dialog-title">
          New shortcut
        </h2>
        <p class="shortcut-where">In {props.folder}</p>
        <div class="shortcut-fields">
          <label class="dialog-field">
            Type the location of the item
            <input
              ref={target}
              value={location()}
              spellcheck={false}
              placeholder="C:\Users\you\Documents\report.docx"
              onInput={(event) => {
                setLocation(event.currentTarget.value);
                void suggest(event.currentTarget.value);
              }}
            />
          </label>
          <Show when={found() === false}>
            <p class="shortcut-problem">Nothing is at that location yet.</p>
          </Show>
          <label class="dialog-field">
            Name of the shortcut
            <input
              value={name()}
              spellcheck={false}
              onInput={(event) => {
                setNamed(event.currentTarget.value.trim() !== "");
                setName(event.currentTarget.value);
              }}
            />
          </label>
          <Show when={error()}>
            <p class="shortcut-problem" role="alert">
              {error()}
            </p>
          </Show>
        </div>
        <div class="dialog-footer">
          <span class="options-spacer" />
          <button type="button" class="dialog-button" onClick={() => props.onClose()}>
            Cancel
          </button>
          <button type="submit" class="dialog-button primary" disabled={busy() || !location().trim() || found() === false}>
            Create
          </button>
        </div>
      </form>
    </div>
  );
}
