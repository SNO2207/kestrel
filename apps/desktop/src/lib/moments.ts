/**
 * Where in each video an AI search matched, by path, while its results are on screen: the
 * items show the moment (`MomentBadge`), and the preview's and Quick Look's video player starts
 * there (`surfaceUrl` in previewSurface.ts). Kept apart from semantic.ts so the preview can read
 * it without importing the rest of the feature.
 */
import { createSignal } from "solid-js";

const moments = new Map<string, number>();
const [version, setVersion] = createSignal(0);

/** Adds the moments of a search's videos (milliseconds, by lower-case path). */
export function addMoments(found: Record<string, number>) {
  for (const [path, ms] of Object.entries(found)) moments.set(path.toLowerCase(), ms);
  setVersion((v) => v + 1);
}

/** Forgets every moment, as when the results that had them go. */
export function clearMoments() {
  if (moments.size === 0) return;
  moments.clear();
  setVersion((v) => v + 1);
}

/** Where in the video at `path` a search matched, in milliseconds, or null. Reactive. */
export function momentOf(path: string): number | null {
  version();
  return moments.get(path.toLowerCase()) ?? null;
}

/** A moment as a player shows it: `0:42`, `1:02:05`. */
export function formatMoment(ms: number): string {
  const total = Math.floor(ms / 1000);
  const [h, m, s] = [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60];
  const two = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}
