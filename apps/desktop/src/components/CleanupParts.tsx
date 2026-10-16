import { For, type JSX, Show, onMount } from "solid-js";

import { baseName, formatSize, parentPath } from "../lib/format";
import type { RecycleReport, Skipped, Tier } from "../lib/cleanupApi";

/**
 * Pieces the cleanup and duplicates panels share: their icons, the confirmation and result
 * dialogs, and the banner that reports what happened, with Undo.
 */

const PATHS = {
  sweep: "M11.5 2 8.5 7M4.5 7h6l1.5 6.5h-9ZM6.5 10v3.5M9 10v3.5",
  copies: "M5.5 5.5h7v8h-7ZM3.5 10.5v-8h7",
  info: "M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11ZM8 7.5v3.5M8 5.2h.01",
  terminal: "M2.5 3.5h11v9h-11ZM5 6.5l2 1.5-2 1.5M8.5 10h3",
  chevron: "M6 3.5 10.5 8 6 12.5",
  close: "M4 4l8 8M12 4l-8 8",
  undo: "M5.5 4 2.5 7l3 3M2.5 7h7a3.5 3.5 0 0 1 0 7H7",
  external: "M9 2.5h4.5V7M13.5 2.5 7.5 8.5M11.5 9.5v4h-9v-9h4",
  folder: "M2.5 4.5h4L8 6h5.5v6.5h-11Z",
  file: "M4 2.5h5l3 3v8H4ZM9 2.5v3h3",
  check: "M3.5 8.5 6.5 11.5 12.5 4.5",
} as const;

export type CleanupIconName = keyof typeof PATHS;

/** Line icons for the cleanup page, drawn like the app's own `Icon`. */
export function CleanupIcon(props: { name: CleanupIconName; size?: number }) {
  return (
    <svg
      class="icon"
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[props.name]} />
    </svg>
  );
}

export const TIER_LABELS: Record<Tier, string> = { safe: "Safe", review: "Review", info: "Info" };

export function TierChip(props: { tier: Tier }) {
  return <span class={`cleanup-tier-chip tier-${props.tier}`}>{TIER_LABELS[props.tier]}</span>;
}

/** Text with `backticked` commands shown as code. */
export function WithCode(props: { text: string }) {
  return (
    <For each={props.text.split("`")}>{(part, i) => (i() % 2 === 1 ? <code>{part}</code> : part)}</For>
  );
}

export const itemCount = (n: number) => (n === 1 ? "1 item" : `${n.toLocaleString()} items`);

/** What a confirmation says about sending `n` items of `bytes` to the Recycle Bin. */
export function toRecycleBin(n: number, bytes: number): string {
  const them = n === 1 ? "it" : "them";
  return `This moves ${itemCount(n)} (${formatSize(bytes)}) to the Recycle Bin. You can restore ${them} from there, or with Undo.`;
}

/**
 * A path as its name, then its folder in quieter text. A long folder loses its start, never
 * the name or the folders nearest to it.
 */
export function PathLabel(props: { path: string; class?: string }) {
  return (
    <span class={`cleanup-path ${props.class ?? ""}`} title={props.path} data-path={props.path}>
      <span class="cleanup-path-name">{baseName(props.path)}</span>
      <span class="cleanup-path-folder">
        <bdi>{parentPath(props.path) ?? ""}</bdi>
      </span>
    </span>
  );
}

export interface ListedItem {
  path: string;
  bytes?: number;
  note?: string;
}

/** A list of paths in a dialog, with sizes or notes beside them. */
export function PathList(props: { items: ListedItem[]; label: string }) {
  return (
    <ul class="cleanup-dialog-list" aria-label={props.label}>
      <For each={props.items}>
        {(item) => (
          <li>
            <PathLabel path={item.path} class="cleanup-dialog-path" />
            <Show when={item.note} fallback={<span class="cleanup-dialog-size">{item.bytes !== undefined ? formatSize(item.bytes) : ""}</span>}>
              <span class="cleanup-dialog-note">{item.note}</span>
            </Show>
          </li>
        )}
      </For>
    </ul>
  );
}

/** A dialog over the page: a title, what it says, and its buttons. Escape cancels. */
export function CleanupDialog(props: {
  title: string;
  children: JSX.Element;
  confirm?: string;
  cancel?: string;
  busy?: boolean;
  onConfirm?: () => void;
  onCancel: () => void;
}) {
  let confirmButton: HTMLButtonElement | undefined;
  let cancelButton: HTMLButtonElement | undefined;
  onMount(() => (confirmButton ?? cancelButton)?.focus());
  return (
    <div
      class="dialog-backdrop cleanup-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !props.busy) props.onCancel();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !props.busy) {
          event.preventDefault();
          props.onCancel();
        }
      }}
    >
      <div class="dialog cleanup-dialog" role="dialog" aria-modal="true" aria-label={props.title}>
        <h2 class="dialog-title">{props.title}</h2>
        <div class="cleanup-dialog-body">{props.children}</div>
        <footer class="dialog-footer">
          <span class="cleanup-spacer" />
          <button ref={cancelButton} class="dialog-button" disabled={props.busy} onClick={() => props.onCancel()}>
            {props.cancel ?? "Cancel"}
          </button>
          <Show when={props.confirm}>
            <button ref={confirmButton} class="dialog-button primary" disabled={props.busy} onClick={() => props.onConfirm?.()}>
              {props.confirm}
            </button>
          </Show>
        </footer>
      </div>
    </div>
  );
}

/** What a dry run found: what would go to the Recycle Bin and what would be skipped. */
export function DryRunDialog(props: { report: RecycleReport; sizes: Map<string, number>; onClose: () => void }) {
  const size = (path: string) => props.sizes.get(path.toLowerCase());
  return (
    <CleanupDialog title="Dry run" cancel="Close" onCancel={props.onClose}>
      <p class="cleanup-dialog-lead">{props.report.message}</p>
      <Show when={props.report.recycled.length > 0}>
        <h3 class="cleanup-dialog-heading">
          Would go to the Recycle Bin ({itemCount(props.report.recycled.length)}, {formatSize(props.report.bytes)})
        </h3>
        <PathList label="Would go to the Recycle Bin" items={props.report.recycled.map((path) => ({ path, bytes: size(path) }))} />
      </Show>
      <SkippedList skipped={props.report.skipped} heading="Would be skipped" />
    </CleanupDialog>
  );
}

export function SkippedList(props: { skipped: Skipped[]; heading: string }) {
  return (
    <Show when={props.skipped.length > 0}>
      <h3 class="cleanup-dialog-heading">
        {props.heading} ({itemCount(props.skipped.length)})
      </h3>
      <PathList label={props.heading} items={props.skipped.map((s) => ({ path: s.path, note: s.reason }))} />
    </Show>
  );
}

export interface Outcome {
  message: string;
  skipped: Skipped[];
  /** Something went to the Recycle Bin, so Undo can bring it back. */
  undoable: boolean;
  tone: "done" | "error";
}

/** The banner saying what an action did, with Undo when it can be undone. */
export function OutcomeBanner(props: { outcome: Outcome; busy: boolean; onUndo: () => void; onShowSkipped: () => void; onDismiss: () => void }) {
  return (
    <div class="cleanup-outcome" classList={{ error: props.outcome.tone === "error" }} role="status">
      <CleanupIcon name={props.outcome.tone === "error" ? "info" : "check"} />
      <span class="cleanup-outcome-text">{props.outcome.message}</span>
      <Show when={props.outcome.skipped.length > 0}>
        <button class="cleanup-link" onClick={() => props.onShowSkipped()}>
          Why skipped
        </button>
      </Show>
      <Show when={props.outcome.undoable}>
        <button class="cleanup-button cleanup-undo" disabled={props.busy} onClick={() => props.onUndo()}>
          <CleanupIcon name="undo" size={14} />
          Undo
        </button>
      </Show>
      <button class="cleanup-icon-button" aria-label="Dismiss" title="Dismiss" onClick={() => props.onDismiss()}>
        <CleanupIcon name="close" size={14} />
      </button>
    </div>
  );
}

/** The outcome of sending items to the Recycle Bin. */
export function outcomeOf(report: RecycleReport): Outcome {
  return {
    message: report.message,
    skipped: report.skipped,
    undoable: report.recycled.length > 0,
    tone: report.recycled.length > 0 || report.skipped.length === 0 ? "done" : "error",
  };
}
