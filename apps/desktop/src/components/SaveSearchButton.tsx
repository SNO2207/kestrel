import { Show } from "solid-js";

import { saveSearch } from "../shortcuts";
import { activeTab } from "../state";
import { askName } from "./ShortcutDialogs";
import { ShortcutIcon } from "./ShortcutIcon";

/** Saves the search being typed, with where it searches, to the sidebar. */
export function SaveSearchButton() {
  const save = async () => {
    const tab = activeTab();
    if (!tab || tab.query.trim() === "") return;
    const answer = await askName({ title: "Save search", label: "Name", initial: tab.query.trim(), action: "Save" });
    if (answer) await saveSearch(tab.id, answer.name);
  };
  return (
    <Show when={(activeTab()?.query.trim() ?? "") !== ""}>
      <button class="save-search" title="Save this search" aria-label="Save this search" onClick={() => void save()}>
        <ShortcutIcon name="save" size={14} />
      </button>
    </Show>
  );
}
