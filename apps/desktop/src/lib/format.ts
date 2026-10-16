const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${UNITS[unit]}`;
}

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

export function formatDate(ms: number): string {
  // Entries synthesised by the index, such as drive roots, have no timestamp.
  return ms > 0 ? dateTime.format(ms) : "";
}

const integer = new Intl.NumberFormat();

export function formatCount(n: number): string {
  return integer.format(n);
}

export function formatDuration(micros: number): string {
  return micros < 1000 ? `${micros} µs` : `${(micros / 1000).toFixed(1)} ms`;
}

/** The last component of a path, or the path itself for a drive root. */
export function baseName(path: string): string {
  const trimmed = path.replace(/\\+$/, "");
  const cut = trimmed.lastIndexOf("\\");
  return cut < 0 ? trimmed : trimmed.slice(cut + 1);
}

/** The containing folder, or `null` for a drive root. */
export function parentPath(path: string): string | null {
  const trimmed = path.replace(/\\+$/, "");
  const cut = trimmed.lastIndexOf("\\");
  if (cut < 0) return null;
  const parent = trimmed.slice(0, cut);
  return parent.endsWith(":") ? `${parent}\\` : parent;
}

/**
 * Splits `C:\Users\me` into crumbs, each with the path it navigates to. A share's path starts
 * with one crumb for the share, named after it: `\\server\share\docs` is `share`, `docs`.
 */
export function breadcrumbs(path: string): { label: string; path: string }[] {
  if (path.startsWith("\\\\") && !path.startsWith("\\\\?\\")) {
    const [server, share, ...rest] = path.slice(2).split("\\").filter(Boolean);
    if (!server) return [];
    if (!share) return [{ label: server, path: `\\\\${server}` }];
    const root = `\\\\${server}\\${share}`;
    return [{ label: share, path: root }, ...rest.map((label, i) => ({ label, path: [root, ...rest.slice(0, i + 1)].join("\\") }))];
  }
  const parts = path.split("\\").filter(Boolean);
  return parts.map((label, i) => {
    const joined = parts.slice(0, i + 1).join("\\");
    return { label, path: i === 0 ? `${joined}\\` : joined };
  });
}
