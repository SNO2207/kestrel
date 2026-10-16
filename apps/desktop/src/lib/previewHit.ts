/**
 * The preview of a search inside files' result opens at the matching line (`Row.hit`): the
 * text view marks the line and scrolls it to the middle; a rendered Markdown document, whose
 * lines are gone, scrolls to the first place showing the matched text.
 */
import type { Hit } from "./searchApi";

/** Puts a marker at the start of line `line` of highlighted HTML, whose line breaks are the text's. */
export function markLine(html: string, line: number | undefined): string {
  if (!line || line < 1) return html;
  let at = 0;
  for (let n = 1; n < line; n++) {
    const next = html.indexOf("\n", at);
    if (next < 0) return html;
    at = next + 1;
  }
  return `${html.slice(0, at)}<span class="preview-hit-line" aria-hidden="true"></span>${html.slice(at)}`;
}

/** The text of the line a marker starts: from the marker to the next line break. */
function lineAfter(marker: HTMLElement): { range: Range; nodes: Text[] } | null {
  const root = marker.closest("code") ?? marker.parentElement;
  if (!root) return null;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  walker.currentNode = marker;
  const range = document.createRange();
  range.setStartAfter(marker);
  const nodes: Text[] = [];
  for (let node = walker.nextNode() as Text | null; node; node = walker.nextNode() as Text | null) {
    const end = node.data.indexOf("\n");
    nodes.push(node);
    if (end >= 0) {
      range.setEnd(node, end);
      return { range, nodes };
    }
    range.setEnd(node, node.data.length);
  }
  return nodes.length > 0 ? { range, nodes } : null;
}

/** Marks the hit's matches within its line, with the CSS highlight API (`::highlight(kestrel-hit)`). */
function markMatches(line: { range: Range; nodes: Text[] }, hit: Hit) {
  if (!("highlights" in CSS)) return;
  const wanted = [...new Set(hit.ranges.map(([start, end]) => hit.text.slice(start, end).toLowerCase()).filter((text) => text.trim() !== ""))];
  const found: Range[] = [];
  for (const node of line.nodes) {
    const text = node.data.toLowerCase();
    const limit = node === line.range.endContainer ? line.range.endOffset : text.length;
    for (const match of wanted) {
      for (let at = text.indexOf(match); at >= 0 && at + match.length <= limit; at = text.indexOf(match, at + match.length)) {
        const range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + match.length);
        found.push(range);
      }
    }
  }
  CSS.highlights.set("kestrel-hit", new Highlight(...found));
}

/**
 * Scrolls the line marked by `markLine` into the middle of `container`, if it is there, marking
 * all of it (a line that wraps too) and the hit's matches in it.
 */
export function revealLine(container: HTMLElement, hit?: Hit): boolean {
  const marker = container.querySelector<HTMLElement>(".preview-hit-line");
  if (!marker) return false;
  const line = lineAfter(marker);
  if (line) {
    const height = line.range.getBoundingClientRect().height;
    if (height > 0) marker.style.height = `${height}px`;
    if (hit) markMatches(line, hit);
  }
  const box = container.getBoundingClientRect();
  const at = marker.getBoundingClientRect();
  container.scrollTop += at.top - box.top - container.clientHeight / 2 + at.height / 2;
  return true;
}

/** Scrolls to the first text in `root` that shows the hit's first match, as in rendered Markdown. */
export function revealText(container: HTMLElement, root: HTMLElement, hit: Hit) {
  const [range] = hit.ranges;
  const wanted = (range ? hit.text.slice(range[0], range[1]) : hit.text).trim().toLowerCase();
  if (!wanted) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const at = (node.textContent ?? "").toLowerCase().indexOf(wanted);
    if (at < 0) continue;
    const found = document.createRange();
    found.setStart(node, at);
    found.setEnd(node, at + wanted.length);
    const box = container.getBoundingClientRect();
    container.scrollTop += found.getBoundingClientRect().top - box.top - container.clientHeight / 3;
    return;
  }
}
