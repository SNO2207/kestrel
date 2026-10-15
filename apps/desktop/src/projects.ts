import { createMemo, createRoot, createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";

import {
  projectsApi,
  type Launcher,
  type ProjectDetail,
  type ProjectStatus,
  type ProjectView,
  type Script,
} from "./lib/projectsApi";
import { baseName, parentPath } from "./lib/format";
import { activeTab, navigate, notify, openTab, selectInFolder } from "./state";

/**
 * The projects hub: whether it is open, the projects found, the filters, and the project whose
 * details are showing. The layout and sort order are remembered in local storage.
 */

export type Layout = "cards" | "table";
export type SortBy = "activity" | "name" | "size";

export const STATUSES: { value: ProjectStatus; label: string }[] = [
  { value: "active", label: "Active" },
  { value: "paused", label: "Paused" },
  { value: "archived", label: "Archived" },
  { value: "idea", label: "Idea" },
];

export interface Filters {
  text: string;
  status: ProjectStatus | "";
  stack: string;
  tag: string;
  /** Only repositories with uncommitted changes. */
  changes: boolean;
  /** Only repositories with commits not on their remote. */
  unpushed: boolean;
  /** Only projects nothing has happened in for 90 days. */
  untouched: boolean;
}

const PREFS_KEY = "kestrel.projects";
const NOTES_SAVE_MS = 600;
const UNTOUCHED_MS = 90 * 86_400_000;

function loadPrefs(): { layout: Layout; sort: SortBy } {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
    return {
      layout: saved.layout === "table" ? "table" : "cards",
      sort: saved.sort === "name" || saved.sort === "size" ? saved.sort : "activity",
    };
  } catch {
    return { layout: "cards", sort: "activity" };
  }
}

function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ layout: layout(), sort: sortBy() }));
  } catch {
    // Remembering the layout is a convenience; without storage the defaults return.
  }
}

const prefs = loadPrefs();
const [open, setOpen] = createSignal(false);
const [state, setState] = createStore<{ projects: ProjectView[]; indexing: boolean; loaded: boolean }>({
  projects: [],
  indexing: false,
  loaded: false,
});
const EMPTY_FILTERS: Filters = { text: "", status: "", stack: "", tag: "", changes: false, unpushed: false, untouched: false };
const [filters, setFilters] = createStore<Filters>({ ...EMPTY_FILTERS });
const [layout, setLayoutSignal] = createSignal<Layout>(prefs.layout);
const [sortBy, setSortBySignal] = createSignal<SortBy>(prefs.sort);
const [showDismissed, setShowDismissed] = createSignal(false);
const [selected, setSelected] = createSignal<string | null>(null);
const [detail, setDetail] = createSignal<ProjectDetail | null>(null);
const [detailError, setDetailError] = createSignal<string | null>(null);

export { open as projectsOpen, state as projectsState, filters, layout, sortBy, showDismissed, selected, detail, detailError };

export const setFilter = <K extends keyof Filters>(key: K, value: Filters[K]) => setFilters(key, value);
export const clearFilters = () => setFilters({ ...EMPTY_FILTERS });
export const setLayout = (next: Layout) => (setLayoutSignal(next), savePrefs());
export const setSortBy = (next: SortBy) => (setSortBySignal(next), savePrefs());
export { setShowDismissed };

const sameRoot = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
export const findProject = (root: string) => state.projects.find((p) => sameRoot(p.root, root));

// ---------- what the filters ask about ----------

export const changeCount = (p: ProjectView) => {
  const c = p.git?.counts;
  return c ? c.staged + c.modified + c.untracked + c.conflicted : 0;
};
/** Commits that are not on the remote: ahead of the upstream, or a branch with no upstream. */
export const isUnpushed = (p: ProjectView) =>
  !!p.git && (p.git.branch.ahead > 0 || (p.git.branch.upstream === null && p.git.branch.head !== null));
/** The latest sign of life: a file written or a commit made. */
export const lastTouched = (p: ProjectView) => Math.max(p.last_activity, p.git?.last_commit?.time ?? 0);
export const isUntouched = (p: ProjectView, now = Date.now()) => {
  const at = lastTouched(p);
  return at > 0 && now - at > UNTOUCHED_MS;
};

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

function matchesText(p: ProjectView, text: string) {
  const words = text.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const haystack = [p.name, p.root, ...p.tags, ...p.stack, p.git?.branch.name ?? "", p.status].join("\n").toLowerCase();
  return words.every((word) => haystack.includes(word));
}

/** Derived lists live as long as the app, in a root of their own. */
const derived = createRoot(() => ({
  visible: createMemo(() => filterAndSort()),
  stacks: createMemo(() =>
    [...new Set(state.projects.filter((p) => !p.dismissed).flatMap((p) => p.stack))].sort(collator.compare),
  ),
  tags: createMemo(() => {
    const tags = new Map<string, string>();
    for (const p of state.projects) for (const tag of p.tags) if (!tags.has(tag.toLowerCase())) tags.set(tag.toLowerCase(), tag);
    return [...tags.values()].sort(collator.compare);
  }),
  dismissed: createMemo(() => state.projects.filter((p) => p.dismissed).length),
}));

/** The projects to show: filtered, then pinned first and sorted. */
export const visibleProjects = () => derived.visible();
/** Every stack and tag in use, for the filter menus. */
export const stackChoices = () => derived.stacks();
export const tagChoices = () => derived.tags();
export const dismissedCount = () => derived.dismissed();

function filterAndSort() {
  const now = Date.now();
  const shown = state.projects.filter(
    (p) =>
      p.dismissed === showDismissed() &&
      matchesText(p, filters.text) &&
      (!filters.status || p.status === filters.status) &&
      (!filters.stack || p.stack.includes(filters.stack)) &&
      (!filters.tag || p.tags.some((tag) => tag.toLowerCase() === filters.tag.toLowerCase())) &&
      (!filters.changes || changeCount(p) > 0) &&
      (!filters.unpushed || isUnpushed(p)) &&
      (!filters.untouched || isUntouched(p, now)),
  );
  const order = sortBy();
  const compare = (a: ProjectView, b: ProjectView) => {
    if (order === "name") return collator.compare(a.name, b.name);
    if (order === "size") return b.size_on_disk - a.size_on_disk;
    return lastTouched(b) - lastTouched(a);
  };
  return shown.sort((a, b) => Number(b.pinned) - Number(a.pinned) || compare(a, b) || collator.compare(a.root, b.root));
}

// ---------- loading ----------

let listening = false;
let loading: Promise<void> | null = null;

async function load(refresh: boolean) {
  const snapshot = await projectsApi.list(refresh);
  setState("projects", reconcile(snapshot.projects, { key: "root", merge: true }));
  setState({ indexing: snapshot.indexing, loaded: true });
}

function listen() {
  if (listening) return;
  listening = true;
  void projectsApi.onChanged(() => {
    if (open()) void load(false).catch(notify);
  });
  void projectsApi.onGit((event) => {
    setState(
      "projects",
      (p) => sameRoot(p.root, event.root),
      produce((p) => {
        p.git = event.git;
        p.git_error = event.error;
        p.git_pending = false;
      }),
    );
  });
}

export function openProjects() {
  listen();
  setOpen(true);
  loading = load(true).catch(notify);
}

export function closeProjects() {
  void flushNotes();
  setOpen(false);
}

export const toggleProjects = () => (open() ? closeProjects() : openProjects());

/** Resolves once the list from the latest opening has arrived. */
export const projectsLoaded = () => loading ?? Promise.resolve();

// ---------- the selected project ----------

let detailRequest = 0;

export async function selectProject(root: string | null) {
  await flushNotes();
  setSelected(root);
  setDetail(null);
  setDetailError(null);
  if (root) await reloadDetail();
}

export async function reloadDetail() {
  const root = selected();
  if (!root) return;
  const request = ++detailRequest;
  try {
    const loaded = await projectsApi.detail(root);
    if (request === detailRequest) setDetail(loaded);
  } catch (error) {
    if (request === detailRequest) setDetailError(String(error));
  }
}

function patchProject(root: string, change: (p: ProjectView) => void) {
  setState("projects", (p) => sameRoot(p.root, root), produce(change));
}

export async function setProjectStatus(root: string, status: ProjectStatus) {
  patchProject(root, (p) => void (p.status = status));
  await projectsApi.setStatus(root, status).catch(notify);
}

export async function setProjectTags(root: string, tags: string[]) {
  try {
    const stored = await projectsApi.setTags(root, tags);
    patchProject(root, (p) => void (p.tags = stored));
  } catch (error) {
    notify(error);
  }
}

export async function setProjectPinned(root: string, pinned: boolean) {
  patchProject(root, (p) => void (p.pinned = pinned));
  await projectsApi.setPinned(root, pinned).catch(notify);
}

/** "Not a project", or bringing a dismissed folder back. */
export async function setProjectDismissed(root: string, dismissed: boolean) {
  patchProject(root, (p) => void (p.dismissed = dismissed));
  if (dismissed && selected() && sameRoot(selected()!, root)) await selectProject(null);
  await projectsApi.setDismissed(root, dismissed).catch(notify);
}

// Notes are saved shortly after typing pauses, and at once when the panel changes or closes.
let pendingNotes: { root: string; notes: string } | null = null;
let notesTimer: number | undefined;
const [notesSaved, setNotesSaved] = createSignal<"saved" | "saving" | "unsaved">("saved");
export { notesSaved };

export function editNotes(root: string, notes: string) {
  pendingNotes = { root, notes };
  setNotesSaved("unsaved");
  clearTimeout(notesTimer);
  notesTimer = window.setTimeout(() => void flushNotes(), NOTES_SAVE_MS);
}

export async function flushNotes() {
  clearTimeout(notesTimer);
  const pending = pendingNotes;
  pendingNotes = null;
  if (!pending) return;
  setNotesSaved("saving");
  try {
    await projectsApi.setNotes(pending.root, pending.notes);
    patchProject(pending.root, (p) => void (p.has_notes = pending.notes.trim() !== ""));
    const shown = detail();
    if (shown && sameRoot(shown.root, pending.root)) setDetail({ ...shown, notes: pending.notes });
    if (!pendingNotes) setNotesSaved("saved");
  } catch (error) {
    setNotesSaved("unsaved");
    notify(error);
  }
}

// ---------- leaving for elsewhere ----------

export async function launchProject(root: string, launcher: Launcher) {
  await projectsApi.launch(root, launcher).catch(notify);
}

export async function runScript(root: string, script: Script) {
  await projectsApi.runScript(root, script).catch(notify);
}

/** Shows a folder in the current tab (or a new one) and closes the hub. */
export function showInKestrel(path: string) {
  const tab = activeTab();
  closeProjects();
  if (tab) navigate(tab.id, path);
  else openTab(path);
}

/** Shows a file in its folder, selected. */
export async function revealFile(path: string) {
  const folder = parentPath(path);
  if (!folder) return;
  showInKestrel(folder);
  // Once the folder is on screen, the file is selected as a pasted one would be.
  for (let attempt = 0; attempt < 100; attempt++) {
    const tab = activeTab();
    if (tab?.listing && sameRoot(tab.path, folder)) return selectInFolder(folder, [baseName(path)]);
    await new Promise((resolve) => setTimeout(resolve, 30));
  }
}
