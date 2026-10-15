import { Match, Switch } from "solid-js";

import type { GitChange } from "../lib/projectsApi";

const LABELS: Record<GitChange, string> = {
  modified: "Modified",
  added: "Added",
  untracked: "Untracked",
  conflicted: "Conflicted",
};

/**
 * A small mark beside an item's name in a repository: a dot for modified, a plus for added
 * (staged as new), a ring for untracked and a warning sign for conflicted. A folder shows the
 * most pressing change inside it.
 */
export function GitGlyph(props: { change: GitChange; dir: boolean }) {
  const label = () => (props.dir ? `${LABELS[props.change]} inside` : LABELS[props.change]);
  return (
    <svg
      class="git-glyph"
      classList={{ [props.change]: true }}
      width="12"
      height="12"
      viewBox="0 0 12 12"
      role="img"
      aria-label={label()}
      data-change={props.change}
    >
      <title>{label()}</title>
      <Switch>
        <Match when={props.change === "modified"}>
          <circle cx="6" cy="6" r="3" fill="currentColor" />
        </Match>
        <Match when={props.change === "added"}>
          <path d="M6 2.5v7M2.5 6h7" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" fill="none" />
        </Match>
        <Match when={props.change === "untracked"}>
          <circle cx="6" cy="6" r="3" stroke="currentColor" stroke-width="1.4" fill="none" />
        </Match>
        <Match when={props.change === "conflicted"}>
          <path d="M6 1.5 11 10.5H1Z" fill="currentColor" />
          <path d="M6 5v2.5M6 9v.01" stroke="var(--surface)" stroke-width="1.3" stroke-linecap="round" />
        </Match>
      </Switch>
    </svg>
  );
}
