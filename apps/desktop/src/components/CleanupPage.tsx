import { Show, createEffect, createMemo, on, onCleanup, onMount } from "solid-js";

import { cleanupOpen, cleanupScope, cleanupTab, closeCleanup, setCleanupScope, setCleanupTab } from "../cleanup";
import { baseName } from "../lib/format";
import { activeId, activeTab, isSearch } from "../state";
import { CleanupIcon } from "./CleanupParts";
import { CleanupPanel } from "./CleanupPanel";
import { DuplicatesPanel } from "./DuplicatesPanel";

/**
 * A page of its own for the cleanup and duplicates panels, until the disk usage hub shows them
 * as tabs. It covers the file list while open; Escape, or going to another folder or tab,
 * closes it.
 */
export function CleanupPage() {
  let root: HTMLElement | undefined;

  // The folder "This folder" means: the one on screen when the page opened.
  const here = createMemo(() => {
    const tab = activeTab();
    return tab && !isSearch(tab) ? tab.path : (tab?.path ?? null);
  });

  const where = createMemo(() => `${activeId()}|${activeTab()?.path ?? ""}|${activeTab()?.query ?? ""}`);
  createEffect(on(where, () => cleanupOpen() && closeCleanup(), { defer: true }));
  createEffect(
    on(cleanupOpen, (open) => {
      if (open) {
        setCleanupScope(null);
        queueMicrotask(() => root?.focus());
      }
    }),
  );

  /** Keys pressed on the page stay on it: the folder underneath must not act on them. */
  const guardKeys = (event: KeyboardEvent) => {
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) return;
    const key = event.key.toLowerCase();
    if (key === "escape") {
      // A dialog of the page's closes itself first.
      if (event.target instanceof Element && event.target.closest(".dialog-backdrop")) return;
      event.preventDefault();
      event.stopPropagation();
      closeCleanup();
      return;
    }
    if (event.ctrlKey && (key === "t" || key === "w" || key === "tab" || key === "z" || key === "y")) return;
    if (event.ctrlKey && !event.shiftKey && !event.altKey && (key === "g" || /^Digit[1-9]$/.test(event.code))) return;
    event.stopPropagation();
  };

  onMount(() => {
    // Registered after Solid's own delegated listener, so the page's handlers run first.
    document.addEventListener("keydown", guardKeys);
    onCleanup(() => document.removeEventListener("keydown", guardKeys));
  });

  return (
    <Show when={cleanupOpen()}>
      <section ref={root} class="cleanup-page" aria-label="Cleanup" tabindex="-1">
        <header class="cleanup-head">
          <span class="cleanup-head-icon">
            <CleanupIcon name={cleanupTab() === "duplicates" ? "copies" : "sweep"} size={18} />
          </span>
          <h1>Cleanup</h1>
          <div class="segmented cleanup-tabs" role="tablist" aria-label="Cleanup">
            <button role="tab" aria-pressed={cleanupTab() === "recommendations"} aria-selected={cleanupTab() === "recommendations"} onClick={() => setCleanupTab("recommendations")}>
              <CleanupIcon name="sweep" size={14} />
              <span>Recommendations</span>
            </button>
            <button role="tab" aria-pressed={cleanupTab() === "duplicates"} aria-selected={cleanupTab() === "duplicates"} onClick={() => setCleanupTab("duplicates")}>
              <CleanupIcon name="copies" size={14} />
              <span>Duplicates</span>
            </button>
          </div>
          <span class="cleanup-spacer" />
          <label class="cleanup-scope">
            <span>Look in</span>
            <select
              aria-label="Look in"
              value={cleanupScope() ?? ""}
              onChange={(event) => setCleanupScope(event.currentTarget.value || null)}
            >
              <option value="">{cleanupTab() === "duplicates" ? "Your user folder" : "Every drive"}</option>
              <Show when={here()}>{(folder) => <option value={folder()}>This folder: {baseName(folder()) || folder()}</option>}</Show>
            </select>
          </label>
          <button class="cleanup-close" aria-label="Close cleanup" title="Close (Esc)" onClick={closeCleanup}>
            <CleanupIcon name="close" size={14} />
          </button>
        </header>
        <div class="cleanup-body">
          <Show when={cleanupTab() === "recommendations"} fallback={<DuplicatesPanel root={cleanupScope()} />}>
            <CleanupPanel root={cleanupScope()} />
          </Show>
        </div>
      </section>
    </Show>
  );
}
