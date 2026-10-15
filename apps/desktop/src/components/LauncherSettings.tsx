import { Show, createSignal, onCleanup, onMount } from "solid-js";

import { paletteApi, type HotkeyStatus, type LauncherSettings } from "../lib/paletteApi";

const [shown, setShown] = createSignal(false);

/** Opens the quick launcher settings dialog. */
export const openLauncherSettings = () => setShown(true);

/** Keys that only modify another key; a shortcut needs one more. */
const MODIFIER_CODES = new Set(["ControlLeft", "ControlRight", "AltLeft", "AltRight", "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight"]);

/** A key's name in a shortcut, from its code: `KeyK` is `K`, `ArrowUp` is `Up`. */
function keyName(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (code.startsWith("Arrow")) return code.slice(5);
  return code;
}

/** The shortcut a key press makes, or null while only modifiers are held. */
export function shortcutFrom(event: Pick<KeyboardEvent, "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): string | null {
  if (MODIFIER_CODES.has(event.code) || event.code === "") return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  if (event.metaKey) parts.push("Win");
  parts.push(keyName(event.code));
  return parts.join("+");
}

/**
 * The quick launcher settings dialog: its system-wide shortcut, and the options for running
 * in the background. Opened from the palette, or from the tray icon's menu.
 */
export function LauncherSettingsDialog(props: { onClose: () => void }) {
  onMount(() => {
    const stop = paletteApi.onOpenSettings(openLauncherSettings);
    onCleanup(() => void stop.then((unlisten) => unlisten()));
  });
  const close = () => {
    setShown(false);
    props.onClose();
  };
  return (
    <Show when={shown()}>
      <Dialog onClose={close} />
    </Show>
  );
}

function statusText(status: HotkeyStatus): string {
  switch (status.state) {
    case "registered":
      return `Press ${status.registered} anywhere in Windows to open the quick launcher.`;
    case "off":
      return "No shortcut. The quick launcher opens from the tray icon.";
    default:
      return status.message ?? "";
  }
}

function Dialog(props: { onClose: () => void }) {
  let panel!: HTMLDivElement;
  const [settings, setSettings] = createSignal<LauncherSettings | null>(null);
  /** A new shortcut pressed in the field and not yet applied. */
  const [draft, setDraft] = createSignal<string | null>(null);
  const [recording, setRecording] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const status = () => settings()?.hotkey ?? null;
  const shownShortcut = () => draft() ?? status()?.shortcut ?? null;

  onMount(async () => {
    panel.focus();
    try {
      setSettings(await paletteApi.settings());
    } catch (problem) {
      setError(String(problem));
    }
  });

  const update = (change: Partial<LauncherSettings>) => {
    const current = settings();
    if (current) setSettings({ ...current, ...change });
  };

  const applyShortcut = async (shortcut: string | null) => {
    setBusy(true);
    setError(null);
    try {
      const next = await paletteApi.setHotkey(shortcut);
      update({ hotkey: next });
      setDraft(null);
    } catch (problem) {
      setError(String(problem));
    } finally {
      setBusy(false);
    }
  };

  const setKeep = async (keep: boolean) => {
    setError(null);
    try {
      await paletteApi.setKeepInTray(keep);
      update({ keep_in_tray: keep });
    } catch (problem) {
      setError(String(problem));
    }
  };

  const setStartup = async (start: boolean) => {
    setError(null);
    try {
      update({ start_with_windows: await paletteApi.setStartWithWindows(start) });
    } catch (problem) {
      setError(String(problem));
    }
  };

  /** The field records the next key combination pressed in it. */
  const onFieldKey = (event: KeyboardEvent) => {
    if (event.key === "Tab") return;
    event.preventDefault();
    event.stopPropagation();
    if (!recording()) {
      if (event.key === "Enter" || event.key === " ") setRecording(true);
      else if (event.key === "Escape") props.onClose();
      return;
    }
    if (event.key === "Escape" && !event.ctrlKey && !event.altKey && !event.shiftKey) return void setRecording(false);
    const shortcut = shortcutFrom(event);
    if (shortcut) {
      setDraft(shortcut);
      setRecording(false);
    }
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // The app's own shortcuts must not act on the window behind the dialog.
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      props.onClose();
    }
  };

  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div
        ref={panel}
        class="dialog launcher-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="launcher-dialog-title"
        tabindex="-1"
        onKeyDown={onKeyDown}
      >
        <h2 id="launcher-dialog-title" class="dialog-title">
          Quick launcher
        </h2>
        <div class="launcher-body">
          <p class="launcher-intro">
            The quick launcher is the command palette in a small window of its own, over any app. It opens folders in
            Kestrel and files in their own apps.
          </p>

          <div class="launcher-row">
            <span class="launcher-label" id="launcher-shortcut-label">
              Shortcut
            </span>
            <button
              class="shortcut-field"
              classList={{ recording: recording(), changed: draft() !== null }}
              data-records-keys=""
              aria-labelledby="launcher-shortcut-label"
              aria-describedby="launcher-shortcut-status"
              onClick={() => setRecording(true)}
              onBlur={() => setRecording(false)}
              onKeyDown={onFieldKey}
            >
              <Show when={!recording()} fallback={<span class="shortcut-prompt">Press the keys…</span>}>
                {shownShortcut() ?? "None"}
              </Show>
            </button>
            <button
              class="dialog-button primary"
              disabled={busy() || draft() === null}
              onClick={() => void applyShortcut(draft())}
            >
              Use this shortcut
            </button>
          </div>
          <div class="launcher-actions">
            <button
              class="link-button"
              disabled={busy() || (status()?.shortcut === settings()?.default_hotkey && draft() === null)}
              onClick={() => void applyShortcut(settings()?.default_hotkey ?? null)}
            >
              Use {settings()?.default_hotkey ?? "the default"}
            </button>
            <button class="link-button" disabled={busy() || (status()?.shortcut === null && draft() === null)} onClick={() => void applyShortcut(null)}>
              No shortcut
            </button>
          </div>
          <Show when={status()}>
            {(current) => (
              <p
                id="launcher-shortcut-status"
                class="launcher-status"
                classList={{ ok: current().state === "registered", problem: current().state === "failed" }}
                data-state={current().state}
                role="status"
              >
                {statusText(current())}
              </p>
            )}
          </Show>

          <label class="dialog-check launcher-check">
            <input type="checkbox" checked={settings()?.keep_in_tray ?? false} disabled={!settings()} onChange={(event) => void setKeep(event.currentTarget.checked)} />
            <span>
              Keep running in the tray when the window is closed
              <small>The index stays current and the shortcut keeps working. Quit from the tray icon.</small>
            </span>
          </label>
          <label class="dialog-check launcher-check">
            <input
              type="checkbox"
              checked={settings()?.start_with_windows ?? false}
              disabled={!settings()}
              onChange={(event) => void setStartup(event.currentTarget.checked)}
            />
            <span>
              Start Kestrel when you sign in to Windows
              <small>With the tray option on, it starts in the tray.</small>
            </span>
          </label>

          <Show when={error()}>
            <p class="launcher-status problem" role="alert">
              {error()}
            </p>
          </Show>
        </div>
        <div class="dialog-footer">
          <button class="dialog-button" onClick={props.onClose}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
