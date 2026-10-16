/**
 * The details view's media columns (Date taken, Dimensions, Duration) read the media library's
 * facts for the rows on screen: asked for together, a moment after rows appear, and remembered
 * by path and modification time. What the library has not read yet is asked again when it says
 * it has news (`media-changed`).
 */
import { createSignal } from "solid-js";

import type { Row } from "./api";
import { mediaApi, mediaKindOf, type ItemFacts } from "./mediaApi";

const MAX_REMEMBERED = 20_000;
const BATCH_MS = 40;

const known = new Map<string, ItemFacts | null>();
const waiting = new Set<string>();
let timer: number | undefined;
let listening = false;
const [version, setVersion] = createSignal(0);

const keyOf = (row: Pick<Row, "path" | "mtime">) => `${row.path}|${row.mtime}`;

function ask() {
  timer = undefined;
  const rows = [...waiting];
  waiting.clear();
  const paths = rows.map((key) => key.slice(0, key.lastIndexOf("|")));
  void mediaApi
    .facts(paths)
    .then((facts) => {
      if (known.size > MAX_REMEMBERED) known.clear();
      rows.forEach((key, i) => known.set(key, facts[i] ?? null));
      setVersion((v) => v + 1);
    })
    .catch(() => rows.forEach((key) => known.set(key, null)));
}

function listen() {
  if (listening) return;
  listening = true;
  // Items read since are asked about again.
  void mediaApi.onChanged(() => {
    for (const [key, facts] of known) if (facts === null) known.delete(key);
    setVersion((v) => v + 1);
  });
}

/** A row's facts: `undefined` while being asked for, `null` when there are none. */
export function factsOf(row: Row): ItemFacts | null | undefined {
  version();
  if (row.dir || mediaKindOf(row.ext) === null) return null;
  const key = keyOf(row);
  if (known.has(key)) return known.get(key);
  listen();
  if (!waiting.has(key)) {
    waiting.add(key);
    timer ??= window.setTimeout(ask, BATCH_MS);
  }
  return undefined;
}
