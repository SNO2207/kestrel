/**
 * The Developer tools page (a special location, `devtoolsSetup.ts`): the terminal pane's
 * settings, the query service other programs ask, and how to add Kestrel's MCP server to
 * Claude Code. Kestrel never edits Claude Code's or any MCP configuration itself: the page
 * shows the command to run, with a button that copies it.
 */
import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { loadSettings, openTerminal, settingsChanged } from "../devtools";
import { devtoolsApi, type DevtoolsSettings, type DevtoolsState, type QueryRecord } from "../lib/devtoolsApi";
import { formatDate } from "../lib/format";
import { setLocationStatus, type LocationPageProps } from "../lib/locations";
import { notify } from "../state";
import { Icon } from "./Icon";

/** Questions shown in the log. */
const LOG_SHOWN = 50;
/** How often the log is read again while the page is on screen. */
const LOG_EVERY_MS = 2000;

const TOOLS: [string, string][] = [
  ["search_files", "search every file by name, path, type, size or date, in Kestrel's query syntax"],
  ["list_folder", "a folder's files and subfolders with their sizes"],
  ["folder_size", "a folder's total size and how many files it holds"],
  ["list_projects", "the code projects on this PC, with their git state"],
  ["project_details", "a project's scripts, README, recent files and git state"],
  ["recent_files", "the files opened most recently"],
  ["disk_usage", "what uses the disk in a drive or folder"],
];

const OPS: Record<string, string> = {
  hello: "Checked Kestrel was running",
  search: "Searched",
  list_folder: "Listed a folder",
  folder_size: "Sized a folder",
  projects: "Listed projects",
  project_details: "Read a project",
  recent_files: "Listed recent files",
  disk_usage: "Read disk usage",
};

export function DevToolsPage(props: LocationPageProps) {
  let root!: HTMLDivElement;
  const [state, setState] = createSignal<DevtoolsState | null>(null);
  const [log, setLog] = createSignal<QueryRecord[]>([]);
  const [copied, setCopied] = createSignal(false);

  const refresh = async () => {
    try {
      setState(await devtoolsApi.state());
      setLog(await devtoolsApi.queries(LOG_SHOWN));
    } catch (error) {
      notify(error);
    }
  };

  const change = async (patch: Partial<DevtoolsSettings>) => {
    const current = state();
    if (!current) return;
    try {
      const next = await devtoolsApi.setSettings({ ...current.settings, ...patch });
      setState(next);
      settingsChanged(next.settings);
    } catch (error) {
      notify(error);
    }
  };

  const copyCommand = async () => {
    const command = state()?.claude_command;
    if (!command) return;
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      notify(error);
    }
  };

  onMount(() => {
    props.focusRef(() => root.focus());
    void loadSettings();
    // The log follows what other programs ask while the page is on screen.
    const timer = window.setInterval(() => {
      if (props.active) void devtoolsApi.queries(LOG_SHOWN).then(setLog).catch(() => {});
    }, LOG_EVERY_MS);
    onCleanup(() => {
      window.clearInterval(timer);
      setLocationStatus(props.tabId, null);
    });
  });
  createEffect(on(() => [props.revision, props.active], () => void refresh()));
  createEffect(() => {
    const current = state();
    setLocationStatus(props.tabId, current ? [current.service_running ? "Answering other programs" : "Not answering other programs"] : null);
  });

  const shellName = (id: string | null) => state()?.shells.shells.find((shell) => shell.id === id)?.name ?? "";

  return (
    <div ref={root} class="devtools-page" tabindex="-1" aria-label="Developer tools">
      <header class="devtools-header">
        <h1>
          <Icon name="terminal" size={18} />
          Developer tools
        </h1>
        <p>A terminal at the bottom of the window, and Kestrel's index for Claude Code and the command line.</p>
      </header>

      <Show when={state()}>
        {(current) => (
          <>
            <section class="devtools-section" aria-label="Terminal">
              <h2>Terminal</h2>
              <p class="devtools-note">
                Ctrl+` shows or hides the terminal, Ctrl+Shift+` opens another, and every folder's menu has "Open terminal here".
                Paths printed in a terminal open in Kestrel when clicked.
              </p>
              <label class="dialog-field devtools-field">
                Shell for new terminals
                <select
                  value={current().settings.default_shell ?? ""}
                  onChange={(event) => void change({ default_shell: event.currentTarget.value || null })}
                >
                  <option value="">Default ({shellName(current().shells.shells[0]?.id ?? null)})</option>
                  <For each={current().shells.shells}>{(shell) => <option value={shell.id}>{shell.name}</option>}</For>
                </select>
              </label>
              <label class="dialog-check devtools-check">
                <input
                  type="checkbox"
                  checked={current().settings.scripts_in_terminal}
                  onChange={(event) => void change({ scripts_in_terminal: event.currentTarget.checked })}
                />
                <span>
                  Run project scripts in the terminal pane
                  <span class="devtools-hint">The projects hub's Run buttons, instead of opening Windows Terminal.</span>
                </span>
              </label>
              <div>
                <button class="dialog-button" onClick={() => void openTerminal()}>
                  Open a terminal
                </button>
              </div>
            </section>

            <section class="devtools-section" aria-label="Claude Code">
              <h2>Claude Code</h2>
              <p class="devtools-note">
                Kestrel's MCP server lets Claude Code search and size files through Kestrel's index instead of walking folders. To add
                it for all your projects, run this once in a terminal:
              </p>
              <div class="devtools-command">
                <code class="devtools-command-text">{current().claude_command}</code>
                <button class="dialog-button devtools-copy" onClick={() => void copyCommand()}>
                  {copied() ? "Copied" : "Copy"}
                </button>
              </div>
              <Show
                when={current().mcp_path}
                fallback={
                  <p class="devtools-warning">
                    kestrel-mcp.exe isn't beside Kestrel yet. Build it with <code>cargo build --release -p kestrel-mcp</code> and put
                    it at {current().mcp_expected}.
                  </p>
                }
              >
                <p class="devtools-hint">
                  Leave out <code>--scope user</code> to add it to the current project only. Kestrel never changes Claude Code's
                  settings itself; <code>claude mcp remove kestrel</code> takes it out again.
                </p>
              </Show>
              <ul class="devtools-tools" aria-label="Tools">
                <For each={TOOLS}>
                  {([name, what]) => (
                    <li>
                      <code>{name}</code>
                      <span>{what}</span>
                    </li>
                  )}
                </For>
              </ul>
              <p class="devtools-hint">Every tool only reads: none can change, move or delete a file.</p>
            </section>

            <section class="devtools-section" aria-label="Other programs">
              <h2>Other programs</h2>
              <label class="dialog-check devtools-check">
                <input
                  type="checkbox"
                  checked={current().settings.query_service}
                  onChange={(event) => void change({ query_service: event.currentTarget.checked })}
                />
                <span>
                  Answer questions from other programs
                  <span class="devtools-hint">
                    Searches, folder listings and sizes, projects, recent files and disk usage, for programs running as you (the MCP
                    server and <code>kestrel-cli --live</code>). Nothing they ask can change a file, and other accounts can't connect.
                  </span>
                </span>
              </label>
              <p class="devtools-status" classList={{ off: !current().service_running }} data-running={current().service_running ? "yes" : "no"}>
                <span class="devtools-dot" aria-hidden="true" />
                {current().service_running
                  ? "Answering"
                  : (current().service_problem ?? (current().settings.query_service ? "Not answering" : "Turned off"))}
                <Show when={current().pipe}>{(pipe) => <code class="devtools-pipe">{pipe()}</code>}</Show>
              </p>
              <label class="dialog-check devtools-check">
                <input
                  type="checkbox"
                  checked={current().settings.log_queries}
                  onChange={(event) => void change({ log_queries: event.currentTarget.checked })}
                />
                <span>Keep a list of what they ask</span>
              </label>
              <div class="devtools-log-head">
                <h3>Asked lately</h3>
                <button class="link-button" disabled={log().length === 0} onClick={() => void devtoolsApi.clearQueries().then(refresh).catch(notify)}>
                  Clear
                </button>
              </div>
              <Show when={log().length > 0} fallback={<p class="devtools-empty">Nothing yet.</p>}>
                <table class="devtools-log">
                  <thead>
                    <tr>
                      <th>When</th>
                      <th>Program</th>
                      <th>Question</th>
                      <th class="devtools-took">Took</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={log()}>
                      {(query) => (
                        <tr classList={{ failed: !query.ok }}>
                          <td>{formatDate(query.at)}</td>
                          <td>{query.client}</td>
                          <td>
                            {OPS[query.op] ?? query.op}
                            <Show when={query.summary}>
                              <span class="devtools-summary"> {query.summary}</span>
                            </Show>
                            <Show when={!query.ok}>
                              <span class="devtools-failed"> (not answered)</span>
                            </Show>
                          </td>
                          <td class="devtools-took">{query.took_ms < 10 ? query.took_ms.toFixed(1) : Math.round(query.took_ms)} ms</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </Show>
            </section>
          </>
        )}
      </Show>
    </div>
  );
}
