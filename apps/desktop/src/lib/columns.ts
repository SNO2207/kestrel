/**
 * The details view's columns: which are shown, in what order and how wide, for folders and for
 * search results separately, as Explorer keeps them. Every column can be sorted by except the
 * media library's.
 *
 * The four Explorer shows by default come from the listing itself; the rest come from the index
 * at no extra cost (allocated size, attributes, extension) or from the row's path (folder path,
 * which search results show by default). Date taken, Dimensions and Duration come from the media
 * library, for the photos and videos it has read (`lib/mediaFacts.ts`); the engine, which sorts,
 * knows nothing of them, so they don't sort. The choice is kept in the app's database
 * (`kestrel_db::layout`), loaded once at start (`loadColumns`) and saved as it changes.
 */
import { createSignal } from "solid-js";

import type { Row, SortKey } from "./api";
import { layoutApi } from "./layoutApi";

export type ColumnId =
  | "name"
  | "modified"
  | "type"
  | "size"
  | "alloc"
  | "attributes"
  | "extension"
  | "location"
  // The media library's facts (components/MediaCell.tsx), for photos and videos.
  | "taken"
  | "dimensions"
  | "duration";
export type ColumnKind = "folder" | "search";

export interface ColumnInfo {
  id: ColumnId;
  label: string;
  /** What clicking the header sorts by; the media library's columns have none. */
  sort?: SortKey;
  /** Numbers, aligned right. */
  numeric?: boolean;
  /** Width when first shown with a fixed width. */
  width: number;
  /** The least it gives way to before the view scrolls sideways, until it is given a width. */
  min: number;
}

export const COLUMNS: Record<ColumnId, ColumnInfo> = {
  name: { id: "name", label: "Name", sort: "name", width: 320, min: 180 },
  modified: { id: "modified", label: "Date modified", sort: "modified", width: 168, min: 150 },
  type: { id: "type", label: "Type", sort: "type", width: 128, min: 64 },
  size: { id: "size", label: "Size", sort: "size", numeric: true, width: 156, min: 120 },
  alloc: { id: "alloc", label: "Allocated size", sort: "alloc", numeric: true, width: 124, min: 84 },
  attributes: { id: "attributes", label: "Attributes", sort: "attributes", width: 92, min: 64 },
  extension: { id: "extension", label: "Extension", sort: "extension", width: 96, min: 64 },
  location: { id: "location", label: "Folder path", sort: "path", width: 320, min: 140 },
  taken: { id: "taken", label: "Date taken", width: 168, min: 140 },
  dimensions: { id: "dimensions", label: "Dimensions", numeric: true, width: 112, min: 84 },
  duration: { id: "duration", label: "Duration", numeric: true, width: 88, min: 64 },
};

/** The columns the header's menu offers, in its order. */
export const CHOOSABLE: readonly ColumnId[] = ["name", "modified", "type", "size", "alloc", "attributes", "extension", "location", "taken", "dimensions", "duration"];

export interface ColumnSet {
  /** The shown columns, in order. */
  order: ColumnId[];
  /** Widths given by resizing or fitting; a column without one keeps its default. */
  widths: Partial<Record<ColumnId, number>>;
}

/** The columns each kind of listing starts with, which is how the list has always looked. */
const DEFAULTS: Record<ColumnKind, ColumnSet> = {
  folder: { order: ["name", "modified", "type", "size"], widths: {} },
  search: { order: ["name", "location", "modified", "size"], widths: {} },
};

/**
 * Columns that share out the room left over until they are given a width: the least width and
 * the share. Every other column without a width has its default.
 */
const FLEXIBLE: Record<ColumnKind, Partial<Record<ColumnId, [number, number]>>> = {
  folder: { name: [150, 1] },
  search: { name: [150, 2], location: [140, 3] },
};

/** The narrowest a column can be dragged. */
export const MIN_WIDTH = 48;
/** The row's own padding, left and right, which the columns sit inside. */
export const ROW_PADDING = 8 + 14;

const isColumn = (value: unknown): value is ColumnId => typeof value === "string" && value in COLUMNS;

function checked(value: unknown, kind: ColumnKind): ColumnSet {
  const fallback = structuredClone(DEFAULTS[kind]);
  if (typeof value !== "object" || value === null) return fallback;
  const raw = value as Partial<ColumnSet>;
  const order = Array.isArray(raw.order) ? raw.order.filter(isColumn) : [];
  // The name is always shown.
  if (!order.includes("name")) order.unshift("name");
  const widths: ColumnSet["widths"] = {};
  if (raw.widths && typeof raw.widths === "object") {
    for (const [id, width] of Object.entries(raw.widths)) {
      if (isColumn(id) && typeof width === "number" && Number.isFinite(width)) widths[id] = Math.max(MIN_WIDTH, Math.round(width));
    }
  }
  return { order: [...new Set(order)], widths };
}

const [sets, setSets] = createSignal<Record<ColumnKind, ColumnSet>>(structuredClone(DEFAULTS));

const docName = (kind: ColumnKind) => `columns:${kind}`;

/** Takes the saved columns from the layout documents loaded at start. */
export function loadColumns(docs: Map<string, string>) {
  const read = (kind: ColumnKind) => {
    try {
      return checked(JSON.parse(docs.get(docName(kind)) ?? "null"), kind);
    } catch {
      return structuredClone(DEFAULTS[kind]);
    }
  };
  setSets({ folder: read("folder"), search: read("search") });
}

const timers: Partial<Record<ColumnKind, number>> = {};

function change(kind: ColumnKind, update: (set: ColumnSet) => ColumnSet) {
  const next = update(structuredClone(sets()[kind]));
  setSets((all) => ({ ...all, [kind]: next }));
  // Resizing changes the width many times a second; the last one is what is kept.
  clearTimeout(timers[kind]);
  timers[kind] = window.setTimeout(() => void layoutApi.save(docName(kind), JSON.stringify(next)).catch(() => {}), 250);
}

export const columnSet = (kind: ColumnKind): ColumnSet => sets()[kind];

/** Shows or hides a column; the name can't be hidden. */
export function toggleColumn(kind: ColumnKind, id: ColumnId) {
  if (id === "name") return;
  change(kind, (set) => {
    set.order = set.order.includes(id) ? set.order.filter((each) => each !== id) : [...set.order, id];
    return set;
  });
}

/** Moves a column to position `to` among the shown ones. */
export function moveColumn(kind: ColumnKind, id: ColumnId, to: number) {
  change(kind, (set) => {
    const rest = set.order.filter((each) => each !== id);
    rest.splice(Math.max(0, Math.min(rest.length, to)), 0, id);
    set.order = rest;
    return set;
  });
}

/** Gives columns widths of their own, in pixels. */
export function setWidths(kind: ColumnKind, widths: Partial<Record<ColumnId, number>>) {
  change(kind, (set) => {
    for (const [id, width] of Object.entries(widths) as [ColumnId, number][]) set.widths[id] = Math.max(MIN_WIDTH, Math.round(width));
    return set;
  });
}

/** Puts a kind's columns back as they started. */
export function resetColumns(kind: ColumnKind) {
  change(kind, () => structuredClone(DEFAULTS[kind]));
}

/**
 * The least width of a column nobody has sized: it gives way before the view scrolls sideways,
 * so two panes side by side still show every column.
 */
const giving = (id: ColumnId) => COLUMNS[id].min;

/** A column's CSS grid track. */
function track(kind: ColumnKind, set: ColumnSet, id: ColumnId): string {
  const width = set.widths[id];
  if (width !== undefined) return `${width}px`;
  const flexible = FLEXIBLE[kind][id];
  return flexible ? `minmax(${flexible[0]}px, ${flexible[1]}fr)` : `minmax(${giving(id)}px, ${COLUMNS[id].width}px)`;
}

/**
 * The grid tracks for a kind's columns. When every column has a fixed width, an empty last
 * track takes up what is left, so rows still span the view.
 */
export function columnTemplate(kind: ColumnKind): string {
  const set = columnSet(kind);
  const tracks = set.order.map((id) => track(kind, set, id));
  const flexes = tracks.some((value) => value.endsWith("fr)"));
  return flexes ? tracks.join(" ") : [...tracks, "minmax(0, 1fr)"].join(" ");
}

/** The least width the columns need, beyond which the details view scrolls sideways. */
export function columnsWidth(kind: ColumnKind): number {
  const set = columnSet(kind);
  return ROW_PADDING + set.order.reduce((sum, id) => sum + (set.widths[id] ?? FLEXIBLE[kind][id]?.[0] ?? giving(id)), 0);
}

// ---------- what the extra columns show ----------

/** Attribute bits and the letters Explorer's Attributes column shows, in its order. */
const ATTRIBUTE_LETTERS: [number, string][] = [
  [0x1, "R"],
  [0x2, "H"],
  [0x4, "S"],
  [0x10, "D"],
  [0x20, "A"],
  [0x2_0000, "X"],
  [0x80, "N"],
  [0x100, "T"],
  [0x400, "L"],
  [0x800, "C"],
  [0x1000, "O"],
  [0x2000, "I"],
  [0x4000, "E"],
  [0x8000, "V"],
  [0x8_0000, "P"],
  [0x10_0000, "U"],
  [0x4_0000, "M"],
  [0x40_0000, "M"],
];

/** The letters Explorer shows for a row's attributes, such as `HSA`; the same as the engine sorts by. */
export function attributeLetters(attrs: number): string {
  let letters = "";
  for (const [bit, letter] of ATTRIBUTE_LETTERS) {
    if ((attrs & bit) !== 0 && !letters.endsWith(letter)) letters += letter;
  }
  return letters;
}

/** The folder a row is in: given for search results, worked out for a folder's own rows. */
export function folderOf(row: Pick<Row, "location" | "path">): string {
  if (row.location !== null) return row.location;
  const cut = row.path.replace(/\\+$/, "").lastIndexOf("\\");
  const parent = cut < 0 ? "" : row.path.slice(0, cut);
  return parent.endsWith(":") ? `${parent}\\` : parent;
}
