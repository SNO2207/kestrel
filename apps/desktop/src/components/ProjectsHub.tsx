import { For, Match, Show, Switch, createEffect, createMemo, on, onCleanup, onMount } from "solid-js";

import { formatSize } from "../lib/format";
import { STATUS_LABELS, branchName, describeChanges, describeSync, formatAgo } from "../lib/projectFormat";
import type { ProjectView } from "../lib/projectsApi";
import {
  STATUSES,
  clearFilters,
  closeProjects,
  dismissedCount,
  filters,
  lastTouched,
  layout,
  projectsOpen,
  projectsState,
  selectProject,
  selected,
  setFilter,
  setLayout,
  setProjectDismissed,
  setShowDismissed,
  setSortBy,
  showDismissed,
  sortBy,
  stackChoices,
  tagChoices,
  toggleProjects,
  visibleProjects,
  type SortBy,
} from "../projects";
import { activeId, activeTab } from "../state";
import { ProjectDetailPanel } from "./ProjectDetail";
import { ProjectIcon } from "./ProjectIcon";

const SORTS: { value: SortBy; label: string }[] = [
  { value: "activity", label: "Last activity" },
  { value: "name", label: "Name" },
  { value: "size", label: "Size" },
];

function StatusPill(props: { status: ProjectView["status"] }) {
  return <span class={`project-status status-${props.status}`}>{STATUS_LABELS[props.status]}</span>;
}

/** The git line of a card or row: branch, where it stands, and uncommitted changes. */
function GitSummary(props: { project: ProjectView }) {
  return (
    <Switch>
      <Match when={props.project.git_pending}>
        <span class="project-git-pending">Reading git…</span>
      </Match>
      <Match when={props.project.git_error && !props.project.git}>
        <span class="project-git-error" title={props.project.git_error ?? ""}>
          {props.project.git_error}
        </span>
      </Match>
      <Match when={props.project.git}>
        <span class="project-branch">
          <ProjectIcon name="branch" size={13} />
          {branchName(props.project)}
        </span>
        <span class="project-sync" classList={{ attention: (props.project.git?.branch.ahead ?? 0) > 0 }}>
          {describeSync(props.project)}
        </span>
      </Match>
    </Switch>
  );
}

function ProjectCard(props: { project: ProjectView }) {
  const p = () => props.project;
  const commit = () => p().git?.last_commit;
  return (
    <article
      class="project-card"
      classList={{ selected: selected()?.toLowerCase() === p().root.toLowerCase(), pinned: p().pinned }}
      data-root={p().root}
      tabindex="0"
      aria-label={p().name}
      onClick={() => void selectProject(p().root)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          void selectProject(p().root);
        }
      }}
    >
      <header class="project-card-head">
        <span class="project-name">{p().name}</span>
        <Show when={p().pinned}>
          <span class="project-pin" title="Pinned">
            <ProjectIcon name="pin" size={13} />
          </span>
        </Show>
        <StatusPill status={p().status} />
      </header>
      <div class="project-path" title={p().root}>
        {p().root}
      </div>
      <Show when={p().stack.length > 0}>
        <div class="project-stack">
          <For each={p().stack}>{(item) => <span class="chip">{item}</span>}</For>
        </div>
      </Show>
      <Show when={p().is_repo}>
        <div class="project-git">
          <GitSummary project={p()} />
        </div>
        <Show when={p().git}>
          <div
            class="project-changes"
            classList={{ dirty: describeChanges(p()) !== "No changes", conflicted: (p().git?.counts.conflicted ?? 0) > 0 }}
          >
            {describeChanges(p())}
          </div>
        </Show>
        <Show when={commit()}>
          {(last) => (
            <div class="project-commit" title={`${last().subject}\n${last().author}, ${new Date(last().time).toLocaleString()}`}>
              <span class="project-commit-subject">{last().subject}</span>
              <span class="project-commit-by">
                {last().author}, {formatAgo(last().time)}
              </span>
            </div>
          )}
        </Show>
      </Show>
      <footer class="project-meta">
        <span class="project-activity" title={lastTouched(p()) ? new Date(lastTouched(p())).toLocaleString() : undefined}>
          {lastTouched(p()) ? `Active ${formatAgo(lastTouched(p()))}` : "No activity"}
        </span>
        <span class="project-size">{formatSize(p().size_on_disk)}</span>
        <Show when={p().reclaimable > 0}>
          <span class="project-reclaimable" title="Build output that could be deleted and rebuilt">
            {formatSize(p().reclaimable)} reclaimable
          </span>
        </Show>
      </footer>
      <Show when={p().tags.length > 0}>
        <div class="project-tags">
          <For each={p().tags}>{(tag) => <span class="tag">{tag}</span>}</For>
        </div>
      </Show>
      <Show when={p().dismissed}>
        <button
          class="project-restore"
          onClick={(event) => {
            event.stopPropagation();
            void setProjectDismissed(p().root, false);
          }}
        >
          Restore as a project
        </button>
      </Show>
    </article>
  );
}

function ProjectTable(props: { projects: ProjectView[] }) {
  return (
    <table class="project-table">
      <thead>
        <tr>
          <th>Name</th>
          <th class="col-status">Status</th>
          <th class="col-stack">Stack</th>
          <th>Branch</th>
          <th>Changes</th>
          <th class="col-activity">Last activity</th>
          <th class="numeric col-size">Size</th>
          <th class="numeric col-reclaimable">Reclaimable</th>
        </tr>
      </thead>
      <tbody>
        <For each={props.projects}>
          {(p) => (
            <tr
              class="project-row"
              classList={{ selected: selected()?.toLowerCase() === p.root.toLowerCase() }}
              data-root={p.root}
              tabindex="0"
              onClick={() => void selectProject(p.root)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void selectProject(p.root);
              }}
            >
              <td>
                <div class="project-name">
                  <Show when={p.pinned}>
                    <span class="project-pin" title="Pinned">
                      <ProjectIcon name="pin" size={12} />
                    </span>
                  </Show>
                  {p.name}
                </div>
                <div class="project-path" title={p.root}>
                  {p.root}
                </div>
              </td>
              <td class="col-status">
                <StatusPill status={p.status} />
              </td>
              <td class="col-stack">{p.stack.slice(0, 3).join(", ")}</td>
              <td>
                <Show when={p.is_repo}>
                  <div class="project-git">
                    <GitSummary project={p} />
                  </div>
                </Show>
              </td>
              <td
                class="project-changes"
                classList={{ dirty: !!p.git && describeChanges(p) !== "No changes", conflicted: (p.git?.counts.conflicted ?? 0) > 0 }}
              >
                {p.git ? describeChanges(p) : ""}
              </td>
              <td class="col-activity project-activity">{formatAgo(lastTouched(p))}</td>
              <td class="numeric col-size project-size">{formatSize(p.size_on_disk)}</td>
              <td class="numeric col-reclaimable project-reclaimable">{p.reclaimable > 0 ? formatSize(p.reclaimable) : ""}</td>
            </tr>
          )}
        </For>
      </tbody>
    </table>
  );
}

/**
 * The projects hub: every project on the drive as cards or a table, with filters, and the
 * selected project's details beside them. It covers the file list while open; going anywhere
 * else (another place, folder or tab) closes it.
 */
export function ProjectsHub() {
  let root: HTMLElement | undefined;
  let filterInput: HTMLInputElement | undefined;

  onMount(() => {
    // Ctrl+Shift+P opens and closes the hub from anywhere.
    const onKey = (event: KeyboardEvent) => {
      if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === "p") {
        event.preventDefault();
        toggleProjects();
      }
    };
    window.addEventListener("keydown", onKey);
    // Registered after Solid's own delegated listener on the document, so the hub's handlers
    // have run by the time this keeps the event from the window's.
    document.addEventListener("keydown", guardKeys);
    onCleanup(() => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("keydown", guardKeys);
    });
  });

  // Leaving for a folder or another tab closes the hub.
  const where = createMemo(() => `${activeId()}|${activeTab()?.path ?? ""}|${activeTab()?.query ?? ""}`);
  createEffect(on(where, () => projectsOpen() && closeProjects(), { defer: true }));

  createEffect(
    on(projectsOpen, (open) => {
      if (open) queueMicrotask(() => filterInput?.focus());
    }),
  );

  /**
   * Keys pressed in the hub are the hub's: the window's file shortcuts (Backspace for back,
   * Ctrl+Z for undo…) must not act on the folder hidden underneath, least of all while typing
   * notes. Only tab switching and the hub's own shortcut get through.
   */
  const guardKeys = (event: KeyboardEvent) => {
    if (!root || !(event.target instanceof Node) || !root.contains(event.target)) return;
    const key = event.key.toLowerCase();
    if (key === "escape") {
      event.preventDefault();
      event.stopPropagation();
      const editing = event.target instanceof HTMLTextAreaElement;
      if (editing) (event.target as HTMLElement).blur();
      else if (selected()) void selectProject(null);
      else closeProjects();
      return;
    }
    if (event.ctrlKey && event.shiftKey && key === "p") return;
    if (event.ctrlKey && (key === "t" || key === "w" || key === "tab")) return;
    event.stopPropagation();
  };

  const anyFilter = () =>
    filters.text !== "" || filters.status !== "" || filters.stack !== "" || filters.tag !== "" || filters.changes || filters.unpushed || filters.untouched;
  const count = () => visibleProjects().length;

  return (
    <Show when={projectsOpen()}>
      <section
        ref={root}
        class="projects-hub"
        classList={{ "with-detail": selected() !== null }}
        aria-label="Projects"
        tabindex="-1"
      >
        <header class="projects-head">
          <div class="projects-title-row">
            <ProjectIcon name="projects" size={18} />
            <h1>{showDismissed() ? "Dismissed folders" : "Projects"}</h1>
            <span class="projects-count">
              <Show when={projectsState.loaded} fallback="Looking for projects…">
                {count()} {count() === 1 ? "project" : "projects"}
                <Show when={projectsState.indexing}> · the index is still being built</Show>
              </Show>
            </span>
            <span class="projects-spacer" />
            <div class="segmented" role="group" aria-label="Layout">
              <button aria-pressed={layout() === "cards"} title="Cards" onClick={() => setLayout("cards")}>
                <ProjectIcon name="cards" size={14} />
                <span>Cards</span>
              </button>
              <button aria-pressed={layout() === "table"} title="Table" onClick={() => setLayout("table")}>
                <ProjectIcon name="table" size={14} />
                <span>Table</span>
              </button>
            </div>
            <label class="projects-sort">
              <span>Sort by</span>
              <select aria-label="Sort by" value={sortBy()} onChange={(event) => setSortBy(event.currentTarget.value as SortBy)}>
                <For each={SORTS}>{(sort) => <option value={sort.value}>{sort.label}</option>}</For>
              </select>
            </label>
            <button class="projects-close" aria-label="Close projects" title="Close (Esc)" onClick={closeProjects}>
              <ProjectIcon name="close" size={14} />
            </button>
          </div>
          <div class="projects-filters">
            <input
              ref={filterInput}
              class="projects-filter"
              aria-label="Filter projects"
              placeholder="Filter by name, path, tag or stack"
              spellcheck={false}
              value={filters.text}
              onInput={(event) => setFilter("text", event.currentTarget.value)}
            />
            <select aria-label="Status" value={filters.status} onChange={(event) => setFilter("status", event.currentTarget.value as never)}>
              <option value="">All statuses</option>
              <For each={STATUSES}>{(status) => <option value={status.value}>{status.label}</option>}</For>
            </select>
            <select aria-label="Stack" value={filters.stack} onChange={(event) => setFilter("stack", event.currentTarget.value)}>
              <option value="">All stacks</option>
              <For each={stackChoices()}>{(stack) => <option value={stack}>{stack}</option>}</For>
            </select>
            <select aria-label="Tag" value={filters.tag} onChange={(event) => setFilter("tag", event.currentTarget.value)}>
              <option value="">All tags</option>
              <For each={tagChoices()}>{(tag) => <option value={tag}>{tag}</option>}</For>
            </select>
            <button class="filter-chip" aria-pressed={filters.changes} onClick={() => setFilter("changes", !filters.changes)}>
              Has changes
            </button>
            <button class="filter-chip" aria-pressed={filters.unpushed} onClick={() => setFilter("unpushed", !filters.unpushed)}>
              Not pushed
            </button>
            <button class="filter-chip" aria-pressed={filters.untouched} onClick={() => setFilter("untouched", !filters.untouched)}>
              Untouched for 90 days
            </button>
            <Show when={anyFilter()}>
              <button class="projects-clear" onClick={clearFilters}>
                Clear filters
              </button>
            </Show>
          </div>
        </header>

        <div class="projects-body">
          <div class="projects-list">
            <Show
              when={count() > 0}
              fallback={
                <div class="projects-empty">
                  <Show
                    when={projectsState.loaded}
                    fallback={<p>Looking for projects in the index…</p>}
                  >
                    <Show
                      when={projectsState.projects.length > 0 || showDismissed()}
                      fallback={
                        <>
                          <p>No projects found yet.</p>
                          <p class="hint">
                            Folders with <code>.git</code>, <code>Cargo.toml</code>, <code>package.json</code> and other build files
                            appear here.
                          </p>
                        </>
                      }
                    >
                      <p>{showDismissed() ? "No dismissed folders." : "No projects match these filters."}</p>
                    </Show>
                  </Show>
                </div>
              }
            >
              <Show when={layout() === "cards"} fallback={<ProjectTable projects={visibleProjects()} />}>
                <div class="project-cards">
                  <For each={visibleProjects()}>{(project) => <ProjectCard project={project} />}</For>
                </div>
              </Show>
            </Show>
            <Show when={dismissedCount() > 0 || showDismissed()}>
              <div class="projects-footer">
                <button class="projects-link" onClick={() => setShowDismissed(!showDismissed())}>
                  {showDismissed() ? "Back to projects" : `Show dismissed folders (${dismissedCount()})`}
                </button>
              </div>
            </Show>
          </div>
          <Show when={selected()}>{(root) => <ProjectDetailPanel root={root()} />}</Show>
        </div>
      </section>
    </Show>
  );
}
