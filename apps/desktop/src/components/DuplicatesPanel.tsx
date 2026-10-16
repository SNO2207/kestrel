import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";

import { api } from "../lib/api";
import { cleanupApi, type DupeProgress, type DupeReport, type RecycleReport, type SimilarReport } from "../lib/cleanupApi";
import { baseName, formatCount, formatDate, formatSize, parentPath } from "../lib/format";
import {
  CleanupDialog,
  CleanupIcon,
  DryRunDialog,
  type Outcome,
  OutcomeBanner,
  PathLabel,
  PathList,
  SkippedList,
  outcomeOf,
  toRecycleBin,
} from "./CleanupParts";

const MIN_SIZES: { bytes: number; label: string }[] = [
  { bytes: 100 * 1024, label: "100 KB" },
  { bytes: 1 << 20, label: "1 MB" },
  { bytes: 10 << 20, label: "10 MB" },
  { bytes: 100 << 20, label: "100 MB" },
];

const lower = (path: string) => path.toLowerCase();
const within = (path: string, folder: string) => {
  const f = lower(folder).replace(/\\+$/, "");
  const p = lower(path);
  return p === f || p.startsWith(`${f}\\`);
};
const copies = (n: number) => (n === 1 ? "1 copy" : `${formatCount(n)} copies`);
const photos = (n: number) => (n === 1 ? "1 photo" : `${formatCount(n)} photos`);
const files = (n: number) => (n === 1 ? "1 file" : `${formatCount(n)} files`);

/** What choosing the copy to keep needs of a group, exact or similar. */
type Choice = { id: string; files: { path: string; modified: number }[] };

function describeProgress(progress: DupeProgress | null): { text: string; share: number } {
  if (!progress) return { text: "Finding files of the same size…", share: 0 };
  const share = progress.total > 0 ? progress.done / progress.total : 0;
  switch (progress.phase) {
    case "checking":
      return { text: `Checking ${formatCount(progress.done)} of ${formatCount(progress.total)} files of the same size…`, share };
    case "partial":
      return { text: `Comparing the start and end of ${formatCount(progress.done)} of ${formatCount(progress.total)} files…`, share };
    case "full":
      return { text: `Comparing whole files: ${formatSize(progress.done)} of ${formatSize(progress.total)}…`, share };
  }
}

type Dialog = { kind: "recycle" } | { kind: "dry-run"; report: RecycleReport } | { kind: "skipped" };

/**
 * Exact duplicate files within `root` (or, with `null`, your user folder without AppData),
 * grouped with the space their extra copies take. Choose the copy to keep in each group (or
 * let "Keep newest", "Keep oldest" or "Keep copies in" choose), tick the groups to clean up,
 * and the other copies go to the Recycle Bin after a confirmation that lists them.
 *
 * Below them come similar photos (near duplicates: a burst, the same shot again, an edited or
 * resized copy), found by comparing AI search's descriptions of the photos the media library
 * has read. They are groups of their own, never ticked for the owner, not even by "Select all".
 */
export function DuplicatesPanel(props: { root: string | null }) {
  const [roots, setRoots] = createSignal<string[]>([]);
  const [exclude, setExclude] = createSignal<string[]>([]);
  const [minSize, setMinSize] = createSignal(1 << 20);
  const [adding, setAdding] = createSignal("");
  const [searching, setSearching] = createSignal(false);
  const [progress, setProgress] = createSignal<DupeProgress | null>(null);
  const [report, setReport] = createSignal<DupeReport | null>(null);
  const [similar, setSimilar] = createSignal<SimilarReport | null>(null);
  const [cancelled, setCancelled] = createSignal(false);
  /** The copy kept in each group, by group id. */
  const [keep, setKeep] = createSignal<Map<string, string>>(new Map());
  /** Groups ticked for cleaning up. */
  const [included, setIncluded] = createSignal<Set<string>>(new Set());
  const [keepFolder, setKeepFolder] = createSignal("");
  const [dialog, setDialog] = createSignal<Dialog | null>(null);
  const [outcome, setOutcome] = createSignal<Outcome | null>(null);
  const [busy, setBusy] = createSignal(false);

  onMount(() => {
    const stop = cleanupApi.onDupesProgress((next) => setProgress(next));
    onCleanup(() => {
      void stop.then((unlisten) => unlisten());
      if (searching()) void cleanupApi.dupesCancel();
    });
  });

  // Where to look: the panel's root, or the defaults (the user folder, without AppData).
  createEffect(
    on(
      () => props.root,
      (root) => {
        void cleanupApi.dupesDefaults().then((defaults) => {
          setRoots(root ? [root] : defaults.roots);
          setExclude(defaults.exclude);
          setMinSize(defaults.min_size);
        });
        setReport(null);
        setSimilar(null);
      },
    ),
  );

  const groups = () => report()?.groups ?? [];
  const similarGroups = () => similar()?.groups ?? [];
  const kept = (group: Choice) => keep().get(group.id) ?? group.files[0].path;
  const removing = <F extends Choice["files"][number]>(group: { id: string; files: F[] }) =>
    group.files.filter((file) => lower(file.path) !== lower(kept(group)));
  const chosen = createMemo(() => groups().filter((group) => included().has(group.id)));
  const chosenSimilar = createMemo(() => similarGroups().filter((group) => included().has(group.id)));
  const chosenCount = () => chosen().length + chosenSimilar().length;
  /** Every file to move, exact copies first, with its bytes on disk. */
  const chosenFiles = () => [
    ...chosen().flatMap((group) => removing(group).map((file) => ({ path: file.path, bytes: group.bytes_each }))),
    ...chosenSimilar().flatMap((group) => removing(group).map((file) => ({ path: file.path, bytes: file.bytes }))),
  ];
  const chosenBytes = () => chosenFiles().reduce((n, file) => n + file.bytes, 0);
  /** What is chosen, in words: copies, photos (similar ones are not copies), or both. */
  const moving = (n: number) => (chosenSimilar().length === 0 ? copies(n) : chosen().length === 0 ? photos(n) : files(n));
  /** Folders holding copies, most first, for "Keep copies in". */
  const folders = createMemo(() => {
    const counts = new Map<string, { folder: string; n: number }>();
    for (const group of [...groups(), ...similarGroups()]) {
      for (const file of group.files) {
        const folder = parentPath(file.path) ?? file.path;
        const entry = counts.get(lower(folder)) ?? { folder, n: 0 };
        entry.n += 1;
        counts.set(lower(folder), entry);
      }
    }
    return [...counts.values()].sort((a, b) => b.n - a.n || a.folder.localeCompare(b.folder)).slice(0, 40);
  });

  /** Chooses the copy each group keeps; groups `pick` has no answer for keep theirs. */
  function keepBy(pick: (group: Choice) => string | undefined) {
    setKeep((current) => {
      const next = new Map(current);
      for (const group of [...groups(), ...similarGroups()]) {
        const path = pick(group);
        if (path) next.set(group.id, path);
      }
      return next;
    });
  }
  const newest = (group: Choice) => [...group.files].sort((a, b) => b.modified - a.modified)[0]?.path;
  const oldest = (group: Choice) => [...group.files].sort((a, b) => a.modified - b.modified)[0]?.path;
  const inFolder = (folder: string) => (group: Choice) => group.files.find((file) => within(file.path, folder))?.path;

  /** Searches the roots; `keepOutcome` leaves the banner saying what the last action did. */
  async function search(keepOutcome = false) {
    if (roots().length === 0) return;
    setSearching(true);
    setCancelled(false);
    setProgress(null);
    if (!keepOutcome) setOutcome(null);
    try {
      const found = await cleanupApi.dupesScan(roots(), exclude(), minSize());
      if (!found) {
        setCancelled(true);
        return;
      }
      setReport(found);
      // Then the photos that only look alike, from the vectors AI search already has: quick,
      // and nothing is read from disk. Without a model there are none.
      const alike = await cleanupApi.dupesSimilar(roots(), exclude(), minSize()).catch(() => null);
      setSimilar(alike);
      setIncluded(new Set<string>());
      setKeep(new Map());
      keepBy(oldest);
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setSearching(false);
    }
  }

  const removals = () =>
    [...chosen(), ...chosenSimilar()].map((group: Choice) => ({ keep: kept(group), remove: removing(group).map((file) => file.path) }));

  async function recycle() {
    setBusy(true);
    try {
      const done = await cleanupApi.dupesRecycle(removals(), false);
      setOutcome(outcomeOf(done));
      // What went is gone from the groups; groups left with one copy are done.
      const gone = new Set([...done.recycled, ...done.erased].map(lower));
      setReport((current) => {
        if (!current) return current;
        const left = current.groups
          .map((group) => ({ ...group, files: group.files.filter((file) => !gone.has(lower(file.path))) }))
          .filter((group) => group.files.length >= 2)
          .map((group) => ({ ...group, wasted: group.bytes_each * (group.files.length - 1) }));
        return { ...current, groups: left, wasted: left.reduce((n, group) => n + group.wasted, 0) };
      });
      setSimilar((current) =>
        current && {
          ...current,
          groups: current.groups
            .map((group) => ({ ...group, files: group.files.filter((file) => !gone.has(lower(file.path))) }))
            .filter((group) => group.files.length >= 2),
        },
      );
      setIncluded(new Set<string>());
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
      setDialog(null);
    }
  }

  async function dryRun() {
    setBusy(true);
    try {
      setDialog({ kind: "dry-run", report: await cleanupApi.dupesRecycle(removals(), true) });
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
    }
  }

  async function undo() {
    setBusy(true);
    try {
      const report = await api.undo();
      setOutcome({ message: report.message, skipped: [], undoable: false, tone: "done" });
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
    }
    // What came back is a duplicate again; the cache makes the search quick. The index hears of
    // the restored files from the drive's change journal a moment later.
    await new Promise((resolve) => setTimeout(resolve, 1000));
    await search(true);
  }

  function addRoot() {
    const folder = adding().trim().replace(/[\\/]+$/, "").replace(/\//g, "\\");
    if (!folder) return;
    const path = /^[a-z]:$/i.test(folder) ? `${folder}\\` : folder;
    if (!roots().some((root) => lower(root) === lower(path))) setRoots([...roots(), path]);
    setAdding("");
  }

  const toggleGroup = (group: { id: string }, on: boolean) =>
    setIncluded((current) => {
      const next = new Set(current);
      if (on) next.add(group.id);
      else next.delete(group.id);
      return next;
    });
  /** "Select all" is for exact duplicates only: similar photos are never chosen for the owner. */
  const allIncluded = () => groups().length > 0 && groups().every((group) => included().has(group.id));
  const selectAll = (on: boolean) =>
    setIncluded((current) => {
      const next = new Set([...current].filter((id) => !groups().some((group) => group.id === id)));
      if (on) for (const group of groups()) next.add(group.id);
      return next;
    });

  return (
    <section class="dupes-panel" aria-label="Duplicate files" aria-busy={searching()}>
      <header class="dupes-controls">
        <div class="dupes-roots">
          <span class="dupes-label">Look in</span>
          <For each={roots()}>
            {(root) => (
              <span class="dupes-root" title={root}>
                <CleanupIcon name="folder" size={13} />
                <span class="dupes-root-path">
                  <bdi>{root}</bdi>
                </span>
                <button
                  class="cleanup-icon-button"
                  aria-label={`Stop looking in ${root}`}
                  disabled={searching()}
                  onClick={() => setRoots(roots().filter((r) => r !== root))}
                >
                  <CleanupIcon name="close" size={12} />
                </button>
              </span>
            )}
          </For>
          <input
            class="dupes-add"
            placeholder="Add a folder, such as D:\Photos"
            aria-label="Add a folder to look in"
            spellcheck={false}
            value={adding()}
            disabled={searching()}
            onInput={(event) => setAdding(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                addRoot();
              }
            }}
          />
        </div>
        <div class="dupes-options">
          <label class="dupes-min">
            <span>Skip files under</span>
            <select value={minSize()} disabled={searching()} onChange={(event) => setMinSize(Number(event.currentTarget.value))}>
              <For each={MIN_SIZES}>{(size) => <option value={size.bytes}>{size.label}</option>}</For>
            </select>
          </label>
          <Show when={exclude().length > 0}>
            <span class="dupes-hint" title={exclude().join("\n")}>
              AppData is left out
            </span>
          </Show>
          <span class="cleanup-spacer" />
          <Show
            when={searching()}
            fallback={
              <button class="cleanup-button primary dupes-search" disabled={roots().length === 0} onClick={() => void search()}>
                Find duplicates
              </button>
            }
          >
            <button class="cleanup-button dupes-cancel" onClick={() => void cleanupApi.dupesCancel()}>
              Cancel
            </button>
          </Show>
        </div>
      </header>

      <Show when={searching()}>
        <div class="dupes-progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(describeProgress(progress()).share * 100)}>
          <span>{describeProgress(progress()).text}</span>
          <div class="dupes-bar">
            <div class="dupes-bar-fill" style={{ width: `${Math.round(describeProgress(progress()).share * 100)}%` }} />
          </div>
        </div>
      </Show>

      <Show when={outcome()}>
        {(shown) => (
          <OutcomeBanner
            outcome={shown()}
            busy={busy()}
            onUndo={() => void undo()}
            onShowSkipped={() => setDialog({ kind: "skipped" })}
            onDismiss={() => setOutcome(null)}
          />
        )}
      </Show>

      <div class="cleanup-scroll">
        <Show
          when={report()}
          fallback={
            <div class="cleanup-empty">
              <CleanupIcon name="copies" size={20} />
              <p>{cancelled() ? "The search was cancelled." : searching() ? "Searching…" : "Find files with exactly the same contents."}</p>
              <p class="cleanup-muted">
                Files are matched by size, then by their contents. Files only in the cloud are never downloaded to compare them.
              </p>
            </div>
          }
        >
          {(found) => (
            <>
              <div class="dupes-summary">
                <span class="dupes-summary-main">
                  <Show when={groups().length > 0} fallback="No duplicates found.">
                    {groups().length === 1 ? "1 group" : `${formatCount(groups().length)} groups`} of duplicates ·{" "}
                    <strong>{formatSize(found().wasted)}</strong> in extra copies
                  </Show>
                </span>
                <span class="dupes-summary-detail">
                  Compared {formatCount(found().candidates)} files
                  <Show when={found().reused > 0}>, {formatCount(found().reused)} from earlier searches</Show>
                  <Show when={found().placeholders > 0}>; {formatCount(found().placeholders)} online-only files left alone</Show>
                </span>
              </div>
              <Show when={groups().length > 0 || similarGroups().length > 0}>
                <div class="dupes-keep">
                  <label class="dupes-all" classList={{ hidden: groups().length === 0 }}>
                    <input
                      type="checkbox"
                      class="cleanup-check"
                      checked={allIncluded()}
                      onChange={(event) => selectAll(event.currentTarget.checked)}
                    />
                    <span>Select all</span>
                  </label>
                  <span class="cleanup-spacer" />
                  <span class="dupes-label">Keep</span>
                  <button class="cleanup-button dupes-keep-newest" onClick={() => keepBy(newest)}>
                    Newest
                  </button>
                  <button class="cleanup-button dupes-keep-oldest" onClick={() => keepBy(oldest)}>
                    Oldest
                  </button>
                  <select
                    class="dupes-keep-folder"
                    aria-label="Keep the copies in a folder"
                    value={keepFolder()}
                    onChange={(event) => {
                      const folder = event.currentTarget.value;
                      setKeepFolder(folder);
                      if (folder) keepBy(inFolder(folder));
                    }}
                  >
                    <option value="">Copies in a folder…</option>
                    <For each={folders()}>{(entry) => <option value={entry.folder}>{entry.folder}</option>}</For>
                  </select>
                </div>
                <For each={groups()}>
                  {(group) => (
                    <article class="dupes-group" classList={{ ticked: included().has(group.id) }} data-group={group.id}>
                      <header class="dupes-group-head">
                        <input
                          type="checkbox"
                          class="cleanup-check dupes-group-check"
                          checked={included().has(group.id)}
                          aria-label={`Clean up ${baseName(group.files[0].path)}`}
                          onChange={(event) => toggleGroup(group, event.currentTarget.checked)}
                        />
                        <span class="dupes-group-name">{baseName(kept(group))}</span>
                        <span class="dupes-group-meta">
                          {copies(group.files.length)} · {formatSize(group.size)} each
                        </span>
                        <span class="cleanup-spacer" />
                        <span class="dupes-group-wasted">{formatSize(group.wasted)} extra</span>
                      </header>
                      <ul class="dupes-files">
                        <For each={group.files}>
                          {(file) => {
                            const isKept = () => lower(kept(group)) === lower(file.path);
                            return (
                              <li class="dupes-file" classList={{ kept: isKept() }} data-path={file.path}>
                                <label class="dupes-keep-choice" title="Keep this copy">
                                  <input
                                    type="radio"
                                    name={`keep-${group.id}`}
                                    checked={isKept()}
                                    onChange={() => setKeep((current) => new Map(current).set(group.id, file.path))}
                                  />
                                </label>
                                <PathLabel path={file.path} class="dupes-file-path" />
                                <Show when={file.links.length > 0}>
                                  <span class="dupes-links" title={file.links.join("\n")}>
                                    and {file.links.length === 1 ? "1 other name" : `${file.links.length} other names`}
                                  </span>
                                </Show>
                                <span class="dupes-file-date">{formatDate(file.modified)}</span>
                                <span class="dupes-file-fate">{isKept() ? "Keep" : included().has(group.id) ? "Recycle" : ""}</span>
                              </li>
                            );
                          }}
                        </For>
                      </ul>
                    </article>
                  )}
                </For>
              </Show>
              <Show when={similarGroups().length > 0}>
                <section class="dupes-similar" aria-label="Similar photos">
                  <h3 class="dupes-similar-title">Similar photos</h3>
                  <p class="cleanup-muted dupes-similar-note">
                    Photos that look nearly the same without being copies, such as a burst or an edited copy, found by AI search's{" "}
                    {similar()?.model === "best" ? "Best" : "Fast"} model. None is ticked for you: tick a group to keep the chosen photo and move the
                    others to the Recycle Bin.
                  </p>
                  <For each={similarGroups()}>
                    {(group) => (
                      <article class="similar-group" classList={{ ticked: included().has(group.id) }} data-group={group.id}>
                        <header class="dupes-group-head">
                          <input
                            type="checkbox"
                            class="cleanup-check dupes-group-check"
                            checked={included().has(group.id)}
                            aria-label={`Clean up the photos like ${baseName(group.files[0].path)}`}
                            onChange={(event) => toggleGroup(group, event.currentTarget.checked)}
                          />
                          <span class="dupes-group-name">{baseName(kept(group))}</span>
                          <span class="dupes-group-meta">
                            {photos(group.files.length)} · {Math.round(group.score * 100)}% alike
                          </span>
                          <span class="cleanup-spacer" />
                          <span class="dupes-group-wasted">
                            {formatSize(removing(group).reduce((n, file) => n + file.bytes, 0))} in the others
                          </span>
                        </header>
                        <ul class="dupes-files">
                          <For each={group.files}>
                            {(file) => {
                              const isKept = () => lower(kept(group)) === lower(file.path);
                              return (
                                <li class="dupes-file" classList={{ kept: isKept() }} data-path={file.path}>
                                  <label class="dupes-keep-choice" title="Keep this photo">
                                    <input
                                      type="radio"
                                      name={`keep-${group.id}`}
                                      checked={isKept()}
                                      onChange={() => setKeep((current) => new Map(current).set(group.id, file.path))}
                                    />
                                  </label>
                                  <PathLabel path={file.path} class="dupes-file-path" />
                                  <span class="dupes-file-size">{formatSize(file.size)}</span>
                                  <span class="dupes-file-date">{formatDate(file.modified)}</span>
                                  <span class="dupes-file-fate">{isKept() ? "Keep" : included().has(group.id) ? "Recycle" : ""}</span>
                                </li>
                              );
                            }}
                          </For>
                        </ul>
                      </article>
                    )}
                  </For>
                </section>
              </Show>
              <Show when={similar() && similar()?.model === null}>
                <p class="cleanup-muted dupes-similar-none">
                  Similar photos (a burst, an edited copy) are found too once AI search has a model: download one on the AI search page.
                </p>
              </Show>
            </>
          )}
        </Show>
      </div>

      <Show when={groups().length > 0 || similarGroups().length > 0}>
        <footer class="cleanup-footer">
          <span class="cleanup-selected">
            {chosenCount() === 0 ? "Tick the groups to clean up." : `${moving(chosenFiles().length)} selected · ${formatSize(chosenBytes())}`}
          </span>
          <span class="cleanup-spacer" />
          <button class="cleanup-button cleanup-dry-run" disabled={chosenCount() === 0 || busy()} onClick={() => void dryRun()}>
            Dry run
          </button>
          <button class="cleanup-button primary cleanup-recycle" disabled={chosenCount() === 0 || busy()} onClick={() => setDialog({ kind: "recycle" })}>
            {chosen().length === 0 && chosenSimilar().length > 0 ? "Move photos to Recycle Bin…" : "Move copies to Recycle Bin…"}
          </button>
        </footer>
      </Show>

      <Show when={dialog()?.kind === "recycle"}>
        <CleanupDialog
          title={`Move ${moving(chosenFiles().length)} to the Recycle Bin?`}
          confirm="Move to Recycle Bin"
          busy={busy()}
          onConfirm={() => void recycle()}
          onCancel={() => setDialog(null)}
        >
          <p class="cleanup-dialog-lead">
            {toRecycleBin(chosenFiles().length, chosenBytes())} One copy of each file stays where it is
            {chosenSimilar().length > 0 ? ", and one photo of each group of similar photos (the others are not identical copies)." : "."}
          </p>
          <PathList label="Copies to move" items={chosenFiles().map((file) => ({ path: file.path, bytes: file.bytes }))} />
          <h3 class="cleanup-dialog-heading">Kept</h3>
          <PathList label="Copies kept" items={[...chosen(), ...chosenSimilar()].map((group) => ({ path: kept(group), note: "stays" }))} />
        </CleanupDialog>
      </Show>
      <Show when={dialog()?.kind === "dry-run" && (dialog() as { report: RecycleReport }).report}>
        {(dry) => (
          <DryRunDialog
            report={dry()}
            sizes={new Map(chosenFiles().map((file) => [lower(file.path), file.bytes] as const))}
            onClose={() => setDialog(null)}
          />
        )}
      </Show>
      <Show when={dialog()?.kind === "skipped" && outcome()}>
        {(shown) => (
          <CleanupDialog title="Skipped" cancel="Close" onCancel={() => setDialog(null)}>
            <SkippedList skipped={shown().skipped} heading="Left where they are" />
          </CleanupDialog>
        )}
      </Show>
    </section>
  );
}
