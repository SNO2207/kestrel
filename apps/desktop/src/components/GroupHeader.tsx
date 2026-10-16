import { Show, type JSX } from "solid-js";

import { formatCount } from "../lib/format";
import type { GroupHeader as GroupInfo } from "../lib/layoutApi";

/**
 * A group's header in a grouped list: its name and size, a rule, and a chevron that collapses
 * the group to the header. Clicking the header selects the group's items, as in Explorer; its
 * menu collapses or expands one group or all of them. The name arrives from the engine once
 * the group is in view (`group_headers`).
 */
export function GroupHeader(props: {
  index: number;
  header: GroupInfo | undefined;
  count: number;
  collapsed: boolean;
  /** The list view's header, in the band above its group's columns. */
  across: boolean;
  style: JSX.CSSProperties;
  onToggle: () => void;
  onSelect: (event: MouseEvent) => void;
  onMenu: (event: MouseEvent) => void;
}) {
  return (
    <div
      class="group-header"
      classList={{ collapsed: props.collapsed, across: props.across }}
      style={props.style}
      role="rowgroup"
      aria-label={props.header?.label}
      data-group={props.index}
      onMouseDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        props.onSelect(event);
      }}
      onContextMenu={(event) => props.onMenu(event)}
    >
      <button
        class="group-toggle"
        aria-expanded={!props.collapsed}
        aria-label={props.collapsed ? "Expand group" : "Collapse group"}
        title={props.collapsed ? "Expand group" : "Collapse group"}
        onMouseDown={(event) => {
          event.stopPropagation();
          event.preventDefault();
        }}
        onClick={() => props.onToggle()}
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M2.5 3.75 5 6.25l2.5-2.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
      </button>
      <span class="group-label">{props.header?.label ?? ""}</span>
      <Show when={props.header}>
        <span class="group-count">{formatCount(props.count)}</span>
      </Show>
      <span class="group-rule" />
    </div>
  );
}
