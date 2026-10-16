/**
 * Image sizes for the Content view, read from the images' headers by the backend
 * (`image_sizes` in layout.rs), which never opens a file whose contents are only in the cloud.
 * Items in view ask for theirs; the requests of one frame go together, and the answers are
 * remembered for each version of a file.
 */
import { createSignal } from "solid-js";

import type { Row } from "./api";
import { layoutApi } from "./layoutApi";

const IMAGE = new Set(["png", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "dib", "ico", "tif", "tiff", "psd"]);
/** Answers kept; the oldest are forgotten first. */
const MAX_KEPT = 4000;

const known = new Map<string, [number, number] | null>();
const waiting = new Set<string>();
let queued: Row[] = [];
let frame = 0;
const [tick, setTick] = createSignal(0);

const keyOf = (row: Pick<Row, "path" | "mtime">) => `${row.mtime}|${row.path}`;

/** Whether the Content view shows dimensions for a row: an image file. */
export const hasDimensions = (row: Pick<Row, "dir" | "ext">) => !row.dir && IMAGE.has(row.ext);

function flush() {
  frame = 0;
  const batch = queued;
  queued = [];
  if (batch.length === 0) return;
  void layoutApi
    .imageSizes(batch.map((row) => row.path))
    .then((sizes) => batch.forEach((row, i) => known.set(keyOf(row), sizes[i] ?? null)))
    .catch(() => batch.forEach((row) => known.set(keyOf(row), null)))
    .finally(() => {
      batch.forEach((row) => waiting.delete(keyOf(row)));
      if (known.size > MAX_KEPT) [...known.keys()].slice(0, known.size - MAX_KEPT).forEach((key) => known.delete(key));
      setTick((n) => n + 1);
    });
}

/**
 * An image's width and height, once known; `undefined` while it is being read, or while it is
 * not known and `ask` is false (the view is being scrolled).
 */
export function imageSize(row: Row, ask = true): [number, number] | null | undefined {
  tick();
  if (!hasDimensions(row)) return null;
  const key = keyOf(row);
  if (known.has(key)) return known.get(key);
  if (ask && !waiting.has(key)) {
    waiting.add(key);
    queued.push(row);
    if (frame === 0) frame = requestAnimationFrame(flush);
  }
  return undefined;
}
