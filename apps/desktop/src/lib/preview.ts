import type { Row } from "./api";

/**
 * What the preview pane and Quick Look need to know about an item; `hit`, for a search inside
 * files' result, is where the text view opens (`lib/previewHit.ts`).
 */
export type PreviewItem = Pick<Row, "name" | "path" | "dir" | "size" | "mtime" | "ext" | "hit">;

/** How an item is shown. */
export type PreviewKind =
  | "folder"
  | "image"
  | "video"
  | "audio"
  | "pdf"
  | "markdown"
  | "json"
  | "code"
  | "text"
  /** Unknown: probed as text, otherwise only described. */
  | "other";

/** Files up to this size are fetched whole (`MAX_WHOLE` in `preview.rs`); larger images and PDFs are not shown. */
export const MAX_WHOLE = 64 * 1024 * 1024;
/** The most of a file read as text (`TEXT_LIMIT` in `preview.rs`). */
export const TEXT_LIMIT = 1024 * 1024;

const IMAGES = new Set(["png", "apng", "jpg", "jpeg", "jfif", "pjpeg", "pjp", "gif", "webp", "avif", "bmp", "ico", "cur", "svg"]);
const VIDEOS = new Set(["mp4", "m4v", "webm", "ogv", "mov", "mkv"]);
const AUDIO = new Set(["mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", "weba"]);
const MARKDOWN = new Set(["md", "markdown", "mdown", "mkd", "mkdn", "mdwn", "mdx"]);
const JSON_FILES = new Set(["json", "jsonc", "json5", "geojson", "webmanifest", "har", "jsonl", "ndjson"]);

/** Highlight.js language for each code extension. */
const LANGUAGES: Record<string, string> = {
  rs: "rust",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  pyw: "python",
  pyi: "python",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hh: "cpp",
  hxx: "cpp",
  inl: "cpp",
  ino: "cpp",
  cs: "csharp",
  csx: "csharp",
  java: "java",
  kt: "kotlin",
  kts: "kotlin",
  go: "go",
  rb: "ruby",
  php: "php",
  swift: "swift",
  m: "objectivec",
  mm: "objectivec",
  scala: "scala",
  dart: "dart",
  hs: "haskell",
  groovy: "groovy",
  gradle: "groovy",
  lua: "lua",
  pl: "perl",
  pm: "perl",
  r: "r",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  ps1: "powershell",
  psm1: "powershell",
  psd1: "powershell",
  bat: "dos",
  cmd: "dos",
  sql: "sql",
  html: "xml",
  htm: "xml",
  xhtml: "xml",
  xml: "xml",
  xsl: "xml",
  xslt: "xml",
  xaml: "xml",
  csproj: "xml",
  vcxproj: "xml",
  props: "xml",
  targets: "xml",
  resx: "xml",
  plist: "xml",
  manifest: "xml",
  vue: "xml",
  svelte: "xml",
  css: "css",
  scss: "scss",
  less: "less",
  yaml: "yaml",
  yml: "yaml",
  toml: "ini",
  ini: "ini",
  cfg: "ini",
  conf: "ini",
  inf: "ini",
  reg: "ini",
  editorconfig: "ini",
  properties: "properties",
  diff: "diff",
  patch: "diff",
  cmake: "cmake",
  graphql: "graphql",
  gql: "graphql",
  vb: "vbnet",
  vbs: "vbnet",
  mk: "makefile",
  dockerfile: "dockerfile",
};

/** Files known by their whole name rather than an extension. */
const NAMED: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
  gnumakefile: "makefile",
  "cmakelists.txt": "cmake",
  ".gitconfig": "ini",
  ".editorconfig": "ini",
  ".bashrc": "bash",
  ".profile": "bash",
};

const PLAIN = new Set(["txt", "text", "log", "csv", "tsv", "nfo", "lst", "out", "srt", "vtt", "env", "lock", "sum", "gitignore", "gitattributes"]);

export function kindOf(item: Pick<PreviewItem, "dir" | "ext" | "name">): PreviewKind {
  if (item.dir) return "folder";
  const ext = item.ext.toLowerCase();
  if (IMAGES.has(ext)) return "image";
  if (VIDEOS.has(ext)) return "video";
  if (AUDIO.has(ext)) return "audio";
  if (ext === "pdf") return "pdf";
  if (MARKDOWN.has(ext)) return "markdown";
  if (JSON_FILES.has(ext)) return "json";
  if (languageOf(item)) return "code";
  if (PLAIN.has(ext)) return "text";
  return "other";
}

/** The highlight.js language for a code file, or null for plain text. */
export function languageOf(item: Pick<PreviewItem, "ext" | "name">): string | null {
  return NAMED[item.name.toLowerCase()] ?? LANGUAGES[item.ext.toLowerCase()] ?? null;
}

/**
 * The URL the `preview` protocol serves a file's bytes at. `version` changes when the file
 * does, so the web view never shows a stale copy. The protocol serves an online-only file
 * only with `download`, which says the user asked for this item to be fetched from the cloud.
 */
export function previewUrl(path: string, version = 0, download = false): string {
  return `http://preview.localhost/?p=${encodeURIComponent(path)}&v=${version}${download ? "&download=1" : ""}`;
}

/**
 * Identifies what a preview shows, so a refresh that changes nothing reloads nothing. A file
 * is shown afresh when its size or time changes; a folder's size changes with anything
 * beneath it, so a folder keeps its preview and only its details are read again (`itemVersion`).
 */
export const itemKey = (item: PreviewItem | null) =>
  !item ? "" : item.dir ? `${item.path}|dir` : `${item.path}|${item.size}|${item.mtime}`;

/** Changes whenever the item's size or time does. */
export const itemVersion = (item: PreviewItem) => `${item.size}|${item.mtime}`;

/**
 * Re-indents valid JSON, or returns null for anything else. Works on the text rather than a
 * parsed value, so numbers too large for a double and key order come out exactly as written.
 */
export function prettyJson(source: string): string | null {
  try {
    JSON.parse(source);
  } catch {
    return null;
  }
  let out = "";
  let depth = 0;
  let inString = false;
  let escaped = false;
  const newline = () => `\n${"  ".repeat(depth)}`;
  for (let i = 0; i < source.length; i++) {
    const ch = source[i];
    if (inString) {
      out += ch;
      if (escaped) escaped = false;
      else if (ch === "\\") escaped = true;
      else if (ch === '"') inString = false;
      continue;
    }
    switch (ch) {
      case '"':
        inString = true;
        out += ch;
        break;
      case "{":
      case "[": {
        // An empty object or array stays on one line.
        let next = i + 1;
        while (next < source.length && /\s/.test(source[next])) next++;
        if (source[next] === (ch === "{" ? "}" : "]")) {
          out += ch + source[next];
          i = next;
        } else {
          depth++;
          out += ch + newline();
        }
        break;
      }
      case "}":
      case "]":
        depth--;
        out += newline() + ch;
        break;
      case ",":
        out += `,${newline()}`;
        break;
      case ":":
        out += ": ";
        break;
      case " ":
      case "\t":
      case "\n":
      case "\r":
        break;
      default:
        out += ch;
    }
  }
  return out;
}

/** The folder that contains a path. */
export function folderOf(path: string): string {
  const cut = path.lastIndexOf("\\");
  return cut <= 2 ? path.slice(0, 3) : path.slice(0, cut);
}

/**
 * How many of a folder's parts, split at backslashes, make up its root: 1 for a drive (`C:`),
 * 4 for a share (`\\server\share` splits into "", "", "server", "share"), or null for anything
 * else, such as a device path (`\\?\`, `\\.\`).
 */
function rootLength(parts: string[]): number | null {
  if (/^[a-z]:$/i.test(parts[0])) return 1;
  const [empty, alsoEmpty, server, share] = parts;
  const isShare = empty === "" && alsoEmpty === "" && !!server && server !== "?" && server !== "." && !!share;
  return isShare ? 4 : null;
}

/**
 * Resolves a relative link in a document against the document's folder, or returns null if it
 * is not a plain relative path: it has a scheme, starts at a root, or climbs out of the
 * document's drive or share. So a document on `\\server\share` can never name another server,
 * which merely opening a file there would send the user's credentials to.
 */
export function resolveRelative(folder: string, link: string): string | null {
  if (link === "" || /^[a-z][a-z0-9+.-]*:/i.test(link) || /^[\\/]/.test(link)) return null;
  let decoded: string;
  try {
    decoded = decodeURIComponent(link.replace(/[?#].*$/, ""));
  } catch {
    return null;
  }
  const parts = folder.replace(/\\+$/, "").split("\\");
  const root = rootLength(parts);
  if (root === null || parts.length < root) return null;
  for (const part of decoded.split(/[\\/]+/)) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (parts.length <= root) return null;
      parts.pop();
    } else {
      parts.push(part);
    }
  }
  return parts.length === 1 ? `${parts[0]}\\` : parts.join("\\");
}
