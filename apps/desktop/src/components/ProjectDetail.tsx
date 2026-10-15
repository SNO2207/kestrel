import { For, Show, createEffect, createSignal, on } from "solid-js";

import { api } from "../lib/api";
import { formatDate, formatSize } from "../lib/format";
import { linkTarget, renderMarkdown } from "../lib/markdown";
import { STATUS_LABELS, branchName, describeChanges, describeSync, formatAgo } from "../lib/projectFormat";
import type { Launcher, ProjectStatus, ProjectView } from "../lib/projectsApi";
import {
  STATUSES,
  detail,
  detailError,
  editNotes,
  findProject,
  flushNotes,
  launchProject,
  lastTouched,
  notesSaved,
  revealFile,
  runScript,
  selectProject,
  setProjectDismissed,
  setProjectPinned,
  setProjectStatus,
  setProjectTags,
  showInKestrel,
} from "../projects";
import { notify } from "../state";
import { Heatmap } from "./Heatmap";
import { ProjectIcon, type ProjectIconName } from "./ProjectIcon";

/** Renders Markdown from `folder` into a container, with links opened through the shell. */
function Markdown(props: { source: string; folder: string; class?: string }) {
  let container: HTMLDivElement | undefined;
  const [html, setHtml] = createSignal("");
  createEffect(
    on([() => props.source, () => props.folder], ([source, folder]) => {
      void renderMarkdown(source, folder).then(setHtml);
    }),
  );
  return (
    <div
      ref={container}
      class={`preview-markdown ${props.class ?? ""}`}
      innerHTML={html()}
      onClick={(event) => {
        if (!container) return;
        const link = linkTarget(event, container);
        if (link) void api.openLink(link).catch(notify);
      }}
    />
  );
}

function TagEditor(props: { project: ProjectView }) {
  const [draft, setDraft] = createSignal("");
  const add = () => {
    const tag = draft().trim();
    if (!tag) return;
    setDraft("");
    void setProjectTags(props.project.root, [...props.project.tags, tag]);
  };
  return (
    <div class="tag-editor">
      <For each={props.project.tags}>
        {(tag) => (
          <span class="tag">
            {tag}
            <button
              class="tag-remove"
              aria-label={`Remove tag ${tag}`}
              onClick={() => void setProjectTags(props.project.root, props.project.tags.filter((t) => t !== tag))}
            >
              <ProjectIcon name="close" size={10} />
            </button>
          </span>
        )}
      </For>
      <input
        class="tag-input"
        aria-label="Add a tag"
        placeholder={props.project.tags.length ? "Add a tag" : "Add tags, such as client or experiment"}
        spellcheck={false}
        value={draft()}
        onInput={(event) => setDraft(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === ",") {
            event.preventDefault();
            add();
          } else if (event.key === "Backspace" && draft() === "" && props.project.tags.length > 0) {
            void setProjectTags(props.project.root, props.project.tags.slice(0, -1));
          }
        }}
        onBlur={add}
      />
    </div>
  );
}

function NotesEditor(props: { root: string; notes: string }) {
  const [text, setText] = createSignal(props.notes);
  const [previewing, setPreviewing] = createSignal(false);
  // A different project, or notes reloaded from elsewhere, replace what is shown.
  createEffect(on(() => props.root, () => setText(props.notes), { defer: true }));
  return (
    <section class="project-section project-notes">
      <div class="project-section-head">
        <h3>Notes</h3>
        <span class="notes-state">
          {notesSaved() === "saved" ? "Saved" : notesSaved() === "saving" ? "Saving…" : "Not saved yet"}
        </span>
        <div class="segmented small" role="group" aria-label="Notes view">
          <button aria-pressed={!previewing()} onClick={() => setPreviewing(false)}>
            Write
          </button>
          <button aria-pressed={previewing()} onClick={() => (void flushNotes(), setPreviewing(true))}>
            Preview
          </button>
        </div>
      </div>
      <Show
        when={previewing()}
        fallback={
          <textarea
            class="notes-input"
            aria-label="Notes"
            placeholder="Notes in Markdown: plans, decisions, where you left off"
            spellcheck={true}
            value={text()}
            onInput={(event) => {
              setText(event.currentTarget.value);
              editNotes(props.root, event.currentTarget.value);
            }}
            onBlur={() => void flushNotes()}
          />
        }
      >
        <Show when={text().trim()} fallback={<p class="project-muted">No notes yet.</p>}>
          <Markdown source={text()} folder={props.root} class="notes-preview" />
        </Show>
      </Show>
    </section>
  );
}

interface LauncherButton {
  launcher: Launcher | "kestrel";
  label: string;
  icon: ProjectIconName;
  title: string;
}

function Launchers(props: { project: ProjectView }) {
  const available = () => detail()?.launchers;
  const buttons = (): LauncherButton[] => {
    const a = available();
    const list: LauncherButton[] = [];
    if (a?.vscode) list.push({ launcher: "vscode", label: "VS Code", icon: "code", title: "Open the folder in VS Code" });
    if (a?.solution)
      list.push({ launcher: "visual-studio", label: "Visual Studio", icon: "window", title: `Open ${a.solution} in Visual Studio` });
    list.push({ launcher: "terminal", label: "Terminal", icon: "terminal", title: "Open Windows Terminal in the folder" });
    if (a?.claude) list.push({ launcher: "claude", label: "Claude Code", icon: "spark", title: "Start Claude Code in a terminal in the folder" });
    list.push({ launcher: "kestrel", label: "Open here", icon: "folder", title: "Show the folder in Kestrel" });
    list.push({ launcher: "explorer", label: "Explorer", icon: "folder", title: "Show the folder in File Explorer" });
    // Git facts may arrive after the details; the page is offered as soon as they say where.
    if (props.project.git?.web_url)
      list.push({ launcher: "web", label: "Web page", icon: "external", title: props.project.git.web_url });
    return list;
  };
  return (
    <div class="project-launchers">
      <For each={buttons()}>
        {(button) => (
          <button
            class="launcher"
            data-launcher={button.launcher}
            title={button.title}
            onClick={() =>
              button.launcher === "kestrel" ? showInKestrel(props.project.root) : void launchProject(props.project.root, button.launcher)
            }
          >
            <ProjectIcon name={button.icon} size={14} />
            <span>{button.label}</span>
          </button>
        )}
      </For>
    </div>
  );
}

function Facts(props: { project: ProjectView }) {
  const p = () => props.project;
  // Languages under one percent would read as 0%.
  const languages = () => (detail()?.languages ?? p().languages).filter((language) => language.share >= 0.01);
  const reclaimable = () => detail()?.reclaimable_dirs ?? p().reclaimable_dirs;
  const relativeTo = (path: string) => path.slice(p().root.replace(/\\$/, "").length + 1) || path;
  return (
    <dl class="project-facts">
      <div class="fact" data-fact="stack">
        <dt>Stack</dt>
        <dd>
          <div class="project-stack">
            <For each={p().stack} fallback={<span class="project-muted">Unknown</span>}>
              {(item) => <span class="chip">{item}</span>}
            </For>
          </div>
          <Show when={languages().length > 0}>
            <div class="language-bar" aria-hidden="true">
              <For each={languages().slice(0, 6)}>
                {(language, i) => <span class={`language-${i()}`} style={{ width: `${Math.max(1, language.share * 100)}%` }} />}
              </For>
            </div>
            <div class="language-list">
              <For each={languages().slice(0, 6)}>
                {(language, i) => (
                  <span class="language">
                    <i class={`language-dot language-${i()}`} />
                    {language.name} {Math.round(language.share * 100)}%
                  </span>
                )}
              </For>
            </div>
          </Show>
        </dd>
      </div>
      <Show when={p().is_repo}>
        <div class="fact" data-fact="branch">
          <dt>Branch</dt>
          <dd>
            <Show when={p().git} fallback={<span class="project-muted">{p().git_error ?? "Reading git…"}</span>}>
              <span class="fact-strong">{branchName(p())}</span>
              <Show when={p().git?.branch.upstream}>
                {(upstream) => <span class="project-muted"> tracking {upstream()}</span>}
              </Show>
              <div class="project-sync">{describeSync(p())}</div>
            </Show>
          </dd>
        </div>
        <Show when={p().git}>
          <div class="fact" data-fact="changes">
            <dt>Changes</dt>
            <dd>{describeChanges(p())}</dd>
          </div>
          <div class="fact" data-fact="commit">
            <dt>Last commit</dt>
            <dd>
              <Show when={p().git?.last_commit} fallback={<span class="project-muted">No commits yet</span>}>
                {(commit) => (
                  <>
                    <div class="fact-strong commit-subject">{commit().subject}</div>
                    <div class="project-muted">
                      {commit().author}, {formatDate(commit().time)}
                    </div>
                  </>
                )}
              </Show>
            </dd>
          </div>
          <div class="fact" data-fact="remote">
            <dt>Remote</dt>
            <dd>
              <Show when={p().git?.remote} fallback={<span class="project-muted">No origin remote</span>}>
                {(remote) => (
                  <Show when={p().git?.web_url} fallback={<span class="remote-url">{remote()}</span>}>
                    {(url) => (
                      <button class="projects-link remote-link" title={remote()} onClick={() => void launchProject(p().root, "web")}>
                        {url().replace(/^https:\/\//, "")}
                      </button>
                    )}
                  </Show>
                )}
              </Show>
            </dd>
          </div>
        </Show>
      </Show>
      <div class="fact" data-fact="activity">
        <dt>Last activity</dt>
        <dd>{lastTouched(p()) ? `${formatDate(lastTouched(p()))} (${formatAgo(lastTouched(p()))})` : "None recorded"}</dd>
      </div>
      <div class="fact" data-fact="size">
        <dt>Size on disk</dt>
        <dd>{formatSize(p().size_on_disk)}</dd>
      </div>
      <div class="fact" data-fact="reclaimable">
        <dt>Reclaimable</dt>
        <dd>
          <Show when={p().reclaimable > 0} fallback={<span class="project-muted">No build output found</span>}>
            <span class="fact-strong">{formatSize(p().reclaimable)}</span>
            <span class="project-muted"> in build output</span>
            <ul class="reclaimable-list">
              <For each={reclaimable()}>
                {(dir) => (
                  <li>
                    <button class="projects-link" title={dir.path} onClick={() => showInKestrel(dir.path)}>
                      {relativeTo(dir.path)}
                    </button>
                    <span>{formatSize(dir.bytes)}</span>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </dd>
      </div>
    </dl>
  );
}

/** The selected project: what the hub knows about it, the user's notes, and ways to open it. */
export function ProjectDetailPanel(props: { root: string }) {
  const project = () => findProject(props.root);
  const loaded = () => {
    const shown = detail();
    return shown && shown.root.toLowerCase() === props.root.toLowerCase() ? shown : null;
  };
  return (
    <aside class="project-detail" aria-label="Project details" data-root={props.root}>
      <Show when={project()} fallback={<p class="project-muted">This project is no longer listed.</p>}>
        {(p) => (
          <>
            <header class="project-detail-head">
              <div class="project-detail-title">
                <h2>{p().name}</h2>
                <div class="project-path" title={p().root}>
                  {p().root}
                </div>
              </div>
              <button class="projects-close" aria-label="Close details" title="Close details" onClick={() => void selectProject(null)}>
                <ProjectIcon name="close" size={14} />
              </button>
            </header>

            <div class="project-controls">
              <label class="project-status-field">
                <span>Status</span>
                <select
                  class="project-status-select"
                  aria-label="Status"
                  value={p().status}
                  onChange={(event) => void setProjectStatus(p().root, event.currentTarget.value as ProjectStatus)}
                >
                  <For each={STATUSES}>{(status) => <option value={status.value}>{STATUS_LABELS[status.value]}</option>}</For>
                </select>
              </label>
              <button class="pin-toggle" aria-pressed={p().pinned} onClick={() => void setProjectPinned(p().root, !p().pinned)}>
                <ProjectIcon name="pin" size={13} />
                {p().pinned ? "Pinned" : "Pin to top"}
              </button>
              <button
                class="dismiss-button"
                title={p().dismissed ? "List this folder as a project again" : "Hide this folder from the projects hub"}
                onClick={() => void setProjectDismissed(p().root, !p().dismissed)}
              >
                {p().dismissed ? "Restore as a project" : "Not a project"}
              </button>
            </div>
            <TagEditor project={p()} />
            <Launchers project={p()} />

            <Show when={detailError()}>
              <p class="project-error">{detailError()}</p>
            </Show>

            <section class="project-section">
              <Facts project={p()} />
            </section>

            <Show when={loaded()} fallback={<p class="project-muted project-loading">Loading details…</p>}>
              {(d) => (
                <>
                  <NotesEditor root={p().root} notes={d().notes} />

                  <section class="project-section">
                    <div class="project-section-head">
                      <h3>Activity</h3>
                    </div>
                    <Heatmap start={d().activity.start} days={d().activity.days} />
                  </section>

                  <section class="project-section project-scripts">
                    <div class="project-section-head">
                      <h3>Scripts</h3>
                    </div>
                    <Show when={d().scripts.length > 0} fallback={<p class="project-muted">No scripts found in the project folder.</p>}>
                      <ul class="script-list">
                        <For each={d().scripts}>
                          {(script) => (
                            <li class="script" data-command={script.command}>
                              <span class="script-source">{script.source}</span>
                              <span class="script-name" title={script.command}>
                                {script.name}
                              </span>
                              <button
                                class="script-run"
                                aria-label={`Run ${script.command}`}
                                title={`Run "${script.command}" in a new terminal`}
                                onClick={() => void runScript(p().root, script)}
                              >
                                <ProjectIcon name="play" size={11} />
                                Run
                              </button>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </section>

                  <section class="project-section project-recent">
                    <div class="project-section-head">
                      <h3>Recently changed</h3>
                    </div>
                    <Show when={d().recent.length > 0} fallback={<p class="project-muted">No files yet.</p>}>
                      <ul class="recent-list">
                        <For each={d().recent}>
                          {(file) => (
                            <li>
                              <button class="projects-link recent-file" title={file.path} onClick={() => void revealFile(file.path)}>
                                {file.path.slice(p().root.replace(/\\$/, "").length + 1)}
                              </button>
                              <span class="project-muted">{formatAgo(file.mtime)}</span>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </section>

                  <section class="project-section project-readme">
                    <div class="project-section-head">
                      <h3>README</h3>
                      <Show when={d().readme}>
                        {(readme) => (
                          <button class="projects-link" onClick={() => void revealFile(readme().path)}>
                            Show file
                          </button>
                        )}
                      </Show>
                    </div>
                    <Show when={d().readme} fallback={<p class="project-muted">This project has no README.</p>}>
                      {(readme) => (
                        <>
                          <Markdown source={readme().text} folder={p().root} class="readme" />
                          <Show when={readme().truncated}>
                            <p class="project-muted">Only the start of the README is shown.</p>
                          </Show>
                        </>
                      )}
                    </Show>
                  </section>
                </>
              )}
            </Show>
          </>
        )}
      </Show>
    </aside>
  );
}
