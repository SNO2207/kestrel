import { createStore } from "solid-js/store";

import { api, type Row } from "./api";

/** What the app says about anything in a folder this account can't open. */
export const LOCKED_MESSAGE = "Your account can't open this folder";

/** How long an answer is trusted before the folder is asked about again. */
const REMEMBER_MS = 120_000;
/** Rows that appear together are asked about in one request. */
const BATCH_MS = 16;

/**
 * Which folders this account can't open, by lower-cased path. Only folders of rows that have
 * been on screen are ever asked about, so a search of millions of results costs nothing extra.
 */
const [known, setKnown] = createStore<Record<string, boolean>>({});
const askedAt = new Map<string, number>();
let queued: string[] = [];
let timer: number | undefined;

const keyOf = (path: string) => path.toLowerCase();

async function flush() {
  timer = undefined;
  const batch = queued;
  queued = [];
  try {
    const answers = await api.lockedFolders(batch);
    batch.forEach((folder, i) => setKnown(keyOf(folder), answers[i] === true));
  } catch {
    // Ask again next time these rows are drawn.
    batch.forEach((folder) => askedAt.delete(keyOf(folder)));
  }
}

/**
 * Asks again about every folder the next time its rows are drawn, for when the user refreshes
 * after permissions changed. What is known stays shown until the new answers arrive.
 */
export function recheckAccess() {
  askedAt.clear();
}

/** Asks about any of these folders not asked about recently. */
export function checkFolders(folders: Iterable<string>) {
  const now = Date.now();
  for (const folder of folders) {
    const key = keyOf(folder);
    const at = askedAt.get(key);
    if (at !== undefined && now - at < REMEMBER_MS) continue;
    askedAt.set(key, now);
    queued.push(folder);
  }
  if (queued.length > 0 && timer === undefined) timer = window.setTimeout(() => void flush(), BATCH_MS);
}

/**
 * The folders that decide whether a row can be opened: the folder a search result sits in,
 * and the row itself when it is a folder. A folder listing's own folder is known to open.
 */
export function foldersOf(row: Pick<Row, "dir" | "path" | "location">): string[] {
  const folders: string[] = [];
  if (row.location) folders.push(row.location);
  if (row.dir) folders.push(row.path);
  return folders;
}

/** Whether the row is known to be in, or to be, a folder this account can't open. */
export const isLocked = (row: Pick<Row, "dir" | "path" | "location">) =>
  foldersOf(row).some((folder) => known[keyOf(folder)] === true);
