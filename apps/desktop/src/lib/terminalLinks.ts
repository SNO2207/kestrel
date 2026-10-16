/**
 * Paths in a terminal's output, for making them links that open in Kestrel.
 *
 * Absolute Windows paths (`C:\x`) and WSL's view of Windows drives (`/mnt/c/x`) are found in a
 * line of text. Shares (`\\server\share`) are not: finding out whether one exists would make
 * Kestrel contact whatever server a program printed. A path may hold spaces, which output doesn't mark,
 * so each match offers its candidates longest first (the whole run, then cut at each space);
 * the backend says which exist, and the longest one that does becomes the link. A compiler's
 * `:line:col` or `(line,col)` after a file name is left out of the path.
 */

export interface PathMatch {
  /** Where the match starts in the line, as a string index. */
  start: number;
  /** Longest first: each a path to ask about and how many characters of the line it covers. */
  candidates: { path: string; length: number }[];
}

/** Characters no path holds: quotes, redirections, wildcards and control characters. */
const STOP = /["'<>|*?\u0000-\u001f\u007f]/;
/** Where a path starts: a drive, or a Windows drive seen from WSL. */
const START = /[A-Za-z]:\\|\/mnt\/[a-z]\//g;
/** What may come just before a path. */
const BEFORE = /[\s"'(=:<>[,;]/;
/** Punctuation that ends a sentence or a list rather than a path. */
const TRAILING = /[\s.,;:\]}]+$/;
/** `main.rs:10`, `main.rs:10:5`, `Program.cs(10,5)`. */
const POSITION = /(?::\d+(?::\d+)?|\(\d+(?:,\d+)?\))$/;
const MAX_CANDIDATES = 8;
const MAX_MATCHES = 8;

const count = (text: string, char: string) => text.split(char).length - 1;

/** Without the punctuation after it, and a closing bracket that closes something around it. */
function trimmed(text: string): string {
  let path = text;
  for (;;) {
    const before = path;
    path = path.replace(TRAILING, "");
    if (path.endsWith(")") && count(path, ")") > count(path, "(")) path = path.slice(0, -1);
    if (path === before) return path;
  }
}

/** A WSL path to a Windows drive (`/mnt/c/x`) as the Windows path (`C:\x`). */
function fromWsl(path: string): string {
  const match = /^\/mnt\/([a-z])(\/.*)?$/.exec(path);
  if (!match) return path;
  const rest = (match[2] ?? "/").replace(/\//g, "\\");
  return `${match[1].toUpperCase()}:${rest}`;
}

/**
 * The ways a run of text may end as a path, longest first: as it is (a folder may be called
 * "New folder (2)"), and without a compiler's position after it.
 */
function endings(text: string): string[] {
  const whole = trimmed(text);
  const bare = trimmed(whole.replace(POSITION, ""));
  return bare === whole ? [whole] : [whole, bare];
}

/** The paths in one line of text. */
export function findPaths(line: string): PathMatch[] {
  const matches: PathMatch[] = [];
  const starts = [...line.matchAll(START)].map((match) => match.index ?? 0).filter((at) => at === 0 || BEFORE.test(line[at - 1]));
  for (const [n, start] of starts.entries()) {
    if (matches.length >= MAX_MATCHES) break;
    // A run of path characters, up to the next path or anything no path holds.
    const limit = n + 1 < starts.length ? starts[n + 1] : line.length;
    let end = start;
    while (end < limit && !STOP.test(line[end])) end++;
    const run = line.slice(start, end);
    const wsl = run.startsWith("/mnt/");
    const ends: number[] = [run.length];
    for (let at = run.length - 1; at > 0; at--) if (run[at] === " ") ends.push(at);
    const candidates: { path: string; length: number }[] = [];
    for (const cut of ends) {
      for (const text of endings(run.slice(0, cut))) {
        if (text.length < 3 || candidates.length >= MAX_CANDIDATES) continue;
        const path = wsl ? fromWsl(text) : text;
        if (!candidates.some((other) => other.path === path)) candidates.push({ path, length: text.length });
      }
    }
    if (candidates.length > 0) matches.push({ start, candidates });
  }
  return matches;
}
