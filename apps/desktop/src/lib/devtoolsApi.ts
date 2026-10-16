import { Channel, invoke } from "@tauri-apps/api/core";

/** Backend commands of the developer tools (`src-tauri/src/devtools`): the terminal pane and the query service. */

export interface Shell {
  /** `pwsh`, `powershell`, `cmd`, or `wsl:<distribution>`. */
  id: string;
  name: string;
  kind: "powershell" | "cmd" | "wsl";
  program: string;
}

export interface ShellList {
  shells: Shell[];
  /** The shell new terminals start: the settings' choice, else PowerShell 7 if installed, else Windows PowerShell. */
  default: string | null;
}

export type TerminalEvent = { kind: "output"; data: string } | { kind: "exit"; code: number | null };

export interface OpenedTerminal {
  id: number;
  pid: number;
  shell: string;
  shell_name: string;
  cwd: string;
  /** The shell's name, or the script it runs. */
  title: string;
}

/** One of a project's scripts, for running in the terminal pane. */
export interface ScriptRef {
  root: string;
  source: string;
  name: string;
}

export interface DevtoolsSettings {
  default_shell: string | null;
  scripts_in_terminal: boolean;
  query_service: boolean;
  log_queries: boolean;
}

export interface DevtoolsState {
  settings: DevtoolsSettings;
  shells: ShellList;
  service_running: boolean;
  service_problem: string | null;
  pipe: string | null;
  mcp_path: string | null;
  mcp_expected: string;
  claude_command: string;
  cli_path: string | null;
}

export interface QueryRecord {
  /** Milliseconds since the Unix epoch. */
  at: number;
  client: string;
  op: string;
  summary: string;
  ok: boolean;
  took_ms: number;
}

/** A terminal as the development hook reports it. */
export interface RunningTerminal {
  id: number;
  window: string;
  shell: string;
  pid: number;
  processes: number[];
}

export const devtoolsApi = {
  shells: () => invoke<ShellList>("terminal_shells"),
  open: (
    options: { cwd: string | null; shell: string | null; script: ScriptRef | null; cols: number; rows: number },
    onEvent: (event: TerminalEvent) => void,
  ) => {
    const channel = new Channel<TerminalEvent>();
    channel.onmessage = onEvent;
    return invoke<OpenedTerminal>("terminal_open", { ...options, onEvent: channel });
  },
  write: (id: number, data: string) => invoke<void>("terminal_write", { id, data }),
  resize: (id: number, cols: number, rows: number) => invoke<void>("terminal_resize", { id, cols, rows }),
  close: (id: number) => invoke<void>("terminal_close", { id }),
  reset: () => invoke<void>("terminal_reset"),
  cd: (id: number, folder: string) => invoke<void>("terminal_cd", { id, folder }),
  /** What each path is: a folder, a file, or nothing (or no answer in time). */
  paths: (paths: string[]) => invoke<("folder" | "file" | null)[]>("terminal_paths", { paths }),
  clipboardText: () => invoke<string | null>("terminal_clipboard_text"),
  state: () => invoke<DevtoolsState>("devtools_state"),
  setSettings: (settings: DevtoolsSettings) => invoke<DevtoolsState>("devtools_set_settings", { settings }),
  queries: (limit: number) => invoke<QueryRecord[]>("devtools_queries", { limit }),
  clearQueries: () => invoke<number>("devtools_clear_queries"),
  devSessions: () => invoke<RunningTerminal[]>("terminal_dev_sessions"),
  devAlive: (pids: number[]) => invoke<boolean[]>("terminal_dev_alive", { pids }),
};
