import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Backend commands and events of the projects hub and the git badges (`src-tauri/src/projects`). */

export type ProjectStatus = "active" | "paused" | "archived" | "idea";

export interface Branch {
  /** `null` when HEAD is detached. */
  name: string | null;
  /** Abbreviated commit HEAD points at; `null` before the first commit. */
  head: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
}

export interface GitCounts {
  staged: number;
  modified: number;
  untracked: number;
  conflicted: number;
}

export interface Commit {
  subject: string;
  author: string;
  /** Milliseconds since the Unix epoch. */
  time: number;
}

export interface GitFacts {
  branch: Branch;
  counts: GitCounts;
  last_commit: Commit | null;
  /** The `origin` remote, credentials removed. */
  remote: string | null;
  /** The repository's page on GitHub, GitLab or Bitbucket. */
  web_url: string | null;
}

export interface LanguageShare {
  name: string;
  bytes: number;
  /** From 0 to 1. */
  share: number;
}

export interface Reclaimable {
  path: string;
  bytes: number;
}

export interface ProjectView {
  root: string;
  name: string;
  /** Marker files found, such as `Cargo.toml`; `git` for a repository. */
  markers: string[];
  stack: string[];
  languages: LanguageShare[];
  is_repo: boolean;
  git: GitFacts | null;
  git_error: string | null;
  /** A repository whose git facts have not arrived yet. */
  git_pending: boolean;
  /** Milliseconds since the Unix epoch; 0 when unknown. */
  last_activity: number;
  size_on_disk: number;
  reclaimable: number;
  reclaimable_dirs: Reclaimable[];
  status: ProjectStatus;
  tags: string[];
  pinned: boolean;
  dismissed: boolean;
  has_notes: boolean;
}

export interface ProjectsSnapshot {
  projects: ProjectView[];
  /** The index is still being built, so the list may be incomplete. */
  indexing: boolean;
}

export interface Script {
  source: "npm" | "pnpm" | "yarn" | "bun" | "cargo" | "just" | "make";
  name: string;
  command: string;
}

export interface Launchers {
  vscode: boolean;
  /** The solution file Visual Studio would open. */
  solution: string | null;
  /** Windows Terminal is installed; without it a console window opens instead. */
  terminal: boolean;
  claude: boolean;
}

export interface RecentFile {
  path: string;
  name: string;
  mtime: number;
  size: number;
}

export interface ProjectDetail {
  root: string;
  notes: string;
  readme: { path: string; text: string; truncated: boolean } | null;
  scripts: Script[];
  launchers: Launchers;
  activity: {
    /** First day shown, in days since the Unix epoch in local time. */
    start: number;
    /** Days with activity: day, commits, files changed. */
    days: [number, number, number][];
  };
  recent: RecentFile[];
  languages: LanguageShare[];
  reclaimable_dirs: Reclaimable[];
}

export type Launcher = "vscode" | "visual-studio" | "terminal" | "claude" | "explorer" | "web";

export type GitChange = "untracked" | "added" | "modified" | "conflicted";

export interface GitBadge {
  change: GitChange | null;
  ignored: boolean;
}

export interface GitEvent {
  root: string;
  git: GitFacts | null;
  error: string | null;
}

/** What the development hook recorded instead of starting a program. */
export interface LaunchRecord {
  file: string;
  params: string;
  dir: string;
}

export const projectsApi = {
  /** The projects found; `refresh` looks again and refreshes every repository's git facts. */
  list: (refresh: boolean) => invoke<ProjectsSnapshot>("projects_list", { refresh }),
  detail: (root: string) =>
    invoke<ProjectDetail>("project_detail", { root, offsetMinutes: -new Date().getTimezoneOffset() }),
  setStatus: (root: string, status: ProjectStatus) => invoke<void>("project_set_status", { root, status }),
  /** Resolves to the tags as stored, cleaned up. */
  setTags: (root: string, tags: string[]) => invoke<string[]>("project_set_tags", { root, tags }),
  setNotes: (root: string, notes: string) => invoke<void>("project_set_notes", { root, notes }),
  setPinned: (root: string, pinned: boolean) => invoke<void>("project_set_pinned", { root, pinned }),
  /** "Not a project", or undoing it. */
  setDismissed: (root: string, dismissed: boolean) => invoke<void>("project_set_dismissed", { root, dismissed }),
  launch: (root: string, launcher: Launcher) => invoke<void>("project_launch", { root, launcher }),
  runScript: (root: string, script: Script) =>
    invoke<void>("project_run_script", { root, source: script.source, name: script.name }),
  /** Badges for items of a folder, in order; `repo` is the repository it is in, if any. */
  gitBadges: (folder: string, items: { name: string; dir: boolean }[]) =>
    invoke<{ repo: string | null; badges: GitBadge[] }>("git_badges", { folder, items }),
  onChanged: (handler: () => void) => listen("projects-changed", () => handler()),
  onGit: (handler: (event: GitEvent) => void) => listen<GitEvent>("project-git", (event) => handler(event.payload)),
};
