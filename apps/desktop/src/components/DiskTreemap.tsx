import { Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import { diskMeasure, diskRevision, diskRoot, zoomTo } from "../disk";
import { CATEGORIES, diskApi, type TreemapData } from "../lib/diskApi";
import { formatCount, formatSize } from "../lib/format";

/** The strip an open folder's name is drawn in (as the backend lays it out). */
const HEADER = 18;
const FONT = "12px 'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif";
const BOLD = "600 12px 'Segoe UI Variable Text', 'Segoe UI', system-ui, sans-serif";

export type TileKind = "file" | "folder" | "rest";

/** A tile as the rest of the hub sees it: what it stands for. */
export interface Tile {
  index: number;
  kind: TileKind;
  path: string;
  name: string;
  bytes: number;
  logical: number;
  /** Items in a tile of smaller items. */
  count: number;
  /** Position in `CATEGORIES`, or -1. */
  category: number;
  cloud: boolean;
}

const KINDS: TileKind[] = ["file", "folder", "rest"];

/** The full path of tile `i`: the map's root, then the names of the tiles it lies in. */
function pathOf(map: TreemapData, i: number): string {
  const names: string[] = [];
  for (let t = i; t >= 0; t = map.parent[t]) if (map.name[t]) names.push(map.name[t]);
  names.reverse();
  return [map.path.replace(/\\+$/, ""), ...names].join("\\");
}

function tileAt(map: TreemapData, i: number): Tile {
  return {
    index: i,
    kind: KINDS[map.kind[i]] ?? "file",
    path: pathOf(map, i),
    name: map.name[i],
    bytes: map.bytes[i],
    logical: map.logical[i],
    count: map.count[i],
    category: map.category[i],
    cloud: (map.flags[i] & 4) !== 0,
  };
}

/** The deepest tile under a point: tiles come parents first, so the last one that holds it. */
function hit(map: TreemapData, x: number, y: number): number {
  for (let i = map.x.length - 1; i >= 0; i--) {
    if (x >= map.x[i] && x < map.x[i] + map.w[i] && y >= map.y[i] && y < map.y[i] + map.h[i]) return i;
  }
  return -1;
}

// ---------- colours ----------

type Rgb = [number, number, number];

function parse(color: string): Rgb {
  const hex = color.trim().replace("#", "");
  const full = hex.length === 3 ? [...hex].map((c) => c + c).join("") : hex.padEnd(6, "0");
  return [0, 2, 4].map((at) => parseInt(full.slice(at, at + 2), 16) || 0) as Rgb;
}

const css = ([r, g, b]: Rgb) => `rgb(${Math.round(r)} ${Math.round(g)} ${Math.round(b)})`;
const mix = (a: Rgb, b: Rgb, t: number): Rgb => [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];

function luminance([r, g, b]: Rgb) {
  const channel = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** A fill with its cushion's light and dark ends, and the text that reads on it. */
interface Swatch {
  base: string;
  light: string;
  dark: string;
  ink: string;
}

function swatch(color: string): Swatch {
  const rgb = parse(color);
  return {
    base: css(rgb),
    light: css(mix(rgb, [255, 255, 255], 0.3)),
    dark: css(mix(rgb, [0, 0, 0], 0.28)),
    ink: luminance(rgb) > 0.36 ? "#10151b" : "#ffffff",
  };
}

interface Palette {
  categories: Swatch[];
  folder: Swatch;
  rest: Swatch;
  open: [string, string];
  line: string;
  text: string;
  muted: string;
  accent: string;
  hatch: string;
}

/** The colours of the hub's theme, from its CSS custom properties (disk.css). */
function readPalette(element: HTMLElement): Palette {
  const style = getComputedStyle(element);
  const value = (name: string) => style.getPropertyValue(name).trim();
  return {
    categories: CATEGORIES.map((category) => swatch(value(`--disk-${category.id.replace("_", "-")}`))),
    folder: swatch(value("--disk-folder")),
    rest: swatch(value("--disk-rest")),
    open: [value("--disk-open"), value("--disk-open-alt")],
    line: value("--line"),
    text: value("--text"),
    muted: value("--muted"),
    accent: value("--accent"),
    hatch: value("--disk-hatch"),
  };
}

/** `text` cut short with an ellipsis to fit `width`, or "" if not even that fits. */
function fit(ctx: CanvasRenderingContext2D, text: string, width: number): string {
  if (ctx.measureText(text).width <= width) return text;
  let [low, high] = [0, text.length];
  while (low < high) {
    const mid = Math.ceil((low + high) / 2);
    if (ctx.measureText(`${text.slice(0, mid)}…`).width <= width) low = mid;
    else high = mid - 1;
  }
  return low > 0 ? `${text.slice(0, low)}…` : "";
}

/** Diagonal lines across a tile: smaller items, and content that lives in the cloud. */
function hatch(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, color: string) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let d = -h; d < w; d += 6) {
    ctx.moveTo(x + d, y + h);
    ctx.lineTo(x + d + h, y);
  }
  ctx.stroke();
  ctx.restore();
}

function label(name: string, map: TreemapData, i: number) {
  if (map.kind[i] === 2) return `${formatCount(map.count[i])} smaller items`;
  return name;
}

/**
 * The treemap: every file and folder under the hub's folder as a rectangle sized by its space
 * on disk, coloured by type and shaded as a cushion, folders holding their contents under a
 * strip with their name. Hovering shows what a tile is; a click looks inside the folder under
 * the pointer; a right-click asks the hub for a menu.
 */
export function DiskTreemap(props: { onMenu: (tile: Tile, x: number, y: number) => void }) {
  let frame!: HTMLDivElement;
  let base!: HTMLCanvasElement;
  let overlay!: HTMLCanvasElement;
  let tooltip: HTMLDivElement | undefined;
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  const [map, setMap] = createSignal<TreemapData | null>(null);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [hover, setHover] = createSignal(-1);
  const [pointer, setPointer] = createSignal({ x: 0, y: 0 });
  const light = window.matchMedia("(prefers-color-scheme: light)");
  const [scheme, setScheme] = createSignal(light.matches);

  onMount(() => {
    const observer = new ResizeObserver(() => {
      const box = frame.getBoundingClientRect();
      setSize({ w: Math.floor(box.width), h: Math.floor(box.height) });
    });
    observer.observe(frame);
    const onScheme = () => setScheme(light.matches);
    light.addEventListener("change", onScheme);
    onCleanup(() => {
      observer.disconnect();
      light.removeEventListener("change", onScheme);
    });
  });

  // ---------- asking for the layout ----------

  let request = 0;
  let timer: number | undefined;
  createEffect(
    on([diskRoot, size, diskMeasure, diskRevision], ([path, { w, h }, measure]) => {
      clearTimeout(timer);
      if (!path || w < 8 || h < 8) return;
      // Resizing asks once the window settles; anything else at once.
      timer = window.setTimeout(
        async () => {
          const id = ++request;
          try {
            const next = await diskApi.treemap(path, w, h, measure);
            if (id !== request) return;
            setMap(next);
            setFailure(null);
            setHover(-1);
          } catch (error) {
            if (id === request) setFailure(String(error));
          }
        },
        map() ? 90 : 0,
      );
    }),
  );
  onCleanup(() => clearTimeout(timer));

  // ---------- drawing ----------

  function prepare(canvas: HTMLCanvasElement) {
    const { w, h } = size();
    const ratio = window.devicePixelRatio || 1;
    const [width, height] = [Math.round(w * ratio), Math.round(h * ratio)];
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, w, h);
    return ctx;
  }

  function draw(data: TreemapData) {
    const ctx = prepare(base);
    const colors = readPalette(frame);
    ctx.textBaseline = "middle";
    for (let i = 0; i < data.x.length; i++) {
      const [x, y, w, h] = [data.x[i], data.y[i], data.w[i], data.h[i]];
      const [kind, flags] = [data.kind[i], data.flags[i]];
      if (kind === 1 && flags & 1) {
        // An open folder: a frame for its contents, with its name along the top.
        ctx.fillStyle = colors.open[data.depth[i] % 2];
        ctx.fillRect(x, y, w, h);
        ctx.strokeStyle = colors.line;
        ctx.lineWidth = 1;
        ctx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
        if (flags & 2) {
          const size = formatSize(data.bytes[i]);
          const middle = y + 2 + HEADER / 2;
          ctx.font = FONT;
          const sizeWidth = ctx.measureText(size).width;
          ctx.font = BOLD;
          ctx.fillStyle = colors.text;
          const name = fit(ctx, data.name[i], w - sizeWidth - 22);
          if (name) {
            ctx.fillText(name, x + 6, middle);
            const after = x + 6 + ctx.measureText(name).width + 8;
            ctx.font = FONT;
            ctx.fillStyle = colors.muted;
            ctx.fillText(size, after, middle);
          } else {
            ctx.fillText(fit(ctx, data.name[i], w - 10), x + 6, middle);
          }
        }
        continue;
      }
      const category = data.category[i];
      const fill = kind === 2 ? colors.rest : category >= 0 ? colors.categories[category] : colors.folder;
      // A one-pixel gap between neighbours, where there is room for one.
      const [gx, gy, gw, gh] = w > 3 && h > 3 ? [x + 0.5, y + 0.5, w - 1, h - 1] : [x, y, w, h];
      if (gw > 6 && gh > 6) {
        const shade = ctx.createLinearGradient(gx, gy, gx + gw, gy + gh);
        shade.addColorStop(0, fill.light);
        shade.addColorStop(0.45, fill.base);
        shade.addColorStop(1, fill.dark);
        ctx.fillStyle = shade;
      } else {
        ctx.fillStyle = fill.base;
      }
      ctx.fillRect(gx, gy, gw, gh);
      if (kind === 2 || flags & 4) hatch(ctx, gx, gy, gw, gh, colors.hatch);
      if (w >= 44 && h >= 18) {
        ctx.font = FONT;
        ctx.fillStyle = fill.ink;
        const text = fit(ctx, label(data.name[i], data, i), w - 10);
        const twoLines = h >= 36;
        if (text) ctx.fillText(text, x + 5, twoLines ? y + 12 : y + h / 2);
        if (twoLines && text) {
          ctx.globalAlpha = 0.8;
          ctx.fillText(fit(ctx, formatSize(data.bytes[i]), w - 10), x + 5, y + 27);
          ctx.globalAlpha = 1;
        }
      }
    }
  }

  function drawHover(data: TreemapData | null, i: number) {
    const ctx = prepare(overlay);
    if (!data || i < 0 || i >= data.x.length) return;
    const colors = readPalette(frame);
    ctx.strokeStyle = colors.accent;
    ctx.lineWidth = 2;
    ctx.strokeRect(data.x[i] + 1, data.y[i] + 1, Math.max(0, data.w[i] - 2), Math.max(0, data.h[i] - 2));
  }

  createEffect(() => {
    const data = map();
    scheme();
    size();
    if (data) draw(data);
  });
  createEffect(() => drawHover(map(), hover()));

  // ---------- the pointer ----------

  const local = (event: MouseEvent) => {
    const box = base.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };

  const onMove = (event: MouseEvent) => {
    const data = map();
    if (!data) return;
    const at = local(event);
    setPointer(at);
    setHover(hit(data, at.x, at.y));
  };

  /** The folder a click looks inside: the tile itself, or the folder it lies in. */
  const folderOf = (data: TreemapData, i: number) => (data.kind[i] === 1 ? i : data.parent[i]);

  const onClick = (event: MouseEvent) => {
    const data = map();
    if (!data || event.button !== 0) return;
    const at = local(event);
    const i = hit(data, at.x, at.y);
    if (i < 0) return;
    const folder = folderOf(data, i);
    if (folder >= 0) zoomTo(pathOf(data, folder));
  };

  const onContextMenu = (event: MouseEvent) => {
    event.preventDefault();
    const data = map();
    if (!data) return;
    const at = local(event);
    const i = hit(data, at.x, at.y);
    if (i >= 0 && data.kind[i] !== 2) props.onMenu(tileAt(data, i), event.clientX, event.clientY);
  };

  /** Where the tooltip goes: beside the pointer, kept inside the map. */
  const tooltipPlace = () => {
    const { x, y } = pointer();
    const { w, h } = size();
    const box = tooltip?.getBoundingClientRect();
    const [tw, th] = [box?.width ?? 260, box?.height ?? 80];
    return {
      left: `${Math.max(4, x + 16 + tw > w ? x - tw - 12 : x + 16)}px`,
      top: `${Math.max(4, y + 18 + th > h ? y - th - 10 : y + 18)}px`,
    };
  };

  const hovered = () => {
    const data = map();
    const i = hover();
    return data && i >= 0 && i < data.x.length ? tileAt(data, i) : null;
  };

  if (import.meta.env.DEV) {
    // The end-to-end suite finds tiles by name, in page coordinates, to hover and click them.
    (window as unknown as { __kestrelDiskMap: object }).__kestrelDiskMap = {
      tiles: () => {
        const data = map();
        if (!data || !base?.isConnected) return [];
        const box = base.getBoundingClientRect();
        return data.x.map((_, i) => ({
          ...tileAt(data, i),
          x: box.left + data.x[i],
          y: box.top + data.y[i],
          w: data.w[i],
          h: data.h[i],
          parent: data.parent[i],
          open: (data.flags[i] & 1) !== 0,
        }));
      },
      path: () => map()?.path ?? null,
    };
  }

  return (
    <div class="disk-map-frame" ref={frame} onMouseLeave={() => setHover(-1)}>
      <canvas ref={base} class="disk-map-canvas" />
      <canvas
        ref={overlay}
        class="disk-map-canvas disk-map-overlay"
        role="img"
        tabindex="0"
        aria-label="Treemap of what uses the disk: click a folder to look inside it, right-click for more"
        onMouseMove={onMove}
        onClick={onClick}
        onContextMenu={onContextMenu}
      />
      <Show when={failure()}>
        <div class="disk-map-message">{failure()}</div>
      </Show>
      <Show when={map() && map()!.x.length === 0 && !failure()}>
        <div class="disk-map-message">Nothing here takes up space on the disk.</div>
      </Show>
      <Show when={hovered()}>
        {(tile) => (
          <div ref={tooltip} class="disk-tooltip" role="tooltip" style={tooltipPlace()}>
            <div class="disk-tooltip-name">
              <Show when={tile().kind !== "folder" || tile().category >= 0}>
                <span class="disk-swatch" data-category={tile().kind === "rest" ? "rest" : (CATEGORIES[tile().category]?.id ?? "folder")} />
              </Show>
              {tile().kind === "rest" ? `${formatCount(tile().count)} smaller items` : tile().name}
            </div>
            <div class="disk-tooltip-path">{tile().kind === "rest" ? `In ${tile().path}` : tile().path}</div>
            <div class="disk-tooltip-size">
              {formatSize(tile().bytes)} on disk
              <Show when={tile().kind !== "rest" && tile().logical !== tile().bytes}> · {formatSize(tile().logical)} in size</Show>
            </div>
            <div class="disk-tooltip-kind">
              {tile().kind === "folder"
                ? `Folder${tile().category >= 0 ? `, ${CATEGORIES[tile().category].label.toLowerCase()}` : ""} · click to look inside`
                : tile().kind === "rest"
                  ? "Each too small to draw"
                  : `${CATEGORIES[tile().category]?.label ?? "File"}${tile().cloud ? " · in the cloud, takes no space here" : ""}`}
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
