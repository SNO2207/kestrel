/**
 * Downloads triage: what arrived in the Downloads folder since the owner last looked, grouped
 * by kind (installers, archives, documents, images, videos, other), each item with the actions
 * that suit it. Open; Extract… (archives, through the archive feature's own dialog); Move to a
 * suggested folder (where similar files went before, the known folder for the kind, a project
 * visited lately) or to a folder the owner picks; and Recycle. Several items can be chosen and
 * moved or recycled at once. Moves and recycling are file operations in the undo history
 * (Ctrl+Z); nothing happens without a click.
 *
 * "Since the owner last looked": the page reads when that was as it opens, keeps showing what
 * came after it while it is open, and records the look, which clears the sidebar's badge.
 * "Everything" lists the whole folder instead. The page follows the folder as it changes.
 */
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { archiveStem, isArchiveFile, setExtractRequest } from "../archives";
import { api, iconUrl } from "../lib/api";
import { archiveApi } from "../lib/archivesApi";
import { formatCount, formatDate, formatSize } from "../lib/format";
import { setLocationStatus, takeFocusIfFree, type LocationPageProps } from "../lib/locations";
import { searchApi, type TriageItem, type TriageKind, type TriagePageData, type TriageSuggestion } from "../lib/searchApi";
import { displayName, showExtensions } from "../lib/views";
import { openLocation } from "../places";
import { notify } from "../state";
import type { MenuEntry } from "./ContextMenu";
import { SearchIcon, type SearchIconName } from "./SearchIcon";
import { showMenu } from "./ShortcutDialogs";

const KINDS: { kind: TriageKind; label: string; icon: SearchIconName }[] = [
  { kind: "installers", label: "Installers", icon: "install" },
  { kind: "archives", label: "Archives", icon: "archive" },
  { kind: "documents", label: "Documents", icon: "document" },
  { kind: "images", label: "Images", icon: "image" },
  { kind: "videos", label: "Videos", icon: "video" },
  { kind: "other", label: "Other", icon: "other" },
];

const WHY: Record<TriageSuggestion["why"], string> = {
  learned: "Where you moved files like this before",
  kind: "Your folder for this kind of file",
  project: "A project you opened lately",
};
/** The same, briefly, beside a place in the menu. */
const WHY_BRIEFLY: Record<TriageSuggestion["why"], string> = { learned: "Used before", kind: "Usual place", project: "Project" };

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** When something arrived, as people say it. */
function ago(ms: number, now = Date.now()): string {
  const passed = now - ms;
  if (passed < MINUTE) return "Just now";
  if (passed < HOUR) return `${Math.floor(passed / MINUTE)} min ago`;
  if (passed < 24 * HOUR) {
    const hours = Math.floor(passed / HOUR);
    return hours === 1 ? "1 hour ago" : `${hours} hours ago`;
  }
  return formatDate(ms);
}

const join = (folder: string, name: string) => (folder.endsWith("\\") ? `${folder}${name}` : `${folder}\\${name}`);
const plural = (count: number, one: string, many: string) => `${formatCount(count)} ${count === 1 ? one : many}`;

export function TriagePage(props: LocationPageProps) {
  let page!: HTMLDivElement;
  const [data, setData] = createSignal<TriagePageData | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [all, setAll] = createSignal(false);
  /** What counts as new on this visit: after the look before it. */
  const [since, setSince] = createSignal<number | null>(null);
  const [selected, setSelected] = createSignal<ReadonlySet<string>>(new Set());
  const [focused, setFocused] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);

  const items = () => data()?.items ?? [];
  /** Each item by its path: rows are drawn by path, so a reload keeps the rows that stay. */
  const byPath = createMemo(() => new Map(items().map((item) => [item.path, item])));
  const groups = createMemo(() =>
    KINDS.map((kind) => ({ ...kind, paths: items().filter((item) => item.kind === kind.kind).map((item) => item.path) })).filter(
      (group) => group.paths.length > 0,
    ),
  );
  /** Items in the order they are drawn, for the keyboard. */
  const ordered = createMemo(() => groups().flatMap((group) => group.paths.flatMap((path) => byPath().get(path) ?? [])));
  const chosen = () => items().filter((item) => selected().has(item.path));

  let loading = 0;
  const load = async () => {
    const ticket = ++loading;
    try {
      const next = await searchApi.triageList(since(), all());
      if (ticket !== loading) return;
      if (since() === null) setSince(next.since);
      setData(next);
      setFailure(null);
      const present = new Set(next.items.map((item) => item.path));
      setSelected((previous) => new Set([...previous].filter((path) => present.has(path))));
      if (focused() !== null && !present.has(focused()!)) setFocused(null);
      // While the owner looks at the page, nothing on it is new any more.
      if (props.active) void searchApi.triageMarkSeen().catch(() => {});
    } catch (error) {
      if (ticket === loading) setFailure(String(error));
    }
  };

  createEffect(on(() => props.revision, () => void load()));
  createEffect(on(all, () => void load(), { defer: true }));
  createEffect(on(() => props.active, (active) => active && since() !== null && void searchApi.triageMarkSeen().catch(() => {}), { defer: true }));

  onMount(() => {
    props.focusRef(() => page.focus());
    if (props.active) queueMicrotask(() => takeFocusIfFree(page));
    const stop = searchApi.onTriageFolder(() => void load());
    onCleanup(() => void stop.then((unlisten) => unlisten()));
  });

  createEffect(() => {
    const shown = items().length;
    const parts = [plural(shown, "item", "items")];
    if (selected().size > 0) parts.push(`${formatCount(selected().size)} selected`);
    setLocationStatus(props.tabId, parts);
  });
  onCleanup(() => setLocationStatus(props.tabId, null));

  // ---------- actions ----------

  const run = async (work: () => Promise<void>) => {
    if (busy()) return;
    setBusy(true);
    try {
      await work();
    } catch (error) {
      notify(error);
    } finally {
      setBusy(false);
      void load();
    }
  };

  const open = (item: TriageItem) => {
    if (item.dir) openLocation(item.path);
    else void api.openPath(item.path).catch(notify);
  };

  const extract = (item: TriageItem) => {
    const folder = data()?.folder ?? "";
    setExtractRequest({ tabId: props.tabId, source: item.path, items: null, name: item.name, destination: join(folder, archiveStem(item.name)) });
  };

  const move = (targets: TriageItem[], destination: string, label: string) =>
    run(async () => {
      const moved = await searchApi.triageMove(
        targets.map((item) => item.path),
        destination,
      );
      if (moved.names.length > 0) {
        const what = moved.names.length === 1 ? moved.names[0] : plural(moved.names.length, "item", "items");
        notify(`Moved ${what} to ${label}. Ctrl+Z puts ${moved.names.length === 1 ? "it" : "them"} back.`, "info");
      }
      setSelected(new Set<string>());
    });

  const chooseFolder = async (targets: TriageItem[]) => {
    const start = targets[0]?.suggestions[0]?.path ?? data()?.folder ?? "";
    const picked = await archiveApi.pickFolder(start).catch(() => null);
    if (picked) await move(targets, picked, picked.split("\\").filter(Boolean).pop() ?? picked);
  };

  const recycle = (targets: TriageItem[]) =>
    run(async () => {
      const done = await searchApi.triageRecycle(targets.map((item) => item.path));
      if (done) {
        const what = targets.length === 1 ? targets[0].name : plural(targets.length, "item", "items");
        notify(`Moved ${what} to the Recycle Bin. Ctrl+Z puts ${targets.length === 1 ? "it" : "them"} back.`, "info");
      }
      setSelected(new Set<string>());
    });

  /** Places the items could go, the ones suggested for most of them first. */
  const placesFor = (targets: TriageItem[]): TriageSuggestion[] => {
    const counted = new Map<string, { suggestion: TriageSuggestion; count: number; rank: number }>();
    for (const item of targets) {
      item.suggestions.forEach((suggestion, rank) => {
        const key = suggestion.path.toLowerCase();
        const known = counted.get(key);
        if (known) known.count += 1;
        else counted.set(key, { suggestion, count: 1, rank });
      });
    }
    return [...counted.values()].sort((a, b) => b.count - a.count || a.rank - b.rank).map((entry) => entry.suggestion);
  };

  const moveMenu = (button: HTMLElement, targets: TriageItem[]) => {
    const box = button.getBoundingClientRect();
    const places = placesFor(targets);
    const entries: MenuEntry[] = [
      ...places.map((place) => ({ label: place.label, hint: WHY_BRIEFLY[place.why], run: () => void move(targets, place.path, place.label) })),
      ...(places.length > 0 ? (["separator"] as MenuEntry[]) : []),
      { label: "Choose a folder…", run: () => void chooseFolder(targets) },
    ];
    showMenu(box.left, box.bottom + 4, entries);
  };

  // ---------- selection and keys ----------

  const toggle = (path: string, on?: boolean) =>
    setSelected((previous) => {
      const next = new Set(previous);
      if (on ?? !next.has(path)) next.add(path);
      else next.delete(path);
      return next;
    });

  const focusItem = (path: string) => {
    setFocused(path);
    page.querySelector<HTMLElement>(`[data-path="${CSS.escape(path)}"]`)?.scrollIntoView({ block: "nearest" });
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.target instanceof HTMLInputElement && event.target.type !== "checkbox") return;
    const list = ordered();
    const at = list.findIndex((item) => item.path === focused());
    const current = list[at];
    const handled = () => event.preventDefault();
    switch (event.key) {
      case "ArrowDown":
        handled();
        if (list.length > 0) focusItem(list[Math.min(list.length - 1, at + 1)].path);
        return;
      case "ArrowUp":
        handled();
        if (list.length > 0) focusItem(list[Math.max(0, at - 1)].path);
        return;
      case " ":
        if (current) {
          handled();
          toggle(current.path);
        }
        return;
      case "Enter":
        if (current) {
          handled();
          open(current);
        }
        return;
      case "Delete": {
        const targets = chosen().length > 0 ? chosen() : current ? [current] : [];
        if (targets.length > 0) {
          handled();
          void recycle(targets);
        }
        return;
      }
      case "Escape":
        setSelected(new Set<string>());
        return;
    }
    if (event.ctrlKey && event.key.toLowerCase() === "a") {
      handled();
      setSelected(new Set(list.map((item) => item.path)));
    }
  };

  // ---------- drawing ----------

  const subtitle = () => {
    const page = data();
    if (!page) return "";
    if (all()) return `${plural(page.total, "item", "items")} in ${page.folder}`;
    const when = ago(page.since);
    const lower = when === "Just now" ? "just now" : when;
    return page.new === 0 ? `Nothing new since you last looked (${lower}).` : `${plural(page.new, "item", "items")} since you last looked (${lower}).`;
  };

  const Row = (rowProps: { path: string }) => {
    // Kept while the row is drawn: a reload that drops the item drops the row first.
    let last: TriageItem | undefined;
    const item = (): TriageItem => (last = byPath().get(rowProps.path) ?? last)!;
    const first = () => item().suggestions[0];
    return (
      <div
        class="triage-row"
        classList={{ selected: selected().has(item().path), focused: focused() === item().path }}
        role="row"
        aria-selected={selected().has(item().path)}
        data-path={item().path}
        data-kind={item().kind}
        onMouseDown={(event) => {
          if (event.button === 0) setFocused(item().path);
        }}
        onDblClick={(event) => {
          if (!(event.target instanceof HTMLButtonElement) && !(event.target instanceof HTMLInputElement)) open(item());
        }}
      >
        <input
          type="checkbox"
          class="triage-check"
          aria-label={`Choose ${item().name}`}
          checked={selected().has(item().path)}
          onChange={(event) => toggle(item().path, event.currentTarget.checked)}
        />
        <img src={iconUrl({ dir: item().dir, ext: item().ext, path: item().path }, 32)} width="24" height="24" alt="" draggable={false} />
        <div class="triage-name" title={item().path}>
          <span class="triage-label">{displayName(item(), showExtensions())}</span>
          <span class="triage-meta">
            {ago(item().arrived)}
            <Show when={!item().dir}> · {formatSize(item().size)}</Show>
          </span>
        </div>
        <div class="triage-actions">
          <button class="triage-action" disabled={busy()} onClick={() => open(item())}>
            Open
          </button>
          <Show when={isArchiveFile(item())}>
            <button class="triage-action" disabled={busy()} onClick={() => extract(item())}>
              Extract…
            </button>
          </Show>
          <Show when={first()}>
            {(place) => (
              <button class="triage-action suggested" disabled={busy()} title={`${WHY[place().why]}: ${place().path}`} data-destination={place().path} onClick={() => void move([item()], place().path, place().label)}>
                <SearchIcon name="move" size={14} />
                <span>Move to {place().label}</span>
              </button>
            )}
          </Show>
          <button class="triage-action" disabled={busy()} aria-haspopup="menu" onClick={(event) => moveMenu(event.currentTarget, [item()])}>
            {first() ? "Other places…" : "Move to…"}
          </button>
          <button class="triage-action danger" disabled={busy()} title="Move to the Recycle Bin" aria-label={`Recycle ${item().name}`} onClick={() => void recycle([item()])}>
            <SearchIcon name="recycle" size={14} />
          </button>
        </div>
      </div>
    );
  };

  return (
    <div ref={page} class="triage-page" tabindex="0" aria-label="New downloads" onKeyDown={onKeyDown}>
      <header class="triage-head">
        <div class="triage-title">
          <h1>New downloads</h1>
          <p class="triage-sub">{subtitle()}</p>
        </div>
        <div class="triage-switch" role="group" aria-label="Show">
          <button classList={{ on: !all() }} aria-pressed={!all()} onClick={() => setAll(false)}>
            New
          </button>
          <button classList={{ on: all() }} aria-pressed={all()} onClick={() => setAll(true)}>
            Everything
          </button>
        </div>
        <Show when={data()}>
          {(page) => (
            <button class="triage-action" onClick={() => openLocation(page().folder)}>
              Open the Downloads folder
            </button>
          )}
        </Show>
      </header>

      <Show when={chosen().length > 0}>
        <div class="triage-bulk" role="toolbar" aria-label="Chosen downloads">
          <span class="triage-bulk-count">{plural(chosen().length, "item", "items")} chosen</span>
          <button class="triage-action" disabled={busy()} aria-haspopup="menu" onClick={(event) => moveMenu(event.currentTarget, chosen())}>
            <SearchIcon name="move" size={14} />
            <span>Move to…</span>
          </button>
          <button class="triage-action danger" disabled={busy()} onClick={() => void recycle(chosen())}>
            <SearchIcon name="recycle" size={14} />
            <span>Recycle</span>
          </button>
          <button class="triage-action" onClick={() => setSelected(new Set<string>())}>
            Clear
          </button>
        </div>
      </Show>

      <div class="triage-groups" role="grid" aria-multiselectable="true">
        <For each={groups()}>
          {(group) => (
            <section class="triage-group" aria-label={group.label} data-kind={group.kind}>
              <h2 class="triage-group-title">
                <SearchIcon name={group.icon} size={14} />
                <span>{group.label}</span>
                <span class="triage-group-count">{group.paths.length}</span>
                <button
                  class="triage-select-group"
                  onClick={() => {
                    const every = group.paths.every((path) => selected().has(path));
                    group.paths.forEach((path) => toggle(path, !every));
                  }}
                >
                  {group.paths.every((path) => selected().has(path)) ? "Clear" : "Choose all"}
                </button>
              </h2>
              <For each={group.paths}>{(path) => <Row path={path} />}</For>
            </section>
          )}
        </For>
        <Show when={data() && items().length === 0}>
          <div class="triage-empty">
            <Show when={failure()} fallback={<p>{all() ? "The Downloads folder is empty." : "Nothing new in Downloads since you last looked."}</p>}>
              <p>{failure()}</p>
            </Show>
            <Show when={!all()}>
              <button class="triage-action" onClick={() => setAll(true)}>
                Show everything in Downloads
              </button>
            </Show>
          </div>
        </Show>
        <Show when={!data() && failure()}>
          <div class="triage-empty">
            <p>{failure()}</p>
          </div>
        </Show>
      </div>
    </div>
  );
}
