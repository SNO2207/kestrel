/**
 * Archives in the window: opening one as a read-only folder, "Extract all…", "Extract here",
 * "Compress to ZIP file" and "Compress to…", and the jobs they run, which show their progress
 * in a panel at the bottom right and can be cancelled. The backend is `src-tauri/src/archive`.
 *
 * An archive is browsed at an address made of its own path and the path inside it
 * (`C:\Downloads\photos.zip\2024`); such a listing carries `archive`, the archive's path on
 * disk, which is how everything here tells it is read-only.
 */
import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";

import type { Listing, Row } from "./lib/api";
import { archiveApi, asProblem, nextJobId, type ArchiveKind, type ArchiveLevel, type ArchivePreferences, type ArchiveProgress } from "./lib/archivesApi";
import { baseName, parentPath } from "./lib/format";
import type { MenuEntry } from "./components/ContextMenu";
import { isSearch, notify, openTab, refresh, renameWhenListed, selectInFolder, selectedRows, tabs } from "./state";

/** Extensions of the archives Kestrel opens, the longer ones first. */
const EXTENSIONS = [".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz2", ".tbz", ".txz", ".tar", ".zip", ".7z", ".rar", ".gz", ".bz2", ".xz"];

/** What Kestrel says when asked to change something inside an archive. */
export const READ_ONLY = "Archives are read-only in Kestrel. Extract the items to change them.";

export type ArchiveCommand = "extract-all" | "extract-here" | "extract-items" | "compress-zip" | "compress-to";

/** The extension that makes a name an archive's, or null. */
function archiveExtension(name: string): string | null {
  const lower = name.toLowerCase();
  return EXTENSIONS.find((ext) => lower.length > ext.length && lower.endsWith(ext)) ?? null;
}

/** Whether a row is an archive file, which opens as a folder. */
export const isArchiveFile = (row: Pick<Row, "dir" | "name">) => !row.dir && archiveExtension(row.name) !== null;

/** An archive's name without the extension that says what it is: `photos.tar.gz` gives `photos`. */
export function archiveStem(name: string): string {
  const ext = archiveExtension(name);
  return ext ? name.slice(0, name.length - ext.length) : name;
}

/** Whether a listing is of an archive, or a folder inside one. */
export const inArchive = (listing: Listing | null | undefined) => !!listing?.archive;

const tabOf = (tabId: number) => tabs.find((tab) => tab.id === tabId);

/** Joins a folder and a name, minding a drive root's own separator. */
const join = (folder: string, name: string) => `${folder.replace(/\\+$/, "")}\\${name}`;

// ---------- remembered choices ----------

const [preferences, setPreferencesSignal] = createSignal<ArchivePreferences>({ show_extracted: true, format: "zip", level: "normal" });
export { preferences };

export function setPreferences(next: ArchivePreferences) {
  setPreferencesSignal(next);
  void archiveApi.setPreferences(next).catch(() => {});
}

// ---------- jobs ----------

export interface JobView {
  id: number;
  title: string;
  phase: ArchiveProgress["phase"] | "starting";
  done: number;
  total: number;
  item: string;
  /** Shown once it has run long enough to be worth a panel. */
  shown: boolean;
  cancelling: boolean;
}

/** A job shows its progress only once it has run this long, so quick ones never flash. */
const SHOW_AFTER_MS = 350;

const [jobs, setJobs] = createStore<JobView[]>([]);
export { jobs };

function updateJob(progress: ArchiveProgress) {
  setJobs(
    (job) => job.id === progress.job,
    produce((job) => {
      job.phase = progress.phase;
      job.done = progress.done;
      job.total = progress.total;
      job.item = progress.item;
    }),
  );
}

export function cancelJob(id: number) {
  setJobs((job) => job.id === id, "cancelling", true);
  void archiveApi.cancel(id);
}

/** Listens for progress; call once, from a component, with the returned function to stop. */
export function startArchives() {
  void archiveApi.preferences().then(setPreferencesSignal).catch(() => {});
  return archiveApi.onProgress(updateJob);
}

// ---------- passwords ----------

export interface PasswordRequest {
  name: string;
  /** The last one tried was wrong. */
  wrong: boolean;
  answer: (password: string | null) => void;
}

const [passwordRequest, setPasswordRequest] = createSignal<PasswordRequest | null>(null);
export { passwordRequest };

/** Asks for an archive's password. It is used for the one job and never kept. */
function askPassword(name: string, wrong: boolean): Promise<string | null> {
  return new Promise((resolve) =>
    setPasswordRequest({
      name,
      wrong,
      answer: (password) => {
        setPasswordRequest(null);
        resolve(password);
      },
    }),
  );
}

/**
 * Runs a job with a progress panel, asking for a password when the archive needs one and again
 * when it was wrong. Resolves to null if the user cancelled; any other failure is told.
 */
async function runJob<T>(title: string, archiveName: string, start: (job: number, password: string | null) => Promise<T>): Promise<T | null> {
  /** One attempt, with its panel, which goes as soon as the attempt ends. */
  const attempt = async (password: string | null) => {
    const id = nextJobId();
    setJobs(jobs.length, { id, title, phase: "starting", done: 0, total: 0, item: "", shown: false, cancelling: false });
    const timer = window.setTimeout(() => setJobs((job) => job.id === id, "shown", true), SHOW_AFTER_MS);
    try {
      return await start(id, password);
    } finally {
      clearTimeout(timer);
      setJobs((all) => all.filter((job) => job.id !== id));
    }
  };
  let password: string | null = null;
  for (;;) {
    try {
      return await attempt(password);
    } catch (error) {
      const problem = asProblem(error);
      if (problem.kind === "password" || problem.kind === "wrong-password") {
        password = await askPassword(archiveName, problem.kind === "wrong-password");
        if (password !== null) continue;
        return null;
      }
      if (problem.kind !== "cancelled") notify(problem.message);
      return null;
    }
  }
}

// ---------- extracting ----------

export interface ExtractRequest {
  tabId: number;
  /** The archive, or for `items`, the folder inside an archive they are in. */
  source: string;
  /** Items inside an archive to extract, or null for the whole archive. */
  items: string[] | null;
  /** The archive's name, for the title. */
  name: string;
  destination: string;
}

const [extractRequest, setExtractRequest] = createSignal<ExtractRequest | null>(null);
export { extractRequest, setExtractRequest };

/** Says what an extraction had to leave out or rename, if anything. */
function describeExtraction(name: string, report: { links_skipped: number; renamed: number }) {
  const notes: string[] = [];
  if (report.links_skipped > 0) {
    notes.push(report.links_skipped === 1 ? "1 link was left out" : `${report.links_skipped} links were left out`);
  }
  if (report.renamed > 0) {
    notes.push(report.renamed === 1 ? "1 item was renamed for Windows" : `${report.renamed} items were renamed for Windows`);
  }
  if (notes.length > 0) notify(`Extracted ${name}: ${notes.join(", and ")}.`, "info");
}

/** Extracts as `request` says, then selects what it made and, if asked, shows it in a new tab. */
export async function extract(request: ExtractRequest, show: boolean) {
  const report = await runJob(`Extracting ${request.name}`, request.name, (job, password) =>
    archiveApi.extract(job, request.source, request.destination, request.items, password),
  );
  if (!report) return;
  selectInFolder(report.folder, report.names);
  describeExtraction(request.name, report);
  if (show) openTab(report.destination);
}

/** The folder an archive's files go to by default: one named after it, beside it. */
function defaultDestination(archive: string): string {
  const folder = parentPath(archive) ?? archive;
  return join(folder, archiveStem(baseName(archive)));
}

/** "Extract all…": the dialog, for an archive file, or for the archive a tab is inside. */
export async function extractAll(tabId: number) {
  const tab = tabOf(tabId);
  if (!tab?.listing) return;
  if (inArchive(tab.listing)) {
    // The archive the folder is in; its files go beside the archive on disk.
    const archive = tab.listing.archive!;
    setExtractRequest({ tabId, source: tab.path, items: null, name: baseName(archive), destination: defaultDestination(archive) });
    return;
  }
  const [row] = await selectedRows(tabId, 1);
  if (row && isArchiveFile(row)) setExtractRequest({ tabId, source: row.path, items: null, name: row.name, destination: defaultDestination(row.path) });
}

/** "Extract…" inside an archive: the selected items, to a folder chosen in the dialog. */
export async function extractItems(tabId: number) {
  const tab = tabOf(tabId);
  const archive = tab?.listing?.archive;
  if (!tab || !archive) return;
  const rows = await selectedRows(tabId, 100_000);
  if (rows.length === 0) return;
  setExtractRequest({ tabId, source: tab.path, items: rows.map((row) => row.path), name: baseName(archive), destination: defaultDestination(archive) });
}

/** "Extract here": the selected archives' contents into the folder they are in. */
export async function extractHere(tabId: number) {
  const rows = (await selectedRows(tabId, 64)).filter(isArchiveFile);
  for (const row of rows) {
    const folder = parentPath(row.path);
    if (folder) await extract({ tabId, source: row.path, items: null, name: row.name, destination: folder }, false);
  }
}

// ---------- compressing ----------

export interface CompressRequest {
  tabId: number;
  /**
   * What to compress, by path rather than row: the folder may be listed again while the dialog
   * is open, which ends the rows' result set.
   */
  paths: string[];
  count: number;
  /** The first item, for the title and the archive's name. */
  first: Row;
  folder: string;
}

/** The most items one compression or copy takes. */
const MAX_ITEMS = 100_000;

const [compressRequest, setCompressRequest] = createSignal<CompressRequest | null>(null);
export { compressRequest, setCompressRequest };

export const KIND_EXTENSIONS: Record<ArchiveKind, string> = { zip: ".zip", "7z": ".7z", "tar-gz": ".tar.gz" };

/** The name an archive of `first` gets: the item's name without its extension, as Explorer names it. */
export function archiveNameFor(first: Pick<Row, "name" | "dir">, kind: ArchiveKind): string {
  const dot = first.name.lastIndexOf(".");
  const stem = first.dir || dot <= 0 ? first.name : first.name.slice(0, dot);
  return `${stem}${KIND_EXTENSIONS[kind]}`;
}

/** What to compress: the tab's selection, from a folder (not search results or an archive). */
async function compressible(tabId: number): Promise<CompressRequest | null> {
  const tab = tabOf(tabId);
  if (!tab?.listing || isSearch(tab)) return null;
  if (inArchive(tab.listing)) {
    notify("Items inside an archive can't be compressed as they are. Extract them first.");
    return null;
  }
  const rows = await selectedRows(tabId, MAX_ITEMS);
  if (rows.length === 0) return null;
  return { tabId, paths: rows.map((row) => row.path), count: rows.length, first: rows[0], folder: tab.path };
}

/**
 * Writes the archive and selects it; with `rename`, starts renaming it in place, as Explorer
 * does after "Compress to ZIP file".
 */
export async function compress(
  request: CompressRequest,
  name: string | null,
  kind: ArchiveKind,
  level: ArchiveLevel,
  folder = request.folder,
  rename = true,
) {
  const subject = request.count === 1 ? request.first.name : `${request.count} items`;
  const report = await runJob(`Compressing ${subject}`, subject, (job) => archiveApi.compress(job, request.paths, folder, name, kind, level));
  if (!report) return;
  const tab = tabOf(request.tabId);
  if (rename && tab && !isSearch(tab) && tab.path.toLowerCase() === folder.toLowerCase()) {
    renameWhenListed(request.tabId, report.name);
    refresh(request.tabId);
  } else {
    selectInFolder(folder, [report.name]);
  }
}

/** "Compress to ZIP file": at once, named after the first item. */
export async function compressToZip(tabId: number) {
  const request = await compressible(tabId);
  if (request) await compress(request, null, "zip", "normal");
}

/** "Compress to…": the dialog with a name, format and level. */
export async function compressTo(tabId: number) {
  const request = await compressible(tabId);
  if (request) setCompressRequest(request);
}

// ---------- items inside archives ----------

/** Opens a file inside an archive from a copy in the temp folder, asking for a password if needed. */
export async function openEntry(tabId: number, row: Row) {
  const archive = tabOf(tabId)?.listing?.archive ?? "";
  await runJob(`Opening ${row.name}`, baseName(archive), (job, password) => archiveApi.open(job, row.path, password));
}

/** Copies items inside an archive to the clipboard, as files copied out of it. */
export async function copyFromArchive(tabId: number) {
  const archive = tabOf(tabId)?.listing?.archive;
  const paths = (await selectedRows(tabId, MAX_ITEMS)).map((row) => row.path);
  if (!archive || paths.length === 0) return;
  await runJob(paths.length === 1 ? "Copying 1 item" : `Copying ${paths.length} items`, baseName(archive), (job, password) =>
    archiveApi.copy(job, paths, password),
  );
}

/** Whether a tab shows a folder inside an archive. */
export const tabInArchive = (tabId: number) => inArchive(tabOf(tabId)?.listing);

/** Says that archives are read-only; for keys such as F2 inside one. */
export const notifyReadOnly = () => notify(READ_ONLY, "info");

/** Carries out an archive command from a menu or the palette. */
export function runArchiveCommand(tabId: number, command: ArchiveCommand) {
  switch (command) {
    case "extract-all":
      return void extractAll(tabId);
    case "extract-here":
      return void extractHere(tabId);
    case "extract-items":
      return void extractItems(tabId);
    case "compress-zip":
      return void compressToZip(tabId);
    case "compress-to":
      return void compressTo(tabId);
  }
}

// ---------- the context menu ----------

interface MenuParts {
  listing: Listing | null;
  /** The one selected row, if one is. */
  sole: Row | undefined;
  count: number;
  run: (command: ArchiveCommand) => () => void;
  open: () => void;
  openInNewTab: () => void;
  copy: () => void;
  /** Undo and redo, which the folder menu inside an archive keeps. */
  history: MenuEntry[];
  refresh: () => void;
}

/**
 * The context menu's archive entries. Inside an archive the menus are its own, read-only
 * ones (`items`, `folder`); elsewhere `extra` goes after the item menu's Open entries.
 */
export function archiveMenu(parts: MenuParts): { items: MenuEntry[] | null; folder: MenuEntry[] | null; extra: MenuEntry[] } {
  if (inArchive(parts.listing)) {
    const items: MenuEntry[] = [
      { label: "Open", hint: "Enter", disabled: !parts.sole, run: parts.open },
      ...(parts.sole?.dir || (parts.sole && isArchiveFile(parts.sole)) ? [{ label: "Open in new tab", run: parts.openInNewTab }] : []),
      "separator",
      { label: "Extract…", run: parts.run("extract-items") },
      { label: "Copy", hint: "Ctrl+C", run: parts.copy },
    ];
    const folder: MenuEntry[] = [
      { label: "Extract all…", run: parts.run("extract-all") },
      "separator",
      ...parts.history,
      "separator",
      { label: "Refresh", hint: "F5", run: parts.refresh },
    ];
    return { items, folder, extra: [] };
  }
  const browsing = parts.listing !== null && parts.listing.path !== "";
  const extra: MenuEntry[] = [];
  if (parts.sole && isArchiveFile(parts.sole)) {
    extra.push({ label: "Extract all…", run: parts.run("extract-all") }, { label: "Extract here", run: parts.run("extract-here") });
  }
  if (browsing && parts.count > 0) {
    extra.push({ label: "Compress to ZIP file", run: parts.run("compress-zip") }, { label: "Compress to…", run: parts.run("compress-to") });
  }
  return { items: null, folder: null, extra: extra.length > 0 ? ["separator", ...extra, "separator"] : [] };
}

// ---------- test hook ----------

// Development builds let the end-to-end tests see the jobs and what a copy would carry
// without touching the clipboard. Production builds compile this out.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kestrelArchives: {
      jobs: () => jobs.map((job) => ({ ...job })),
      materialize: (paths: string[]) => archiveApi.devMaterialize(paths),
    },
  });
}
