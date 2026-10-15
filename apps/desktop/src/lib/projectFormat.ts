import type { ProjectStatus, ProjectView } from "./projectsApi";
import { formatCount } from "./format";

const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
const STEPS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 86_400_000],
  ["month", 30 * 86_400_000],
  ["week", 7 * 86_400_000],
  ["day", 86_400_000],
  ["hour", 3_600_000],
  ["minute", 60_000],
];

/** "3 days ago", "last month", "just now". Empty for an unknown time. */
export function formatAgo(ms: number, now = Date.now()): string {
  if (ms <= 0) return "";
  const elapsed = now - ms;
  if (elapsed < 60_000) return "just now";
  for (const [unit, size] of STEPS) {
    if (elapsed >= size) return relative.format(-Math.floor(elapsed / size), unit);
  }
  return "just now";
}

export const STATUS_LABELS: Record<ProjectStatus, string> = {
  active: "Active",
  paused: "Paused",
  archived: "Archived",
  idea: "Idea",
};

const count = (n: number, one: string, many = one) => `${formatCount(n)} ${n === 1 ? one : many}`;

/** "2 staged, 1 modified, 3 untracked", or "No changes". */
export function describeChanges(p: ProjectView): string {
  const c = p.git?.counts;
  if (!c) return "";
  const parts = [];
  if (c.conflicted) parts.push(count(c.conflicted, "conflicted"));
  if (c.staged) parts.push(count(c.staged, "staged"));
  if (c.modified) parts.push(count(c.modified, "modified"));
  if (c.untracked) parts.push(count(c.untracked, "untracked"));
  return parts.length ? parts.join(", ") : "No changes";
}

/** Where the branch stands against its upstream. */
export function describeSync(p: ProjectView): string {
  const branch = p.git?.branch;
  if (!branch) return "";
  if (!branch.head) return "No commits yet";
  if (!branch.upstream) return "Not on a remote";
  if (!branch.ahead && !branch.behind) return "Up to date";
  const parts = [];
  if (branch.ahead) parts.push(`${formatCount(branch.ahead)} ahead`);
  if (branch.behind) parts.push(`${formatCount(branch.behind)} behind`);
  return parts.join(", ");
}

export const branchName = (p: ProjectView) => p.git?.branch.name ?? (p.git?.branch.head ? `detached at ${p.git.branch.head.slice(0, 7)}` : "");
