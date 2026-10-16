import { Show, createSignal, onCleanup, onMount } from "solid-js";

import { paletteProvider, registerPaletteProvider, rememberChoice, type OpenMode, type PaletteHit } from "../lib/palette";
import { actionsProvider } from "../lib/paletteActions";
import { featureProviders } from "../lib/paletteFeatures";
import { paletteApi } from "../lib/paletteApi";
import { filesProvider } from "../lib/paletteFiles";
import { notify } from "../state";
import { CommandPalette } from "./CommandPalette";
import type { ListCommand } from "./FileList";
import { openLauncherSettings } from "./LauncherSettings";

const [open, setOpen] = createSignal(false);

/** Whether the main window's palette is showing. */
export const paletteOpen = open;

const isPaletteKey = (event: KeyboardEvent) =>
  event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey && event.key.toLowerCase() === "k";

/**
 * The main window's command palette: Ctrl+K opens it over the window, and Ctrl+K or Escape
 * closes it. Also where the main window carries out what the quick launcher hands it, and
 * where its palette providers are registered.
 */
export function PaletteOverlay(props: { runCommand: (command: ListCommand) => void; focusList: () => void }) {
  const run = async (hit: PaletteHit, mode: OpenMode) => {
    const provider = paletteProvider(hit.provider);
    if (!provider) return;
    try {
      await provider.run(hit, mode);
    } catch (error) {
      notify(error);
    }
    // The keyboard goes back to the files, unless what ran took it somewhere else.
    queueMicrotask(() => {
      if (!document.activeElement || document.activeElement === document.body) props.focusList();
    });
  };

  const close = () => {
    setOpen(false);
    props.focusList();
  };

  onMount(() => {
    const host = { runCommand: props.runCommand, focusList: props.focusList, openSettings: openLauncherSettings };
    const removers = [actionsProvider(host), filesProvider, ...featureProviders()].map(registerPaletteProvider);
    // Captured, so the shortcut works from any field, menu or dialog.
    const onKeyDown = (event: KeyboardEvent) => {
      // A field recording a shortcut (the launcher settings) takes Ctrl+K as keys to record.
      const recording = event.target instanceof HTMLElement && event.target.closest("[data-records-keys]");
      if (!isPaletteKey(event) || recording) return;
      event.preventDefault();
      event.stopPropagation();
      if (open()) close();
      else setOpen(true);
    };
    window.addEventListener("keydown", onKeyDown, true);
    const stopRuns = paletteApi.onRun((request) => void run(request.hit, request.mode));
    onCleanup(() => {
      window.removeEventListener("keydown", onKeyDown, true);
      removers.forEach((remove) => remove());
      void stopRuns.then((stop) => stop());
    });
  });

  return (
    <Show when={open()}>
      <div
        class="palette-backdrop"
        onMouseDown={(event) => {
          if (event.target === event.currentTarget) close();
        }}
      >
        <CommandPalette
          window="main"
          onChoose={(hit, mode) => {
            setOpen(false);
            rememberChoice(hit);
            void run(hit, mode);
          }}
          onDismiss={close}
        />
      </div>
    </Show>
  );
}
