import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup } from "solid-js";

import { api, iconUrl, thumbUrl, type PreviewDetails, type Row } from "../lib/api";
import { baseName, formatCount, formatDate, formatSize } from "../lib/format";
import { locationOf } from "../lib/locations";
import { parityApi, type SelectionSummary } from "../lib/parityApi";
import { SYNC_LABELS, syncState } from "../lib/syncStatus";
import { detailsOpen, setDetailsOpen } from "../parity";
import { DEFAULT_PANE_WIDTH, MIN_PANE_WIDTH, focusedItem, paneOpen, paneWidth, setPaneWidth } from "../previewState";
import { activeTab, isSearch, selectionOf } from "../state";
import { MediaText } from "./MediaText";
import { ParityIcon } from "./ParityIcon";

/** Room always left for the file list beside the pane. */
const LIST_MIN_WIDTH = 300;
/** How long the selection must rest before the pane reads it again. */
const SETTLE_MS = 120;

type Line = [label: string, value: string];

const bytes = (size: number) => (size >= 1024 ? `${formatSize(size)} (${formatCount(size)} bytes)` : formatSize(size));

/**
 * The details pane (Alt+Shift+P), in the place of the preview pane at the right of the list,
 * as in Explorer: the selected item's properties (size, dates, attributes, sync status, and
 * what Windows knows of it, such as a photo's dimensions or a video's length), the totals of
 * a selection of several, or the folder's own details when nothing is selected.
 */
export function DetailsPane() {
  // The two panes share the right of the window: opening the preview closes this.
  createEffect(on(paneOpen, (open) => open && detailsOpen() && setDetailsOpen(false)));

  return (
    <Show when={detailsOpen()}>
      <aside class="preview-pane details-pane" aria-label="Details">
        <Resizer />
        <Body />
      </aside>
    </Show>
  );
}

/** The left edge, which resizes the pane like the preview's (they share one width). */
function Resizer() {
  const maxWidth = (pane: HTMLElement) =>
    Math.max(MIN_PANE_WIDTH, (pane.parentElement?.clientWidth ?? window.innerWidth) - LIST_MIN_WIDTH);
  const start = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const pane = handle.parentElement!;
    const startX = event.clientX;
    const startWidth = pane.getBoundingClientRect().width;
    handle.setPointerCapture(event.pointerId);
    event.preventDefault();
    const move = (moved: PointerEvent) => setPaneWidth(Math.min(maxWidth(pane), startWidth + (startX - moved.clientX)), false);
    const end = () => {
      setPaneWidth(paneWidth(), true);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
  };
  return (
    <div
      class="preview-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the details pane"
      title="Drag to resize. Double-click to reset."
      onPointerDown={start}
      onDblClick={() => setPaneWidth(DEFAULT_PANE_WIDTH, true)}
    />
  );
}

/** What the pane is about: nothing, one item, or several. */
type Subject =
  | { kind: "none"; message: string }
  | { kind: "one"; item: Pick<Row, "name" | "path" | "dir" | "size" | "mtime" | "ext"> & { attrs?: number }; folder: boolean }
  | { kind: "many"; count: number; key: string };

function Body() {
  const [settled, setSettled] = createSignal<Subject>({ kind: "none", message: "Select an item to see its details." });
  const subject = createMemo<Subject>(() => {
    const tab = activeTab();
    if (!tab) return { kind: "none", message: "" };
    const count = tab.selected;
    if (count > 1) return { kind: "many", count, key: `${tab.id}:${tab.revision}:${count}:${focusedItem()?.path ?? ""}` };
    const focused = focusedItem();
    if (count === 1 && focused) return { kind: "one", item: focused, folder: false };
    // A special location's page keeps its own selection (HomeView reports it as the focused item).
    if (locationOf(tab.path) && !tab.listing) {
      return focused ? { kind: "one", item: focused, folder: false } : { kind: "none", message: "Select an item to see its details." };
    }
    if (isSearch(tab)) return { kind: "none", message: "Select a result to see its details." };
    if (!tab.listing) return { kind: "none", message: "" };
    // Nothing selected: the folder itself, as Explorer shows it. Its revision stands in for a
    // modification time, so its details are read again whenever its listing changes.
    const folder = { name: baseName(tab.path) || tab.path, path: tab.path, dir: true, size: 0, mtime: tab.revision, ext: "" };
    return { kind: "one", item: folder, folder: true };
  });
  // Holding an arrow key moves through many items; the pane reads only where it comes to rest.
  let timer: number | undefined;
  createEffect(
    on(subject, (next) => {
      clearTimeout(timer);
      timer = window.setTimeout(() => setSettled(next), SETTLE_MS);
    }),
  );
  onCleanup(() => clearTimeout(timer));

  return (
    <div class="details-body">
      <Show when={settled().kind === "one" && (settled() as Extract<Subject, { kind: "one" }>)} keyed>
        {(one) => <OneItem item={one.item} isFolder={one.folder} />}
      </Show>
      <Show when={settled().kind === "many" && (settled() as Extract<Subject, { kind: "many" }>)} keyed>
        {(many) => <ManyItems count={many.count} />}
      </Show>
      <Show when={settled().kind === "none" && (settled() as Extract<Subject, { kind: "none" }>)}>
        {(none) => (
          <div class="details-empty">
            <ParityIcon name="details" size={28} />
            <p>{none().message}</p>
          </div>
        )}
      </Show>
    </div>
  );
}

function OneItem(props: { item: Extract<Subject, { kind: "one" }>["item"]; isFolder: boolean }) {
  const [details] = createResource(() => `${props.item.path}|${props.item.size}|${props.item.mtime}`, () => api.previewDetails(props.item.path));
  const [properties] = createResource(
    () => (props.item.dir ? null : props.item.path),
    (path) => parityApi.itemProperties(path),
  );
  const known = (): PreviewDetails | undefined => (details.error ? undefined : details());
  const sync = () => syncState(props.item.attrs === undefined ? undefined : { path: props.item.path, dir: props.item.dir, attrs: props.item.attrs });

  const lines = createMemo<Line[]>(() => {
    const d = known();
    const list: Line[] = [];
    if (!d) return list;
    list.push(["Type", d.type_name]);
    if (d.folder) {
      if (d.folder.readable) list.push(["Contains", `${formatCount(d.folder.files)} files, ${formatCount(d.folder.folders)} folders`]);
      list.push(["Size", d.size === null ? "Not indexed yet" : bytes(d.size)]);
    } else if (d.size !== null) {
      list.push(["Size", bytes(d.size)]);
    }
    const extra = properties.error ? [] : (properties() ?? []);
    const named = new Set(extra.map((property) => property.label.toLowerCase()));
    if (d.dimensions && !named.has("dimensions")) list.push(["Dimensions", `${d.dimensions[0]} × ${d.dimensions[1]}`]);
    for (const property of extra) list.push([property.label, property.value]);
    if (d.modified) list.push(["Date modified", formatDate(d.modified)]);
    if (d.created) list.push(["Date created", formatDate(d.created)]);
    if (d.accessed) list.push(["Date accessed", formatDate(d.accessed)]);
    const state = sync();
    if (state) list.push(["Availability", SYNC_LABELS[state]]);
    else if (d.online_only) list.push(["Availability", SYNC_LABELS.cloud]);
    if (d.attributes.length > 0) list.push(["Attributes", d.attributes.join(", ")]);
    if (d.path.length > 3) {
      const cut = d.path.replace(/\\+$/, "").lastIndexOf("\\");
      if (cut > 0) list.push(["Location", d.path.slice(0, cut).replace(/:$/, ":\\")]);
    }
    return list;
  });

  /** A thumbnail for files that have one; online-only files show their icon, as elsewhere. */
  const picture = () =>
    props.item.dir || known()?.online_only ? iconUrl(props.item, 48) : thumbUrl(props.item, 192);

  return (
    <div class="details-item" data-path={props.item.path}>
      <div class="details-picture">
        <img src={picture()} alt="" draggable={false} onError={(event) => (event.currentTarget.src = iconUrl(props.item, 48))} />
      </div>
      <div class="details-name" title={props.item.path}>
        {props.item.name}
      </div>
      <div class="details-kind">{props.isFolder ? "This folder" : (known()?.type_name ?? "")}</div>
      <Show when={details.error}>
        <p class="details-failure">{String(details.error)}</p>
      </Show>
      <DetailLines lines={lines()} />
      <Show when={!props.item.dir}>
        <MediaText path={props.item.path} ext={props.item.ext} mtime={props.item.mtime} />
      </Show>
    </div>
  );
}

function ManyItems(props: { count: number }) {
  const [summary] = createResource(
    () => {
      const tab = activeTab();
      const selection = tab ? selectionOf(tab.id) : null;
      return selection ? { ...selection, count: props.count } : null;
    },
    ({ set, ranges }) => parityApi.selectionSummary(set, ranges),
  );
  const lines = createMemo<Line[]>(() => {
    const s: SelectionSummary | undefined = summary.error ? undefined : summary();
    if (!s) return [];
    const list: Line[] = [];
    if (s.files > 0) list.push(["Files", formatCount(s.files)]);
    if (s.folders > 0) list.push(["Folders", formatCount(s.folders)]);
    list.push(["Total size", s.complete ? bytes(s.bytes) : `At least ${formatSize(s.bytes)}`]);
    if (s.modified_from > 0) {
      const from = formatDate(s.modified_from);
      const to = formatDate(s.modified_to);
      list.push(["Date modified", from === to ? from : `${from} – ${to}`]);
    }
    return list;
  });
  return (
    <div class="details-item details-many">
      <div class="details-picture many">
        <ParityIcon name="select" size={40} />
      </div>
      <div class="details-name">{formatCount(props.count)} items selected</div>
      <DetailLines lines={lines()} />
    </div>
  );
}

function DetailLines(props: { lines: Line[] }) {
  return (
    <dl class="details-list">
      <For each={props.lines}>
        {([label, value]) => (
          <div class="details-line" data-label={label}>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        )}
      </For>
    </dl>
  );
}
