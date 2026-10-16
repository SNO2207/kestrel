import { Show, createSignal, onCleanup, onMount } from "solid-js";

import { paletteProvider, registerPaletteProvider, rememberChoice, type OpenMode, type PaletteHit } from "../lib/palette";
import { actionsProvider } from "../lib/paletteActions";
import { featureProviders } from "../lib/paletteFeatures";
import { paletteApi } from "../lib/paletteApi";
import { filesProvider } from "../lib/paletteFiles";
import { locationsProvider } from "../lib/paletteLocations";
// This PC and the Recycle Bin, which the palette lists, are registered there.
import "../placesSetup";
import { setOption, showExtensions, showHidden } from "../lib/views";
import { setPaneOpen } from "../previewState";
import { CommandPalette } from "./CommandPalette";

/**
 * Keeps the options the launcher depends on in step with the main window, which changes them
 * in the storage both windows share: whether hidden items are found, and what the toggle
 * actions say.
 */
function followMainWindow(event: StorageEvent) {
  try {
    if (event.key === "kestrel.view-options" && event.newValue) {
      const options: unknown = JSON.parse(event.newValue);
      if (options && typeof options === "object") {
        const { hidden, extensions } = options as { hidden?: unknown; extensions?: unknown };
        if (typeof hidden === "boolean" && hidden !== showHidden()) setOption("hidden", hidden);
        if (typeof extensions === "boolean" && extensions !== showExtensions()) setOption("extensions", extensions);
      }
    }
    if (event.key === "preview-open") setPaneOpen(event.newValue === "1");
  } catch {
    // A value this window cannot read leaves its own as it was.
  }
}

/**
 * The quick launcher's whole window (quick.html): the palette, summoned from anywhere in
 * Windows by the shortcut or the tray icon (launcher.rs). A file opens from here; anything
 * else is handed to the main window, which comes forward to carry it out. Either way the
 * launcher hides, as it does on Escape or when it loses the focus.
 */
export function QuickLauncher() {
  /** Bumped each time the window is shown, so the palette starts afresh. */
  const [generation, setGeneration] = createSignal(0);
  const [failure, setFailure] = createSignal<string | null>(null);

  onMount(() => {
    const removers = [actionsProvider(), filesProvider, locationsProvider, ...featureProviders()].map(registerPaletteProvider);
    const stopShown = paletteApi.onShown(() => {
      setFailure(null);
      setGeneration((count) => count + 1);
    });
    window.addEventListener("storage", followMainWindow);
    onCleanup(() => {
      removers.forEach((remove) => remove());
      void stopShown.then((stop) => stop());
      window.removeEventListener("storage", followMainWindow);
    });
  });

  const choose = async (hit: PaletteHit, mode: OpenMode) => {
    rememberChoice(hit);
    const provider = paletteProvider(hit.provider);
    try {
      if (provider?.runsAnywhere?.(hit, mode)) {
        await provider.run(hit, mode);
      } else {
        // The window used last, which Ctrl+N's windows may be rather than `main`.
        const shown = await paletteApi.showMainWindow();
        await paletteApi.runInMain({ hit, mode }, shown ?? "main");
      }
      setFailure(null);
      await paletteApi.hideQuickWindow();
    } catch (error) {
      // Stay open, so the reason can be read.
      setFailure(String(error));
    }
  };

  return (
    <div class="quick-launcher">
      <CommandPalette
        window="quick"
        generation={generation()}
        onChoose={(hit, mode) => void choose(hit, mode)}
        onDismiss={() => void paletteApi.hideQuickWindow()}
      />
      <Show when={failure()}>
        <div class="palette-failure" role="alert">
          {failure()}
        </div>
      </Show>
    </div>
  );
}
