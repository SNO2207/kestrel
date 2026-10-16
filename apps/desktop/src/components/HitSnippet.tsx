import { For, Show, createMemo } from "solid-js";

import type { Hit } from "../lib/searchApi";
import { setPaneOpen } from "../previewState";

/** Characters kept before the first match, so that a narrow column still shows the match. */
const LEAD = 12;

/** The snippet starting a little before its first match, with what was cut marked. */
function nearMatch(hit: Hit): Hit {
  const first = hit.ranges[0]?.[0] ?? 0;
  if (first <= LEAD + 2) return hit;
  let cut = first - LEAD;
  // Never between the two halves of a character outside the Basic Multilingual Plane.
  const code = hit.text.charCodeAt(cut);
  if (code >= 0xdc00 && code <= 0xdfff) cut -= 1;
  const shift = 1 - cut;
  const ranges = hit.ranges.filter(([start]) => start >= cut).map(([start, end]): [number, number] => [start + shift, end + shift]);
  return { ...hit, text: `…${hit.text.slice(cut)}`, ranges };
}

/** The snippet's text cut into plain and matching parts. */
function parts(hit: Hit): { text: string; match: boolean }[] {
  const out: { text: string; match: boolean }[] = [];
  let at = 0;
  for (const [start, end] of hit.ranges) {
    if (start < at || end <= start) continue;
    if (start > at) out.push({ text: hit.text.slice(at, start), match: false });
    out.push({ text: hit.text.slice(start, end), match: true });
    at = end;
  }
  if (at < hit.text.length) out.push({ text: hit.text.slice(at), match: false });
  return out;
}

/** How many more lines match: `+3`, or `+999` and more. */
const moreLines = (hit: Hit) => {
  const more = hit.lines - 1;
  if (more <= 0 && !hit.more) return "";
  return hit.more ? `+${more}…` : `+${more}`;
};

const moreTitle = (hit: Hit) => {
  const more = hit.lines - 1;
  return hit.more ? `${more} or more other lines match` : more === 1 ? "1 other line matches" : `${more} other lines match`;
};

/**
 * A search inside files' match, as a result shows it: the line number, the line with the
 * matches marked, and how many more lines match. Clicking it opens the preview pane, which
 * shows the file at that line (the row is selected by the press, as any row is).
 */
export function HitSnippet(props: { hit: Hit }) {
  const pieces = createMemo(() => parts(nearMatch(props.hit)));
  return (
    <span
      class="hit-snippet"
      title={`Line ${props.hit.line}: ${props.hit.text}`}
      data-line={props.hit.line}
      onClick={(event) => {
        // A plain click, not one that extends the selection.
        if (!event.ctrlKey && !event.shiftKey) setPaneOpen(true);
      }}
    >
      <span class="hit-line">{props.hit.line}</span>
      <span class="hit-text">
        <For each={pieces()}>{(piece) => (piece.match ? <mark class="hit-mark">{piece.text}</mark> : piece.text)}</For>
      </span>
      <Show when={moreLines(props.hit)}>
        {(more) => (
          <span class="hit-more" title={moreTitle(props.hit)}>
            {more()}
          </span>
        )}
      </Show>
    </span>
  );
}
