/**
 * Folder Options' second-round options (parity2.ts), in the dialog's View section and a Privacy
 * section of their own, as Explorer's Options dialog has them.
 */
import { Show, createSignal } from "solid-js";

import { clearHistory, parity2Options, setParity2Options, type Parity2Options } from "../parity2";

function Check(props: { label: string; name: keyof Parity2Options; note?: string; invert?: boolean }) {
  const on = () => (props.invert ? !parity2Options()[props.name] : parity2Options()[props.name]);
  return (
    <label class="dialog-check options-check">
      <input
        type="checkbox"
        data-option={props.name}
        checked={on()}
        onChange={(event) => setParity2Options({ [props.name]: props.invert ? !event.currentTarget.checked : event.currentTarget.checked })}
      />
      <span>
        {props.label}
        <Show when={props.note}>
          <span class="options-note">{props.note}</span>
        </Show>
      </span>
    </label>
  );
}

/** The View section's options. */
export function ViewOptions2() {
  return (
    <>
      <Check label="Always show icons, never thumbnails" name="alwaysIcons" />
      <Check label="Decrease space between items (compact view)" name="compact" />
      <Check label="Hide empty drives" name="hideEmptyDrives" note="Card readers and disc drives with nothing in them." />
      <Check label="Show encrypted or compressed NTFS files in colour" name="ntfsColour" note="Encrypted files in green, compressed ones in blue." />
      <Check label="Use check boxes to select items" name="checkboxes" />
      <Check label="Show the navigation pane" name="navPane" />
      <Check label="Show libraries in the navigation pane" name="showLibraries" />
    </>
  );
}

/** The Privacy section: what Home and the sidebar show, and Clear. */
export function PrivacyOptions() {
  const [cleared, setCleared] = createSignal(false);
  return (
    <section class="options-privacy">
      <h3>Privacy</h3>
      <Check label="Show recently used files" name="showRecent" note="On Home." />
      <Check label="Show frequently used folders" name="showFrequent" note="On Home and in the sidebar." />
      <div class="options-clear">
        <span class="options-clear-text">
          Clear Kestrel's history
          <span class="options-note">
            Forgets the folders you visited, the files you opened through Kestrel and what the command palette remembers. Windows' own Recent items are left as they are, and
            Home leaves out those from before.
          </span>
        </span>
        <button
          class="dialog-button"
          onClick={() => {
            void clearHistory().then((done) => setCleared(done));
          }}
        >
          {cleared() ? "Cleared" : "Clear"}
        </button>
      </div>
    </section>
  );
}
