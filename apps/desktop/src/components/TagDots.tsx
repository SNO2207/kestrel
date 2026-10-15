import { For, Show } from "solid-js";

import { tagsFor } from "../shortcuts";

/** The coloured dots of an item's tags, shown beside its name in every view. */
export function TagDots(props: { path: string | undefined }) {
  const list = () => tagsFor(props.path);
  const names = () => list().map((tag) => tag.name).join(", ");
  return (
    <Show when={list().length > 0}>
      <span class="tag-dots" title={names()} role="img" aria-label={`Tags: ${names()}`}>
        <For each={list()}>{(tag) => <span class="tag-dot" data-color={tag.color} data-tag={tag.name} />}</For>
      </span>
    </Show>
  );
}
