import { For, Match, Show, Switch, batch, createEffect, createSignal, on, onMount } from "solid-js";

import { iconUrl } from "../lib/api";
import { baseName } from "../lib/format";
import {
  PREFIXES,
  askPalette,
  highlightRuns,
  paletteProvider,
  splitPrefix,
  type OpenMode,
  type PaletteAnswer,
  type PaletteHit,
  type PaletteWindow,
} from "../lib/palette";
import { showHidden } from "../lib/views";
import { Icon, hasIcon, type IconName } from "./Icon";
import { ProjectIcon, hasProjectIcon, type ProjectIconName } from "./ProjectIcon";
import { ShortcutIcon, hasShortcutGlyph, type ShortcutGlyph } from "./ShortcutIcon";

/** Rows moved by Page Up and Page Down. */
const PAGE = 8;
/** How long a query may take before the palette says it is still searching. */
const SLOW_MS = 200;

interface Props {
  window: PaletteWindow;
  /** Carries out a choice; the palette has already closed or is closing. */
  onChoose: (hit: PaletteHit, mode: OpenMode) => void;
  /** Escape, or the palette otherwise dismissed without a choice. */
  onDismiss: () => void;
  /** Bumped by the owner to start afresh, as when the quick launcher is shown again. */
  generation?: number;
}

/** A file's extension, for its system icon: from its path, since a pin's title may be an alias. */
const extensionOf = (name: string) => {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
};

/** The mode a key or click with these modifiers asks for. */
const modeFor = (event: KeyboardEvent | MouseEvent): OpenMode =>
  event.ctrlKey ? "new-tab" : event.shiftKey ? "reveal" : "here";

/**
 * The command palette: an input and one ranked list of actions, folders, files and whatever
 * other providers offer. Used by the main window's Ctrl+K and by the quick launcher.
 */
export function CommandPalette(props: Props) {
  let input!: HTMLInputElement;
  let list!: HTMLDivElement;
  const [text, setText] = createSignal("");
  const [answer, setAnswer] = createSignal<PaletteAnswer | null>(null);
  const [selected, setSelected] = createSignal(0);
  const [slow, setSlow] = createSignal(false);
  /** The text the answer on screen is for. */
  const [answered, setAnswered] = createSignal<string | null>(null);
  let request = 0;
  /** Enter pressed before the answer for the latest text arrived: the mode to open with then. */
  let pendingEnter: OpenMode | null = null;

  const hits = () => answer()?.hits ?? [];
  /** The answer on screen is not yet the one for what was typed. */
  const busy = () => answered() !== text();

  const ask = async (value: string) => {
    const id = ++request;
    const timer = window.setTimeout(() => id === request && setSlow(true), SLOW_MS);
    try {
      const next = await askPalette(value, props.window, showHidden());
      if (id !== request) return;
      batch(() => {
        setAnswered(value);
        setAnswer(next);
        setSelected(0);
      });
      list?.scrollTo({ top: 0 });
      if (pendingEnter !== null) {
        const mode = pendingEnter;
        pendingEnter = null;
        choose(0, mode);
      }
    } catch (error) {
      // A newer query replaced this one in the backend, which is expected; anything else is
      // reported, and the palette stops waiting for this answer.
      if (id === request) {
        console.error(error);
        pendingEnter = null;
        setAnswered(value);
      }
    } finally {
      window.clearTimeout(timer);
      if (id === request) setSlow(false);
    }
  };

  createEffect(on(text, (value) => void ask(value)));

  // Starting afresh: empty input, focused, and a new answer.
  createEffect(
    on(
      () => props.generation,
      () => {
        pendingEnter = null;
        if (text() === "") void ask("");
        else setText("");
        queueMicrotask(() => input?.focus());
      },
      { defer: true },
    ),
  );

  onMount(() => queueMicrotask(() => input.focus()));

  // Keep the selected row in view.
  createEffect(() => {
    const index = selected();
    list?.querySelector(`[data-index="${index}"]`)?.scrollIntoView({ block: "nearest" });
  });

  const choose = (index: number, mode: OpenMode) => {
    const hit = hits()[index];
    if (hit) props.onChoose(hit, mode);
  };

  const complete = () => {
    const hit = hits()[selected()];
    const completion = hit && paletteProvider(hit.provider)?.complete?.(hit);
    if (!completion) return;
    setText(completion);
    input.value = completion;
    input.setSelectionRange(completion.length, completion.length);
  };

  const move = (step: number, wrap: boolean) => {
    const count = hits().length;
    if (count === 0) return;
    const next = selected() + step;
    setSelected(wrap ? (next + count) % count : Math.max(0, Math.min(count - 1, next)));
  };

  const onKeyDown = (event: KeyboardEvent) => {
    // Every key typed here is the palette's: the window's own shortcuts (Ctrl+T, Alt+P, Ctrl+G,
    // the pins' Ctrl+1…) must not act behind it. Text editing keys still do what they do.
    event.stopPropagation();
    const handled = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    switch (event.key) {
      case "ArrowDown":
        return handled(), move(1, true);
      case "ArrowUp":
        return handled(), move(-1, true);
      case "PageDown":
        return handled(), move(PAGE, false);
      case "PageUp":
        return handled(), move(-PAGE, false);
      case "Enter":
        handled();
        // The answer for what was just typed has not arrived: open its first hit once it does.
        if (busy()) pendingEnter = modeFor(event);
        else choose(selected(), modeFor(event));
        return;
      case "Tab":
        handled();
        if (!event.shiftKey && !busy()) complete();
        return;
      case "Escape":
        handled();
        return props.onDismiss();
    }
  };

  const emptyMessage = () => {
    const current = answer();
    if (!current) return "";
    const prefix = PREFIXES.find((entry) => entry.prefix === current.prefix);
    // A prefix with nothing typed after it lists everything its provider has: none yet.
    if (prefix && (!current.provided || splitPrefix(text()).rest.trim() === "")) return prefix.empty;
    const typed = text().trim();
    return typed ? `Nothing matches “${typed}”.` : "Nothing to show yet.";
  };

  const kind = (hit: PaletteHit) => paletteProvider(hit.provider)?.label(hit) ?? "";
  /** What Shift+Enter does to the selected item. */
  const revealLabel = () => {
    const hit = hits()[selected()];
    return (hit && paletteProvider(hit.provider)?.revealLabel) || "show in folder";
  };

  return (
    <div class="palette" classList={{ quick: props.window === "quick" }} role="dialog" aria-label="Command palette">
      <div class="palette-input">
        <Icon name="search" size={16} />
        <input
          ref={input}
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={hits().length > 0 ? `palette-item-${selected()}` : undefined}
          aria-label="Search actions, folders and files"
          placeholder="Search actions, folders and files"
          spellcheck={false}
          autocomplete="off"
          value={text()}
          onInput={(event) => {
            pendingEnter = null;
            setText(event.currentTarget.value);
          }}
          onKeyDown={onKeyDown}
        />
        <Show when={slow()}>
          <span class="palette-busy">Searching…</span>
        </Show>
      </div>
      <div ref={list} id="palette-list" class="palette-list" role="listbox" aria-label="Results" aria-busy={busy()}>
        <For each={hits()}>
          {(hit, index) => (
            <div
              id={`palette-item-${index()}`}
              class="palette-item"
              classList={{ active: selected() === index() }}
              role="option"
              aria-selected={selected() === index()}
              data-index={index()}
              data-provider={hit.provider}
              data-id={hit.id}
              title={hit.path ?? undefined}
              onMouseMove={() => selected() !== index() && setSelected(index())}
              onMouseDown={(event) => event.preventDefault()}
              onClick={(event) => choose(index(), modeFor(event))}
            >
              <span class="palette-icon">
                <Show when={hit.path} fallback={<PaletteGlyph icon={hit.icon} />}>
                  {(path) => (
                    <img
                      src={iconUrl({ dir: hit.dir ?? false, ext: extensionOf(baseName(path())), path: path() })}
                      width="16"
                      height="16"
                      alt=""
                      draggable={false}
                    />
                  )}
                </Show>
              </span>
              <span class="palette-text">
                <span class="palette-title">
                  <For each={highlightRuns(hit.title, hit.highlights)}>
                    {(run) => (run.matched ? <mark>{run.text}</mark> : run.text)}
                  </For>
                </span>
                <Show when={hit.subtitle}>
                  <span class="palette-subtitle">{hit.subtitle}</span>
                </Show>
              </span>
              <Show when={hit.hint} fallback={<span class="palette-kind">{kind(hit)}</span>}>
                <kbd class="palette-hint">{hit.hint}</kbd>
              </Show>
            </div>
          )}
        </For>
        <Show when={answer() && hits().length === 0 && !busy()}>
          <div class="palette-empty">{emptyMessage()}</div>
        </Show>
      </div>
      <div class="palette-footer">
        <span>
          <kbd>Enter</kbd> open
        </span>
        <span>
          <kbd>Ctrl+Enter</kbd> new tab
        </span>
        <span>
          <kbd>Shift+Enter</kbd> {revealLabel()}
        </span>
        <span>
          <kbd>Tab</kbd> complete
        </span>
        <span class="palette-prefixes" title="Type one of these first to narrow the list">
          <For each={PREFIXES}>
            {(entry) => (
              <span>
                <kbd>{entry.prefix}</kbd> {entry.label}
              </span>
            )}
          </For>
        </span>
      </div>
    </div>
  );
}

/**
 * The mark of an item that is not a file or folder: an app icon by name, a shortcut's glyph
 * (`shortcut:<name>`), a project glyph (`project:<name>`), a tag's colour (`tag:<colour>`), or
 * for an action without one, a bolt.
 */
function PaletteGlyph(props: { icon: string | null | undefined }) {
  const icon = () => props.icon ?? "";
  const after = (prefix: string) => (icon().startsWith(prefix) ? icon().slice(prefix.length) : null);
  return (
    <Switch fallback={<ActionGlyph />}>
      <Match when={after("tag:")}>{(color) => <span class="tag-dot palette-tag" data-color={color()} />}</Match>
      <Match when={after("shortcut:")}>
        {(name) => (hasShortcutGlyph(name()) ? <ShortcutIcon name={name() as ShortcutGlyph} /> : <ActionGlyph />)}
      </Match>
      <Match when={after("project:")}>
        {(name) => (hasProjectIcon(name()) ? <ProjectIcon name={name() as ProjectIconName} /> : <ActionGlyph />)}
      </Match>
      <Match when={hasIcon(icon())}>
        <Icon name={icon() as IconName} />
      </Match>
    </Switch>
  );
}

/** The mark of an action without an icon of its own: a bolt. */
function ActionGlyph() {
  return (
    <svg class="icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M9 2.5 4 9h4l-1 4.5L12 7H8l1-4.5Z" />
    </svg>
  );
}
