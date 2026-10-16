import { Show, onCleanup, onMount } from "solid-js";

import { folderOptions, setFolderOptions, type StartLocation } from "../lib/folderOptions";
import { setOption, showExtensions, showHidden } from "../lib/views";
import { optionsOpen, setOptionsOpen } from "../parity";
import { refresh, tabs } from "../state";

/** Lists every tab's folder again, for options the engine applies (what is listed). */
const relist = () => tabs.forEach((tab) => refresh(tab.id));

/**
 * Folder Options, as Explorer's Options dialog has them: where Kestrel opens, how items open,
 * the full path in the tabs, hidden and protected files, file extensions and sync status. Each
 * takes effect at once.
 */
export function FolderOptionsDialog(props: { onClose: () => void }) {
  return (
    <Show when={optionsOpen()}>
      <Dialog
        onClose={() => {
          setOptionsOpen(false);
          props.onClose();
        }}
      />
    </Show>
  );
}

function Dialog(props: { onClose: () => void }) {
  let dialog!: HTMLDivElement;
  onMount(() => {
    dialog.querySelector<HTMLElement>("select, input")?.focus();
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

  const check = (label: string, checked: () => boolean, change: (on: boolean) => void, note?: string) => (
    <label class="dialog-check options-check">
      <input type="checkbox" checked={checked()} onChange={(event) => change(event.currentTarget.checked)} />
      <span>
        {label}
        <Show when={note}>
          <span class="options-note">{note}</span>
        </Show>
      </span>
    </label>
  );

  return (
    <div
      class="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) props.onClose();
      }}
    >
      <div ref={dialog} class="dialog options-dialog" role="dialog" aria-modal="true" aria-labelledby="options-title">
        <h2 id="options-title" class="dialog-title">
          Folder options
        </h2>
        <div class="options-body">
          <section>
            <h3>General</h3>
            <label class="dialog-field options-field">
              Open Kestrel to
              <select
                value={folderOptions().start}
                onChange={(event) => setFolderOptions({ start: event.currentTarget.value as StartLocation })}
              >
                <option value="profile">Your user folder</option>
                <option value="home">Home</option>
              </select>
            </label>
            <div class="options-radios" role="radiogroup" aria-label="Click items as follows">
              <label class="dialog-check options-check">
                <input
                  type="radio"
                  name="click"
                  checked={folderOptions().singleClick}
                  onChange={() => setFolderOptions({ singleClick: true })}
                />
                <span>Single-click to open an item (Ctrl or Shift and click to select)</span>
              </label>
              <label class="dialog-check options-check">
                <input
                  type="radio"
                  name="click"
                  checked={!folderOptions().singleClick}
                  onChange={() => setFolderOptions({ singleClick: false })}
                />
                <span>Double-click to open an item (single-click to select)</span>
              </label>
            </div>
          </section>
          <section>
            <h3>View</h3>
            {check("Display the full path in the title bar", () => folderOptions().fullPath, (on) => setFolderOptions({ fullPath: on }), "Tabs show their folder's whole path.")}
            {check("Show hidden files, folders and drives", showHidden, (on) => {
              setOption("hidden", on);
              relist();
            })}
            {check("Hide protected operating system files (recommended)", () => folderOptions().hideProtected, (on) => {
              setFolderOptions({ hideProtected: on });
              relist();
            }, "Files marked both hidden and system stay out even while hidden files are shown.")}
            {check("Hide extensions for known file types", () => !showExtensions(), (on) => setOption("extensions", !on))}
            {check("Show sync status", () => folderOptions().syncStatus, (on) => setFolderOptions({ syncStatus: on }), "In OneDrive and other synced folders.")}
          </section>
        </div>
        <div class="dialog-footer">
          <span class="options-spacer" />
          <button class="dialog-button primary" onClick={() => props.onClose()}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
