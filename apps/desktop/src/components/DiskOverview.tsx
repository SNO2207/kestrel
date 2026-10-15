import { For, Show, createMemo, createSignal } from "solid-js";

import { diskGrowth, zoomTo } from "../disk";
import type { Change, DiskItem, DiskOverview, VolumeInfo } from "../lib/diskApi";
import { baseName, formatCount, formatSize, parentPath } from "../lib/format";
import { DiskIcon } from "./DiskIcon";
import { Icon } from "./Icon";

/** Something in the overview that a menu can be opened for. */
export interface ListedItem {
  path: string;
  name: string;
  folder: boolean;
  bytes: number;
}

type OnMenu = (item: ListedItem, x: number, y: number) => void;

const DAY_MS = 86_400_000;
const LISTED = 20;
const percent = (part: number, whole: number) => (whole > 0 ? (part / whole) * 100 : 0);
const share = (part: number, whole: number) => {
  const value = percent(part, whole);
  return value === 0 ? "0%" : value < 0.1 ? "<0.1%" : `${value < 10 ? value.toFixed(1) : value.toFixed(0)}%`;
};
const plural = (n: number, one: string, many: string) => `${formatCount(n)} ${n === 1 ? one : many}`;
/** A sample's day: days are counted in UTC, so the date is read in UTC too. */
const dayDate = (day: number) => new Date(day * DAY_MS).toLocaleDateString(undefined, { day: "numeric", month: "short", timeZone: "UTC" });

const SYSTEM_FILES: Record<string, string> = {
  "pagefile.sys": "the page file",
  "hiberfil.sys": "the hibernation file",
  "swapfile.sys": "the swap file",
};

/** The drive's own figures beside the index's, and why they differ. */
function DriveFacts(props: { overview: DiskOverview; volume: VolumeInfo }) {
  const v = () => props.volume;
  const drive = () => `${v().label || "Local Disk"} (${v().root.slice(0, 2)})`;
  /** Used space the index has no file for; negative when the index counts more. */
  const unseen = () => v().used - v().indexed;
  const counted = () => v().system_files.filter((file) => file.counted);
  const hidden = () => v().system_files.filter((file) => !file.counted);
  const mine = () => (props.overview.is_drive ? 0 : props.overview.totals.bytes);
  return (
    <div class="disk-drive">
      <div class="disk-drive-head">
        <Icon name="drive" />
        <span class="disk-drive-name">{drive()}</span>
        <span class="disk-drive-numbers" data-used={v().used} data-free={v().free}>
          {formatSize(v().used)} used of {formatSize(v().total)} · {formatSize(v().free)} free
        </span>
      </div>
      <div class="disk-meter" role="img" aria-label={`${formatSize(v().used)} used of ${formatSize(v().total)}`}>
        <Show
          when={props.overview.is_drive}
          fallback={
            <>
              <span class="disk-meter-part mine" style={{ width: `${percent(mine(), v().total)}%` }} />
              <span class="disk-meter-part indexed" style={{ width: `${percent(Math.max(0, v().used - mine()), v().total)}%` }} />
            </>
          }
        >
          <span class="disk-meter-part indexed" style={{ width: `${percent(Math.min(v().indexed, v().used), v().total)}%` }} />
          <span class="disk-meter-part unseen" style={{ width: `${percent(Math.max(0, unseen()), v().total)}%` }} />
        </Show>
      </div>
      <Show
        when={props.overview.is_drive}
        fallback={
          <ul class="disk-legend">
            <li>
              <span class="disk-key mine" />
              This folder, {formatSize(mine())} ({share(mine(), v().used)} of what is used)
            </li>
            <li>
              <span class="disk-key indexed" />
              The rest of the drive
            </li>
            <li>
              <span class="disk-key free" />
              Free
            </li>
          </ul>
        }
      >
        <ul class="disk-legend">
          <li>
            <span class="disk-key indexed" />
            In files the index holds, {formatSize(v().indexed)}
          </li>
          <Show when={unseen() > 0}>
            <li>
              <span class="disk-key unseen" />
              Not in any file the index sees, {formatSize(unseen())}
            </li>
          </Show>
          <li>
            <span class="disk-key free" />
            Free, {formatSize(v().free)}
          </li>
        </ul>
        <div class="disk-explain" data-unseen={unseen()}>
          <Show
            when={unseen() >= 0}
            fallback={
              <p>
                The index counts {formatSize(-unseen())} more than Windows reports as used: a file with several names (Windows
                links many of its own files into more than one folder) is counted under each of them where the index read its
                folder by name.
              </p>
            }
          >
            <p>Windows counts space the index has no file for:</p>
            <ul>
              <Show when={v().mft}>
                {(mft) => (
                  <li data-part="mft">
                    NTFS's own records of every file and folder (the master file table), {formatSize(mft())}
                  </li>
                )}
              </Show>
              <For each={hidden()}>
                {(file) => (
                  <li data-part={file.name}>
                    {capitalise(SYSTEM_FILES[file.name] ?? file.name)}, {formatSize(file.bytes)}
                  </li>
                )}
              </For>
              <li>
                System Volume Information: restore points and earlier versions of files (shadow copies), which only Windows can
                open
              </li>
              <li>
                Files Windows keeps compressed in a stream of their own, such as browsers' program files, whose compressed copy
                the index doesn't count
              </li>
              <li>Folders your account can't open, and files still being written</li>
            </ul>
            <Show when={(v().mft ?? 0) + hidden().reduce((sum, file) => sum + file.bytes, 0) > unseen()}>
              <p class="disk-hint">
                The known parts add up to more than the difference: where the index read a folder by name, it counts a file
                with several names (Windows links many of its own files into more than one folder) under each of them.
              </p>
            </Show>
          </Show>
          <Show when={counted().length > 0}>
            <p class="disk-hint">
              Counted in the index: {counted().map((file) => `${SYSTEM_FILES[file.name] ?? file.name} (${formatSize(file.bytes)})`).join(", ")}.
            </p>
          </Show>
        </div>
      </Show>
    </div>
  );
}

const capitalise = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

function TotalsCard(props: { overview: DiskOverview }) {
  const totals = () => props.overview.totals;
  return (
    <section class="disk-card disk-totals" aria-label="Totals">
      <div class="disk-totals-main">
        <div class="disk-big">
          <span class="disk-big-number" data-bytes={totals().bytes}>
            {formatSize(totals().bytes)}
          </span>
          <span class="disk-big-label">on disk</span>
        </div>
        <dl class="disk-facts">
          <div>
            <dt>Size of the files</dt>
            <dd data-bytes={totals().logical}>{formatSize(totals().logical)}</dd>
          </div>
          <div>
            <dt>Files</dt>
            <dd data-count={totals().files}>{formatCount(totals().files)}</dd>
          </div>
          <div>
            <dt>Folders</dt>
            <dd data-count={totals().folders}>{formatCount(totals().folders)}</dd>
          </div>
          <Show when={totals().cloud_files > 0}>
            <div class="disk-cloud" title="Files kept online only take no space on this device; Kestrel never downloads them.">
              <dt>
                <DiskIcon name="cloud" size={14} />
                In the cloud
              </dt>
              <dd data-bytes={totals().cloud_bytes}>
                {formatSize(totals().cloud_bytes)} in {plural(totals().cloud_files, "file", "files")}
              </dd>
            </div>
          </Show>
        </dl>
        <Show when={props.overview.indexing}>
          <p class="disk-hint">The index is still being built, so these numbers will grow.</p>
        </Show>
      </div>
      <Show when={props.overview.volume}>{(volume) => <DriveFacts overview={props.overview} volume={volume()} />}</Show>
    </section>
  );
}

function TypesCard(props: { overview: DiskOverview }) {
  const rows = createMemo(() =>
    props.overview.categories.filter((row) => row.files > 0 || row.cloud_files > 0).sort((a, b) => b.bytes - a.bytes || a.label.localeCompare(b.label)),
  );
  const total = () => props.overview.totals.bytes;
  return (
    <section class="disk-card disk-types" aria-labelledby="disk-types-title">
      <h2 id="disk-types-title">By type</h2>
      <div class="disk-stack" role="img" aria-label="Space on disk by type">
        <For each={rows().filter((row) => row.bytes > 0)}>
          {(row) => (
            <span
              class="disk-stack-part"
              data-category={row.category}
              style={{ "flex-grow": row.bytes }}
              title={`${row.label}: ${formatSize(row.bytes)}, ${share(row.bytes, total())}`}
            />
          )}
        </For>
      </div>
      <table class="disk-table">
        <thead>
          <tr>
            <th>Type</th>
            <th class="numeric">On disk</th>
            <th class="numeric">Share</th>
            <th class="numeric col-logical">Size</th>
            <th class="numeric">Files</th>
          </tr>
        </thead>
        <tbody>
          <For each={rows()}>
            {(row) => (
              <tr data-category={row.category} data-bytes={row.bytes} data-logical={row.logical} data-files={row.files}>
                <td>
                  <span class="disk-type-label">
                    <span class="disk-swatch" data-category={row.category} />
                    {row.label}
                  </span>
                  <Show when={row.cloud_files > 0}>
                    <span class="disk-cloud-note" title="Kept online only: takes no space here">
                      and {formatSize(row.cloud_bytes)} in the cloud
                    </span>
                  </Show>
                </td>
                <td class="numeric">{formatSize(row.bytes)}</td>
                <td class="numeric muted">{share(row.bytes, total())}</td>
                <td class="numeric muted col-logical">{formatSize(row.logical)}</td>
                <td class="numeric muted">{formatCount(row.files)}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </section>
  );
}

function AgesCard(props: { overview: DiskOverview }) {
  const largest = () => Math.max(1, ...props.overview.ages.map((row) => row.bytes));
  return (
    <section class="disk-card disk-ages" aria-labelledby="disk-ages-title">
      <h2 id="disk-ages-title">By age</h2>
      <p class="disk-subtitle">When files were last changed</p>
      <ol class="disk-bars">
        <For each={props.overview.ages}>
          {(row, i) => (
            <li class="disk-bar-row" data-age={i()} data-bytes={row.bytes} data-files={row.files}>
              <span class="disk-bar-label">{row.label}</span>
              <span class="disk-bar-track">
                <span class="disk-bar" style={{ width: `${percent(row.bytes, largest())}%` }} />
              </span>
              <span class="disk-bar-value">{formatSize(row.bytes)}</span>
              <span class="disk-bar-count">{plural(row.files, "file", "files")}</span>
            </li>
          )}
        </For>
      </ol>
      <Show when={props.overview.undated.files > 0}>
        <p class="disk-hint">
          {plural(props.overview.undated.files, "file", "files")} ({formatSize(props.overview.undated.bytes)}) have no date.
        </p>
      </Show>
    </section>
  );
}

function ItemsCard(props: { title: string; items: DiskItem[]; folders: boolean; total: number; onMenu: OnMenu }) {
  const listed = (item: DiskItem): ListedItem => ({ path: item.path, name: item.name, folder: props.folders, bytes: item.bytes });
  const largest = () => Math.max(1, props.items[0]?.bytes ?? 1);
  return (
    <section class="disk-card disk-list-card" aria-label={props.title}>
      <h2>{props.title}</h2>
      <Show when={props.items.length > 0} fallback={<p class="disk-empty">None here.</p>}>
        <ol class="disk-items" classList={{ folders: props.folders }}>
          <For each={props.items.slice(0, LISTED)}>
            {(item) => (
              <li>
                <button
                  class="disk-item"
                  data-path={item.path}
                  data-bytes={item.bytes}
                  title={props.folders ? `${item.path}\nClick to look inside` : item.path}
                  onClick={(event) => {
                    if (props.folders) zoomTo(item.path);
                    else props.onMenu(listed(item), event.clientX, event.clientY);
                  }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    props.onMenu(listed(item), event.clientX, event.clientY);
                  }}
                >
                  <Show when={!props.folders} fallback={<Icon name="folder" size={14} />}>
                    <span class="disk-swatch" data-category={item.category ?? "other"} />
                  </Show>
                  <span class="disk-item-text">
                    <span class="disk-item-name">{props.folders ? item.name || item.path : item.name}</span>
                    <span class="disk-item-path">{parentPath(item.path) ?? ""}</span>
                  </span>
                  <span class="disk-item-weight" aria-hidden="true">
                    <span style={{ width: `${percent(item.bytes, largest())}%` }} />
                  </span>
                  <span class="disk-item-size">{formatSize(item.bytes)}</span>
                  <span class="disk-item-share">{share(item.bytes, props.total)}</span>
                </button>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </section>
  );
}

function ChangeList(props: { title: string; changes: Change[]; threshold: number; showNow?: boolean; onMenu: OnMenu }) {
  return (
    <Show when={props.changes.length > 0}>
      <h3>{props.title}</h3>
      <ol class="disk-items compact">
        <For each={props.changes.slice(0, 10)}>
          {(change) => (
            <li>
              <button
                class="disk-item"
                data-path={change.path}
                title={
                  change.before === null
                    ? `${change.path}\nUnder ${formatSize(props.threshold)} then, so it grew by at least this much`
                    : `${change.path}\n${formatSize(change.before)} then, ${formatSize(change.now)} now`
                }
                onClick={(event) => (change.file ? props.onMenu(changed(change), event.clientX, event.clientY) : zoomTo(change.path))}
                onContextMenu={(event) => {
                  event.preventDefault();
                  props.onMenu(changed(change), event.clientX, event.clientY);
                }}
              >
                <Show when={change.file} fallback={<Icon name="folder" size={14} />}>
                  <span class="disk-swatch" data-category="other" />
                </Show>
                <span class="disk-item-text">
                  <span class="disk-item-name">{baseName(change.path)}</span>
                  <span class="disk-item-path">{parentPath(change.path) ?? ""}</span>
                </span>
                <span class="disk-item-size grew">
                  {props.showNow ? formatSize(change.now) : `${change.before === null ? "at least " : ""}+${formatSize(change.grew)}`}
                </span>
              </button>
            </li>
          )}
        </For>
      </ol>
    </Show>
  );
}

const changed = (change: Change): ListedItem => ({ path: change.path, name: baseName(change.path), folder: !change.file, bytes: change.now });

/** What grew since the daily samples, or, until there are some, the files changed lately. */
function GrowthCard(props: { overview: DiskOverview; onMenu: OnMenu }) {
  const windows = () => diskGrowth()?.windows ?? [];
  const available = () => windows().filter((window) => window.growth);
  const [chosen, setChosen] = createSignal<number | null>(null);
  const shown = () => {
    const list = windows();
    const pick = chosen();
    if (pick !== null && list.find((window) => window.days === pick)?.growth) return list.find((window) => window.days === pick)!;
    return available()[0];
  };
  return (
    <section class="disk-card disk-growth" aria-labelledby="disk-growth-title">
      <div class="disk-card-head">
        <h2 id="disk-growth-title">What grew</h2>
        <Show when={available().length > 0}>
          <div class="segmented small" role="group" aria-label="Over the last">
            <For each={windows()}>
              {(window) => (
                <button aria-pressed={shown()?.days === window.days} disabled={!window.growth} onClick={() => setChosen(window.days)}>
                  {window.label}
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
      <Show when={shown()?.growth} fallback={<RecentChanges overview={props.overview} onMenu={props.onMenu} />}>
        {(growth) => {
          const change = () => (growth().before === null ? null : growth().now - growth().before!);
          const empty = () => growth().folders.length + growth().files.length + growth().new_files.length === 0;
          return (
            <>
              <p class="disk-growth-summary">
                Since {dayDate(growth().since_day)}:{" "}
                <Show when={change() !== null} fallback={`${formatSize(growth().now)} on disk now`}>
                  {change()! >= 0 ? `up ${formatSize(change()!)}` : `down ${formatSize(-change()!)}`}, to {formatSize(growth().now)}
                </Show>
              </p>
              <Show when={!empty()} fallback={<p class="disk-empty">Nothing here grew by much.</p>}>
                <ChangeList title="Folders that grew most" changes={growth().folders} threshold={diskGrowth()!.folder_min} onMenu={props.onMenu} />
                <ChangeList title="Large files that grew" changes={growth().files} threshold={diskGrowth()!.file_min} onMenu={props.onMenu} />
                <ChangeList title="New large files" changes={growth().new_files} threshold={diskGrowth()!.file_min} showNow onMenu={props.onMenu} />
              </Show>
            </>
          );
        }}
      </Show>
    </section>
  );
}

function RecentChanges(props: { overview: DiskOverview; onMenu: OnMenu }) {
  const [week, month] = [() => props.overview.recent[0], () => props.overview.recent[1]];
  return (
    <div class="disk-recent">
      <p class="disk-hint">
        Kestrel notes the size of every large folder once a day. What grew shows here once there is a day to compare with;
        until then, these are the files changed lately.
      </p>
      <div class="disk-tiles">
        <For each={[week(), month()].filter(Boolean)}>
          {(recent) => (
            <div class="disk-tile" data-days={recent.days} data-bytes={recent.bytes}>
              <span class="disk-tile-label">Last {recent.days} days</span>
              <span class="disk-tile-number">{formatSize(recent.bytes)}</span>
              <span class="disk-tile-sub">in {plural(recent.files, "file", "files")}</span>
            </div>
          )}
        </For>
      </div>
      <Show when={(week()?.folders.length ?? 0) > 0}>
        <h3>Where, in the last 7 days</h3>
        <ol class="disk-items compact">
          <For each={week()!.folders.slice(0, 8)}>
            {(folder) => (
              <li>
                <button
                  class="disk-item"
                  data-path={folder.path}
                  title={`${folder.path}\nClick to look inside`}
                  onClick={() => zoomTo(folder.path)}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    props.onMenu({ path: folder.path, name: baseName(folder.path), folder: true, bytes: folder.bytes }, event.clientX, event.clientY);
                  }}
                >
                  <Icon name="folder" size={14} />
                  <span class="disk-item-text">
                    <span class="disk-item-name">{baseName(folder.path) || folder.path}</span>
                    <span class="disk-item-path">{parentPath(folder.path) ?? ""}</span>
                  </span>
                  <span class="disk-item-count">{plural(folder.files, "file", "files")}</span>
                  <span class="disk-item-size">{formatSize(folder.bytes)}</span>
                </button>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </div>
  );
}

/** The overview tab: totals, types, ages, the largest files and folders, and what grew. */
export function DiskOverviewPanel(props: { overview: DiskOverview; onMenu: OnMenu }) {
  return (
    <div class="disk-overview">
      <TotalsCard overview={props.overview} />
      <TypesCard overview={props.overview} />
      <AgesCard overview={props.overview} />
      <ItemsCard title="Largest files" items={props.overview.largest_files} folders={false} total={props.overview.totals.bytes} onMenu={props.onMenu} />
      <ItemsCard title="Largest folders" items={props.overview.largest_folders} folders total={props.overview.totals.bytes} onMenu={props.onMenu} />
      <GrowthCard overview={props.overview} onMenu={props.onMenu} />
      <p class="disk-footnote">
        Worked out from Kestrel's index in {props.overview.took_ms.toFixed(0)} ms, without reading the disk. Sizes on disk count
        whole clusters, as Explorer's "Size on disk" does.
      </p>
    </div>
  );
}
