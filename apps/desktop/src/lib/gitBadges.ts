import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

import type { Row } from "./api";
import { projectsApi, type GitBadge } from "./projectsApi";

/** Rows that appear together are asked about in one request per folder. */
const BATCH_MS = 30;
/** Listings whose questions are remembered; older ones are simply asked about again. */
const REMEMBERED_LISTINGS = 16;

/**
 * Git badges of items in folder views, by lower-cased path. Only rows that have been on screen
 * are asked about, so a folder of thousands of files costs a request for the few dozen showing.
 * Each listing asks afresh (a refresh or a change in the folder makes a new one), and answers
 * are asked for again when the repository's status changes (the `project-git` event). Answers
 * already known stay shown until the new ones arrive, so badges never flicker.
 */
const [known, setKnown] = createStore<Record<string, GitBadge>>({});
/** Bumped when answers are invalidated, so the lists ask again for the rows they show. */
const [epoch, setEpoch] = createSignal(0);
/** Lower-cased paths asked about, per listing (result set id). */
const asked = new Map<number, Set<string>>();
let queued = new Map<string, { folder: string; items: Map<string, { name: string; dir: boolean; path: string; set: number }> }>();
let timer: number | undefined;
let listening = false;

const keyOf = (path: string) => path.toLowerCase();

async function flush() {
  timer = undefined;
  const batch = queued;
  queued = new Map();
  for (const { folder, items } of batch.values()) {
    const list = [...items.values()];
    try {
      const answer = await projectsApi.gitBadges(folder, list.map(({ name, dir }) => ({ name, dir })));
      list.forEach((item, i) => setKnown(keyOf(item.path), answer.badges[i] ?? { change: null, ignored: false }));
    } catch {
      // Ask again next time these rows are drawn.
      list.forEach((item) => asked.get(item.set)?.delete(keyOf(item.path)));
    }
  }
}

/** Forgets which rows in the repository at `root` were asked about, so they are asked again. */
function invalidate(root: string) {
  const prefix = `${keyOf(root).replace(/\\+$/, "")}\\`;
  let any = false;
  for (const paths of asked.values()) {
    for (const path of paths) {
      if (path.startsWith(prefix)) {
        paths.delete(path);
        any = true;
      }
    }
  }
  if (any) setEpoch((n) => n + 1);
}

function listen() {
  if (listening) return;
  listening = true;
  void projectsApi.onGit((event) => invalidate(event.root));
}

/**
 * Asks for the badges of these rows of the folder listing `set` of `folder`, those not yet
 * asked about. Reads the invalidation epoch, so an effect calling this runs again when
 * answers go stale.
 */
export function checkGitBadges(folder: string, set: number, rows: Iterable<Pick<Row, "name" | "dir" | "path">>) {
  epoch();
  listen();
  let paths = asked.get(set);
  if (!paths) {
    paths = new Set();
    asked.set(set, paths);
    if (asked.size > REMEMBERED_LISTINGS) asked.delete(asked.keys().next().value!);
  }
  for (const row of rows) {
    const key = keyOf(row.path);
    if (paths.has(key)) continue;
    paths.add(key);
    const entry = queued.get(keyOf(folder)) ?? { folder, items: new Map() };
    entry.items.set(key, { name: row.name, dir: row.dir, path: row.path, set });
    queued.set(keyOf(folder), entry);
  }
  if (queued.size > 0 && timer === undefined) timer = window.setTimeout(() => void flush(), BATCH_MS);
}

/** The row's badge, once known. */
export const gitBadge = (row: Pick<Row, "path"> | undefined): GitBadge | undefined =>
  row ? known[keyOf(row.path)] : undefined;

/** Whether git ignores the row, which is then shown dimmed. */
export const gitIgnored = (row: Pick<Row, "path"> | undefined) => gitBadge(row)?.ignored === true;
