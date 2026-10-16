/**
 * The address bar while an address is being typed, with completion as in Explorer: the
 * folders in the folder typed so far (names starting with what follows the last separator
 * first, then names containing it), special locations by name (This PC, Recycle Bin), and
 * `shell:` names. Up and Down choose a suggestion and put it in the box, Tab completes the
 * chosen one (or the first) and lists what is inside it, Enter goes to what the box says,
 * Escape closes the list and then stops editing. Addresses are understood as Explorer
 * understands them; see `address.rs`.
 */
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";

import { allLocations, locationOf, locationTitle } from "../lib/locations";
import { placesApi } from "../lib/placesApi";
import { showHidden } from "../lib/views";
import { openAddress } from "../places";
import { tabs } from "../state";
import { Icon, type IconName } from "./Icon";

/** How long typing must pause before suggestions are asked for. */
const SUGGEST_DELAY_MS = 40;
/** Suggestions shown at most. */
const SHOWN = 12;

interface Choice {
  /** What the box says when it is chosen. */
  path: string;
  name: string;
  detail: string | null;
  icon: IconName;
  /** A folder, whose contents Tab lists after it. */
  folder: boolean;
}

const lower = (text: string) => text.trim().toLowerCase();

/** Special locations whose names start with, then contain, what was typed. */
function locationChoices(typed: string): Choice[] {
  const text = lower(typed);
  if (text.length === 0) return [];
  const scored: { choice: Choice; rank: number }[] = [];
  for (const location of allLocations()) {
    const title = locationTitle(location.address) ?? location.address;
    const names = [title, ...(location.aliases ?? [])].map(lower);
    const rank = names.some((name) => name.startsWith(text)) ? 0 : names.some((name) => name.includes(text)) ? 1 : -1;
    if (rank >= 0) scored.push({ rank, choice: { path: title, name: title, detail: null, icon: location.icon, folder: false } });
  }
  return scored.sort((a, b) => a.rank - b.rank).map((entry) => entry.choice);
}

export function AddressInput(props: {
  tabId: number | undefined;
  /** What the box starts with: the folder's path, or the location's name. */
  initial: string;
  ref: (input: HTMLInputElement) => void;
  /** Editing is over; `went` when the tab is going somewhere. */
  onDone: (went: boolean) => void;
}) {
  let input!: HTMLInputElement;
  const [choices, setChoices] = createSignal<Choice[]>([]);
  const [chosen, setChosen] = createSignal(-1);
  const [open, setOpen] = createSignal(false);
  /** What was typed, which arrowing through the list replaces in the box for a moment. */
  let typed = props.initial;
  let request = 0;
  let timer: number | undefined;

  const base = () => {
    const tab = tabs.find((candidate) => candidate.id === props.tabId);
    return tab && !locationOf(tab.path) ? tab.path : null;
  };

  const suggest = (text: string) => {
    clearTimeout(timer);
    const asked = ++request;
    timer = window.setTimeout(async () => {
      const locations = locationChoices(text);
      const found = text.trim() ? await placesApi.addressSuggest(text, base(), showHidden()).catch(() => null) : null;
      if (asked !== request) return;
      // Folders by their full paths, as Explorer lists them; shell names with where they lead.
      const folders: Choice[] = (found?.items ?? []).map((item) => {
        const shell = item.name.startsWith("shell:");
        return { path: item.path, name: shell ? item.name : item.path, detail: shell ? item.hint : null, icon: "folder", folder: true };
      });
      const all = [...locations, ...folders].slice(0, SHOWN);
      setChoices(all);
      setChosen(-1);
      setOpen(all.length > 0);
    }, SUGGEST_DELAY_MS);
  };

  const close = () => {
    clearTimeout(timer);
    request++;
    setOpen(false);
    setChosen(-1);
  };

  const go = (text: string) => {
    close();
    props.onDone(true);
    if (props.tabId !== undefined) void openAddress(props.tabId, text);
  };

  /** Puts a suggestion in the box, as arrowing to it does. */
  const show = (index: number) => {
    setChosen(index);
    input.value = index >= 0 ? choices()[index].path : typed;
    input.setSelectionRange(input.value.length, input.value.length);
  };

  /** Completes the chosen suggestion, or the first, and lists what is inside it. */
  const complete = (): boolean => {
    const list = choices();
    if (!open() || list.length === 0) return false;
    const choice = list[Math.max(0, chosen())];
    const text = choice.folder && !choice.path.endsWith("\\") ? `${choice.path}\\` : choice.path;
    input.value = text;
    typed = text;
    input.setSelectionRange(text.length, text.length);
    if (choice.folder) suggest(text);
    else close();
    return true;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const list = choices();
    switch (event.key) {
      case "ArrowDown":
        if (!open() || list.length === 0) return;
        event.preventDefault();
        return show(chosen() + 1 >= list.length ? -1 : chosen() + 1);
      case "ArrowUp":
        if (!open() || list.length === 0) return;
        event.preventDefault();
        return show(chosen() <= -1 ? list.length - 1 : chosen() - 1);
      case "Tab":
        if (!event.shiftKey && complete()) event.preventDefault();
        return;
      case "Enter":
        event.preventDefault();
        return go(input.value);
      case "Escape":
        event.preventDefault();
        if (open()) {
          input.value = typed;
          return close();
        }
        close();
        return props.onDone(false);
    }
  };

  onMount(() => {
    props.ref(input);
    // The box exists only while the address is edited, so it takes the keyboard as it appears:
    // the toolbar's own focus call runs before this mounts (Alt+D, Ctrl+L, F4, a click).
    input.focus();
    input.select();
    onCleanup(() => clearTimeout(timer));
  });

  return (
    <>
      <input
        ref={input}
        class="address-input"
        aria-label="Address"
        aria-autocomplete="list"
        aria-expanded={open()}
        aria-controls="address-suggestions"
        aria-activedescendant={chosen() >= 0 ? `address-suggestion-${chosen()}` : undefined}
        spellcheck={false}
        value={props.initial}
        onInput={(event) => {
          typed = event.currentTarget.value;
          suggest(typed);
        }}
        onBlur={() => {
          close();
          props.onDone(false);
        }}
        onKeyDown={onKeyDown}
      />
      <Show when={open()}>
        <div
          id="address-suggestions"
          class="address-suggest"
          role="listbox"
          aria-label="Suggestions"
          // Keeps the box focused, so a click on a suggestion is not a blur first.
          onMouseDown={(event) => event.preventDefault()}
        >
          <For each={choices()}>
            {(choice, index) => (
              <div
                id={`address-suggestion-${index()}`}
                class="address-suggestion"
                classList={{ chosen: chosen() === index() }}
                role="option"
                aria-selected={chosen() === index()}
                onMouseEnter={() => setChosen(index())}
                onClick={() => go(choice.path)}
              >
                <Icon name={choice.icon} />
                <span class="address-suggestion-name">{choice.name}</span>
                <Show when={choice.detail && choice.detail !== choice.name}>
                  <span class="address-suggestion-detail">{choice.detail}</span>
                </Show>
              </div>
            )}
          </For>
        </div>
      </Show>
    </>
  );
}
