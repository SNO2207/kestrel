import { Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { formatCount } from "../lib/format";
import { addMoments, clearMoments } from "../lib/moments";
import { semanticApi } from "../lib/semanticApi";
import { AI_SEARCH, semanticState, watchSemantic } from "../semantic";
import { openTab } from "../state";
import { Icon } from "./Icon";

/** What a description query looks for, as the bar names it. */
function described(query: string): string {
  const tilde = query.match(/^\s*~\s*(.*)$/);
  if (tilde) return tilde[1].trim();
  const like = query.match(/(?:^|\s)like:(?:"([^"]*)"?|(\S+))/i);
  const text = (like?.[1] ?? like?.[2] ?? query).trim();
  // "Find similar" names a picture by its path: its name says enough.
  return /^([a-z]:\\|\\\\)/i.test(text) ? text.slice(text.lastIndexOf("\\") + 1) : text;
}

/**
 * Above the results of a description (`~dog`, `like:"a dog"`, Find similar): what was looked
 * for, how many were found, whether describing is still under way (results may then be
 * missing some photos), and the relevance slider, from fewer (only close matches) to more.
 */
export function DescribeBar(props: {
  query: string;
  /** The results' set, for where in each video they matched. */
  set: number | null;
  total: number;
  /** Runs the search again, as the slider moves. */
  onRefresh: () => void;
}) {
  onMount(watchSemantic);
  // The moments are the results' own; they go with them.
  onCleanup(clearMoments);
  createEffect(
    on(
      () => props.set,
      (set) => {
        if (set !== null) void semanticApi.moments(set).then(addMoments, () => {});
      },
    ),
  );
  const [cutoff, setCutoff] = createSignal<number | null>(null);
  const shown = () => cutoff() ?? semanticState()?.cutoff ?? 0.5;
  let timer: number | undefined;
  const slide = (value: number) => {
    setCutoff(value);
    clearTimeout(timer);
    timer = window.setTimeout(() => {
      void semanticApi.setCutoff(value).then(() => props.onRefresh());
    }, 180);
  };
  onCleanup(() => clearTimeout(timer));
  const indexing = () => semanticState()?.indexing;
  const noModel = () => semanticState() !== null && semanticState()?.active === null;

  return (
    <div class="describe-bar" role="region" aria-label="Describe">
      <Icon name="sparkle" size={14} />
      <span class="describe-what">
        Looks like <b>{described(props.query)}</b>
      </span>
      <Show when={!noModel()}>
        <span class="describe-count">{props.total === 1 ? "1 found" : `${formatCount(props.total)} found`}</span>
      </Show>
      <Show when={indexing()?.state === "working"}>
        <span class="describe-progress" title="Photos and videos not described yet can't be found by description">
          Still describing: {formatCount(indexing()?.done ?? 0)} of {formatCount((indexing()?.done ?? 0) + (indexing()?.queued ?? 0))}
        </span>
      </Show>
      <span class="describe-spacer" />
      <Show
        when={!noModel()}
        fallback={
          <button class="describe-setup" onClick={() => openTab(AI_SEARCH)}>
            Set up AI search
          </button>
        }
      >
        <label class="describe-cutoff" title="How alike a photo must look to be shown">
          <span>Fewer</span>
          <input
            type="range"
            min="0"
            max="100"
            step="1"
            aria-label="Relevance: fewer or more results"
            value={Math.round(shown() * 100)}
            onInput={(event) => slide(Number(event.currentTarget.value) / 100)}
          />
          <span>More</span>
        </label>
        <button class="describe-settings" title="Models, folders and progress" onClick={() => openTab(AI_SEARCH)}>
          AI search
        </button>
      </Show>
    </div>
  );
}
