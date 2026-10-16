import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on } from "solid-js";

import { showFolder } from "../disk";
import { RECYCLE_BIN } from "../places";
import { api } from "../lib/api";
import { cleanupApi, type CleanupTool, type Recommendation, type RecycleReport, type ScanReport, type Tier, type ToolReport } from "../lib/cleanupApi";
import { formatSize } from "../lib/format";
import { formatAgo } from "../lib/projectFormat";
import {
  CleanupDialog,
  CleanupIcon,
  DryRunDialog,
  type Outcome,
  OutcomeBanner,
  PathLabel,
  PathList,
  SkippedList,
  TierChip,
  WithCode,
  itemCount,
  outcomeOf,
  toRecycleBin,
} from "./CleanupParts";

/** Items listed under a recommendation; the rest are counted, and ticked with it. */
const SHOWN_ITEMS = 200;

const TIERS: { tier: Tier; title: string; hint: string }[] = [
  { tier: "safe", title: "Safe to remove", hint: "Caches and temporary files, made again when they're needed." },
  { tier: "review", title: "Review before removing", hint: "Things you may still want. Look through them first." },
  { tier: "info", title: "Worth knowing", hint: "Shown only: Kestrel never removes these. The advice says how." },
];

const lower = (path: string) => path.toLowerCase();
const isRecycle = (rec: Recommendation) => rec.action.kind === "recycle" && rec.tier !== "info";
const toolOf = (rec: Recommendation): CleanupTool | null => (rec.action.kind === "tool" ? rec.action : null);

type Dialog =
  | { kind: "recycle" }
  | { kind: "tool"; rec: Recommendation }
  | { kind: "tool-output"; report: ToolReport }
  | { kind: "dry-run"; report: RecycleReport }
  | { kind: "skipped" };

/**
 * Cleanup recommendations for `root` (a folder, or `null` for every drive): what could go and
 * why, grouped by how sure you can be. Nothing is removed until you tick items and confirm;
 * they go to the Recycle Bin, and Undo brings them back. Tools such as `cargo clean` run only
 * when you click them, after a confirmation that says they can't be undone.
 */
export function CleanupPanel(props: { root: string | null }) {
  const [report, setReport] = createSignal<ScanReport | null>(null);
  const [scanning, setScanning] = createSignal(false);
  const [failed, setFailed] = createSignal<string | null>(null);
  /** Ticked items, by recommendation id, as lower-cased paths. */
  const [ticked, setTicked] = createSignal<Map<string, Set<string>>>(new Map());
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
  const [dialog, setDialog] = createSignal<Dialog | null>(null);
  const [outcome, setOutcome] = createSignal<Outcome | null>(null);
  const [busy, setBusy] = createSignal(false);
  let scanRequest = 0;

  async function scan() {
    const request = ++scanRequest;
    setScanning(true);
    setFailed(null);
    try {
      const next = await cleanupApi.scan(props.root);
      if (request !== scanRequest) return;
      setReport(next);
      // Ticks stay on what is still offered.
      const offered = new Map(next.recommendations.map((rec) => [rec.id, new Set(rec.items.map((item) => lower(item.path)))]));
      setTicked((current) => {
        const kept = new Map<string, Set<string>>();
        for (const [id, paths] of current) {
          const still = offered.get(id);
          const left = still ? new Set([...paths].filter((path) => still.has(path))) : new Set<string>();
          if (left.size > 0) kept.set(id, left);
        }
        return kept;
      });
    } catch (error) {
      if (request === scanRequest) setFailed(String(error));
    } finally {
      if (request === scanRequest) setScanning(false);
    }
  }

  createEffect(on(() => props.root, () => void scan()));

  const recs = () => report()?.recommendations ?? [];
  const byTier = (tier: Tier) => recs().filter((rec) => rec.tier === tier);
  const tickedOf = (rec: Recommendation) => ticked().get(rec.id) ?? new Set<string>();
  const isTicked = (rec: Recommendation, path: string) => tickedOf(rec).has(lower(path));

  /** Ticked items, with the recommendation each is in. */
  const selection = createMemo(() => {
    const chosen: { rec: Recommendation; items: Recommendation["items"] }[] = [];
    for (const rec of recs()) {
      const paths = ticked().get(rec.id);
      if (!paths || paths.size === 0 || !isRecycle(rec)) continue;
      chosen.push({ rec, items: rec.items.filter((item) => paths.has(lower(item.path))) });
    }
    return chosen;
  });
  const selectedCount = () => selection().reduce((n, each) => n + each.items.length, 0);
  const selectedBytes = () => selection().reduce((n, each) => n + each.items.reduce((m, item) => m + item.bytes, 0), 0);
  const sizes = () => new Map(selection().flatMap((each) => each.items.map((item) => [lower(item.path), item.bytes] as const)));

  function setRecTicked(rec: Recommendation, on: boolean) {
    setTicked((current) => {
      const next = new Map(current);
      if (on) next.set(rec.id, new Set(rec.items.map((item) => lower(item.path))));
      else next.delete(rec.id);
      return next;
    });
  }

  function setItemTicked(rec: Recommendation, path: string, on: boolean) {
    setTicked((current) => {
      const next = new Map(current);
      const paths = new Set(next.get(rec.id) ?? []);
      if (on) paths.add(lower(path));
      else paths.delete(lower(path));
      if (paths.size > 0) next.set(rec.id, paths);
      else next.delete(rec.id);
      return next;
    });
  }

  const toggleExpanded = (rec: Recommendation) =>
    setExpanded((current) => {
      const next = new Set(current);
      if (!next.delete(rec.id)) next.add(rec.id);
      return next;
    });

  const chosen = () => selection().map((each) => ({ id: each.rec.id, paths: each.items.map((item) => item.path) }));

  async function dryRun() {
    setBusy(true);
    try {
      setDialog({ kind: "dry-run", report: await cleanupApi.recycle(chosen(), true) });
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
    }
  }

  async function recycle() {
    setBusy(true);
    try {
      const done = await cleanupApi.recycle(chosen(), false);
      setOutcome(outcomeOf(done));
      setTicked(new Map());
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
      setDialog(null);
    }
    await scan();
  }

  async function runTool(rec: Recommendation) {
    setBusy(true);
    try {
      setDialog({ kind: "tool-output", report: await cleanupApi.runTool(rec.id, false) });
    } catch (error) {
      setDialog(null);
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
    }
    await scan();
  }

  async function undo() {
    setBusy(true);
    try {
      const report = await api.undo();
      setOutcome({ message: report.message, skipped: [], undoable: false, tone: "done" });
    } catch (error) {
      setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" });
    } finally {
      setBusy(false);
    }
    await scan();
  }

  function follow(target: string) {
    if (target === RECYCLE_BIN) return showFolder(RECYCLE_BIN);
    void cleanupApi.open(target).catch((error) => setOutcome({ message: String(error), skipped: [], undoable: false, tone: "error" }));
  }

  return (
    <section class="cleanup-panel" aria-label="Cleanup recommendations" aria-busy={scanning()}>
      <header class="cleanup-summary">
        <div class="cleanup-total">
          <span class="cleanup-total-size">{report() ? formatSize(report()!.total) : "…"}</span>
          <span class="cleanup-total-label">could be freed</span>
        </div>
        <span class="cleanup-meta">
          <Show when={!scanning()} fallback="Looking through the index…">
            <Show when={report()}>
              {(r) => (
                <>
                  {r().recommendations.length === 1 ? "1 recommendation" : `${r().recommendations.length} recommendations`}
                  <Show when={r().indexing}> · the index is still being built</Show>
                </>
              )}
            </Show>
          </Show>
        </span>
        <span class="cleanup-spacer" />
        <button class="cleanup-button cleanup-rescan" disabled={scanning()} onClick={() => void scan()}>
          Scan again
        </button>
      </header>

      <Show when={outcome()}>
        {(shown) => (
          <OutcomeBanner
            outcome={shown()}
            busy={busy()}
            onUndo={() => void undo()}
            onShowSkipped={() => setDialog({ kind: "skipped" })}
            onDismiss={() => setOutcome(null)}
          />
        )}
      </Show>

      <div class="cleanup-scroll">
        <Switch>
          <Match when={failed()}>
            <p class="cleanup-empty">{failed()}</p>
          </Match>
          <Match when={report() && recs().length === 0}>
            <div class="cleanup-empty">
              <CleanupIcon name="check" size={20} />
              <p>Nothing to clean up {props.root ? "here" : "right now"}.</p>
            </div>
          </Match>
          <Match when={report()}>
            <For each={TIERS.filter((t) => byTier(t.tier).length > 0)}>
              {(section) => (
                <section class="cleanup-tier" data-tier={section.tier}>
                  <header class="cleanup-tier-head">
                    <h2>{section.title}</h2>
                    <span class="cleanup-tier-hint">{section.hint}</span>
                  </header>
                  <For each={byTier(section.tier)}>
                    {(rec) => (
                      <RecommendationCard
                        rec={rec}
                        ticked={tickedOf(rec)}
                        expanded={expanded().has(rec.id)}
                        onTick={(on) => setRecTicked(rec, on)}
                        onTickItem={(path, on) => setItemTicked(rec, path, on)}
                        isTicked={(path) => isTicked(rec, path)}
                        onExpand={() => toggleExpanded(rec)}
                        onRunTool={() => setDialog({ kind: "tool", rec })}
                        onLink={follow}
                      />
                    )}
                  </For>
                </section>
              )}
            </For>
          </Match>
        </Switch>
      </div>

      <footer class="cleanup-footer">
        <span class="cleanup-selected">
          {selectedCount() === 0 ? "Tick what you want to remove." : `${itemCount(selectedCount())} selected · ${formatSize(selectedBytes())}`}
        </span>
        <span class="cleanup-spacer" />
        <button class="cleanup-button cleanup-dry-run" disabled={selectedCount() === 0 || busy()} onClick={() => void dryRun()}>
          Dry run
        </button>
        <button class="cleanup-button primary cleanup-recycle" disabled={selectedCount() === 0 || busy()} onClick={() => setDialog({ kind: "recycle" })}>
          Move to Recycle Bin…
        </button>
      </footer>

      <Switch>
        <Match when={dialog()?.kind === "recycle"}>
          <CleanupDialog
            title={`Move ${itemCount(selectedCount())} to the Recycle Bin?`}
            confirm="Move to Recycle Bin"
            busy={busy()}
            onConfirm={() => void recycle()}
            onCancel={() => setDialog(null)}
          >
            <p class="cleanup-dialog-lead">
              {toRecycleBin(selectedCount(), selectedBytes())} Anything a program still has open is skipped.
            </p>
            <PathList label="Items to move" items={selection().flatMap((each) => each.items.map((item) => ({ path: item.path, bytes: item.bytes })))} />
          </CleanupDialog>
        </Match>
        <Match when={dialog()?.kind === "tool" && (dialog() as { rec: Recommendation }).rec}>
          {(rec) => (
            <CleanupDialog
              title={`Run ${toolOf(rec())?.command ?? ""}?`}
              confirm="Run"
              busy={busy()}
              onConfirm={() => void runTool(rec())}
              onCancel={() => setDialog(null)}
            >
              <p class="cleanup-dialog-lead">
                Kestrel runs this command{toolOf(rec())?.project ? " for the project below" : toolOf(rec())?.dir ? " in the folder below" : ""}:
              </p>
              <pre class="cleanup-command-block">{toolOf(rec())?.command}</pre>
              <Show when={toolOf(rec())?.project ?? toolOf(rec())?.dir}>
                {(folder) => (
                  <p class="cleanup-dialog-path-line" title={folder()}>
                    {folder()}
                  </p>
                )}
              </Show>
              <p class="cleanup-warning">
                This can't be undone: what it removes doesn't go to the Recycle Bin. Up to {formatSize(rec().bytes)} is freed.
              </p>
              <Show when={busy()}>
                <p class="cleanup-dialog-lead">Running…</p>
              </Show>
            </CleanupDialog>
          )}
        </Match>
        <Match when={dialog()?.kind === "tool-output" && (dialog() as { report: ToolReport }).report}>
          {(ran) => (
            <CleanupDialog
              title={ran().timed_out ? `${ran().command} took too long and was stopped` : ran().status === 0 ? `${ran().command} finished` : `${ran().command} ended with code ${ran().status ?? "unknown"}`}
              cancel="Close"
              onCancel={() => setDialog(null)}
            >
              <pre class="cleanup-output" aria-label="Output">
                {ran().output || "It printed nothing."}
              </pre>
            </CleanupDialog>
          )}
        </Match>
        <Match when={dialog()?.kind === "dry-run" && (dialog() as { report: RecycleReport }).report}>
          {(dry) => <DryRunDialog report={dry()} sizes={sizes()} onClose={() => setDialog(null)} />}
        </Match>
        <Match when={dialog()?.kind === "skipped" && outcome()}>
          {(shown) => (
            <CleanupDialog title="Skipped" cancel="Close" onCancel={() => setDialog(null)}>
              <SkippedList skipped={shown().skipped} heading="Left where they are" />
            </CleanupDialog>
          )}
        </Match>
      </Switch>
    </section>
  );
}

function RecommendationCard(props: {
  rec: Recommendation;
  ticked: Set<string>;
  expanded: boolean;
  onTick: (on: boolean) => void;
  onTickItem: (path: string, on: boolean) => void;
  isTicked: (path: string) => boolean;
  onExpand: () => void;
  onRunTool: () => void;
  onLink: (target: string) => void;
}) {
  const rec = () => props.rec;
  const recyclable = () => isRecycle(rec());
  const tool = () => toolOf(rec());
  const all = () => recyclable() && rec().items.length > 0 && props.ticked.size === rec().items.length;
  const some = () => props.ticked.size > 0 && !all();
  const shown = () => rec().items.slice(0, SHOWN_ITEMS);
  const hidden = () => rec().items.length - shown().length;
  let check: HTMLInputElement | undefined;
  createEffect(() => {
    if (check) check.indeterminate = some();
  });

  return (
    <article class="cleanup-rec" classList={{ ticked: props.ticked.size > 0 }} data-id={rec().id} data-tier={rec().tier}>
      <div class="cleanup-rec-head">
        <Show
          when={recyclable()}
          fallback={
            <span class="cleanup-rec-glyph" aria-hidden="true">
              <CleanupIcon name={tool() ? "terminal" : "info"} />
            </span>
          }
        >
          <input
            ref={check}
            type="checkbox"
            class="cleanup-check cleanup-rec-check"
            checked={all()}
            aria-label={`Select ${rec().title}`}
            onChange={(event) => props.onTick(event.currentTarget.checked)}
          />
        </Show>
        <div class="cleanup-rec-title">
          <h3>{rec().title}</h3>
          <TierChip tier={rec().tier} />
        </div>
        <Show when={rec().bytes > 0 || rec().tier !== "info"}>
          <span class="cleanup-rec-size" title={rec().action.kind === "recycle" ? "Freed by moving everything here to the Recycle Bin" : "At most"}>
            {rec().action.kind === "recycle" || rec().tier === "info" ? "" : "up to "}
            {formatSize(rec().bytes)}
          </span>
        </Show>
      </div>
      <p class="cleanup-rec-reason">{rec().reason}</p>
      <Show when={rec().parts.length > 0}>
        <ul class="cleanup-parts">
          <For each={rec().parts}>
            {(part) => (
              <li>
                <span>{part.label}</span>
                <span class="cleanup-part-size">{formatSize(part.bytes)}</span>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={rec().advice}>
        <p class="cleanup-advice">
          <WithCode text={rec().advice!} />
        </p>
      </Show>
      <Show when={rec().note}>
        <p class="cleanup-note">{rec().note}</p>
      </Show>
      <div class="cleanup-rec-actions">
        <Show when={rec().items.length > 0}>
          <button class="cleanup-link cleanup-expand" aria-expanded={props.expanded} onClick={() => props.onExpand()}>
            <span class="cleanup-chevron" classList={{ open: props.expanded }}>
              <CleanupIcon name="chevron" size={12} />
            </span>
            {props.expanded ? "Hide" : "Show"} {itemCount(rec().items.length)}
            <Show when={recyclable() && props.ticked.size > 0 && !all()}>
              <span class="cleanup-muted"> ({props.ticked.size} ticked)</span>
            </Show>
          </button>
        </Show>
        <span class="cleanup-spacer" />
        <Show when={tool()}>
          {(t) => (
            <>
              <code class="cleanup-command">{t().command}</code>
              <button class="cleanup-button cleanup-run" onClick={() => props.onRunTool()}>
                <CleanupIcon name="terminal" size={14} />
                Run {t().command}…
              </button>
            </>
          )}
        </Show>
        <For each={rec().links}>
          {(link) => (
            <button class="cleanup-button cleanup-open" data-target={link.target} onClick={() => props.onLink(link.target)}>
              <CleanupIcon name="external" size={14} />
              {link.label}
            </button>
          )}
        </For>
      </div>
      <Show when={props.expanded}>
        <ul class="cleanup-items">
          <For each={shown()}>
            {(item) => (
              <li class="cleanup-item" data-path={item.path}>
                <Show when={recyclable()}>
                  <input
                    type="checkbox"
                    class="cleanup-check"
                    checked={props.isTicked(item.path)}
                    aria-label={`Select ${item.path}`}
                    onChange={(event) => props.onTickItem(item.path, event.currentTarget.checked)}
                  />
                </Show>
                <span class="cleanup-item-icon">
                  <CleanupIcon name={item.dir ? "folder" : "file"} size={14} />
                </span>
                <PathLabel path={item.path} class="cleanup-item-path" />
                <span class="cleanup-item-when">{formatAgo(item.modified)}</span>
                <span class="cleanup-item-size">{formatSize(item.bytes)}</span>
              </li>
            )}
          </For>
          <Show when={hidden() > 0}>
            <li class="cleanup-item cleanup-more">and {itemCount(hidden())} more, ticked along with the recommendation</li>
          </Show>
        </ul>
      </Show>
    </article>
  );
}
