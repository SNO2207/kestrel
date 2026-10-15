/**
 * The command palette's front end half: its providers and how a query reaches them.
 *
 * The palette mixes items from several providers in one ranked list. Every provider has a
 * front end half, registered here, that says what its items are called and what choosing one
 * does. Its items come from one of two places:
 * - the front end, through `items`, when they are the front end's own (the app's actions);
 * - a backend provider of the same id (`palette.rs`), when they come from the backend's data
 *   (files and folders from the index, and features such as projects and tags).
 * Either way they are ranked together in the backend, by one fuzzy matcher, boosted by how
 * often and how lately each was used.
 *
 * To add a provider, register both halves under one id: the backend one in Rust (see the
 * documentation of `palette.rs`), and this one at startup, in both windows (the main window,
 * App.tsx, and the quick launcher, QuickLauncher.tsx), for example:
 *
 * ```ts
 * registerPaletteProvider({
 *   id: "projects",
 *   prefix: "@",
 *   label: () => "Project",
 *   run: (hit, mode) => (mode === "new-tab" ? openTab(hit.path!) : navigate(activeId(), hit.path!)),
 *   complete: (hit) => `@${hit.title}`,
 * });
 * ```
 *
 * Prefixes typed first narrow the palette to one provider's items: `>` actions, `/` paths,
 * `@` projects, `#` tags.
 */
import { paletteApi, type OpenMode, type PaletteHit, type PaletteItem, type PaletteResults } from "./paletteApi";

export type { OpenMode, PaletteHit, PaletteItem } from "./paletteApi";

/** Where the palette is: the main window's Ctrl+K, or the quick launcher. */
export type PaletteWindow = "main" | "quick";

export interface PaletteContext {
  window: PaletteWindow;
  /** The text typed after any prefix. */
  text: string;
}

export interface PaletteProvider {
  /** The id items carry in `PaletteItem.provider`; the same as its backend half's, if it has one. */
  id: string;
  /** What one of its items is, shown beside it when it has no shortcut: "Folder", "Project". */
  label: (hit: PaletteHit) => string;
  /** Typed first, narrows the palette to this provider: one of `>`, `/`, `@`, `#`. */
  prefix?: string;
  /** Whether its items take part when no prefix is typed. Defaults to true. */
  inDefault?: boolean;
  /** Its items, when the front end makes them. Leave out when a backend provider does. */
  items?: (context: PaletteContext) => PaletteItem[];
  /** Whether its items are listed when the palette opens with nothing typed. */
  listWhenEmpty?: boolean;
  /** Carries out a chosen item. In the quick launcher this runs in the main window, unless `runsAnywhere`. */
  run: (hit: PaletteHit, mode: OpenMode) => void | Promise<void>;
  /** Whether a choice needs no window of Kestrel's, such as a file opening in its own app. */
  runsAnywhere?: (hit: PaletteHit, mode: OpenMode) => boolean;
  /** What Tab puts in the input for an item, or null to leave the input alone. */
  complete?: (hit: PaletteHit) => string | null;
}

/** The prefixes, and what each one narrows the palette to. */
export const PREFIXES: { prefix: string; label: string; empty: string }[] = [
  { prefix: ">", label: "actions", empty: "No actions match." },
  { prefix: "/", label: "paths", empty: "No folder or file there." },
  { prefix: "@", label: "projects", empty: "No projects yet." },
  { prefix: "#", label: "tags", empty: "No tags yet." },
];

const providers = new Map<string, PaletteProvider>();

/** Adds a provider, replacing any with the same id. Returns a function that removes it. */
export function registerPaletteProvider(provider: PaletteProvider): () => void {
  providers.set(provider.id, provider);
  return () => {
    if (providers.get(provider.id) === provider) providers.delete(provider.id);
  };
}

export const paletteProvider = (id: string) => providers.get(id);

/** Splits the typed text into its prefix, if any, and the text to match, as the backend does. */
export function splitPrefix(text: string): { prefix: string | null; rest: string } {
  const trimmed = text.trimStart();
  const first = trimmed.charAt(0);
  if (PREFIXES.some((entry) => entry.prefix === first)) return { prefix: first, rest: trimmed.slice(1).trimStart() };
  return { prefix: null, rest: trimmed };
}

/** What the palette shows for some text: the hits each with its provider, and how to read an empty list. */
export interface PaletteAnswer {
  hits: PaletteHit[];
  prefix: string | null;
  /** Some provider answers this prefix, so an empty list means nothing matched. */
  provided: boolean;
  tookUs: number;
}

/**
 * Asks for the hits for `text`: gathers the front end providers' items, has the backend rank
 * them with its own, and drops hits no registered provider can carry out.
 */
export async function askPalette(text: string, window: PaletteWindow, showHidden: boolean): Promise<PaletteAnswer> {
  const { prefix, rest } = splitPrefix(text);
  const context: PaletteContext = { window, text: rest };
  const taking = [...providers.values()].filter((provider) =>
    prefix !== null ? provider.prefix === prefix : provider.inDefault !== false,
  );
  const items = taking
    .filter((provider) => provider.items && (prefix !== null || rest !== "" || provider.listWhenEmpty))
    .flatMap((provider) => provider.items!(context));
  const results: PaletteResults = await paletteApi.query({ text, session: window, items, show_hidden: showHidden, limit: 60 });
  const hits = results.hits.filter((hit) => providers.has(hit.provider));
  const provided = taking.some((provider) => provider.items) || results.providers.length > 0;
  return { hits, prefix, provided, tookUs: results.took_us };
}

/** Remembers a choice, so it ranks higher next time. */
export function rememberChoice(hit: PaletteHit) {
  void paletteApi
    .record({ provider: hit.provider, id: hit.id, title: hit.title, subtitle: hit.subtitle ?? null, path: hit.path ?? null, dir: hit.dir ?? false })
    .catch((error) => console.error("could not remember the palette choice", error));
}

/** Splits a title into runs of plain and matched text, for highlighting. */
export function highlightRuns(title: string, highlights: number[]): { text: string; matched: boolean }[] {
  const marked = new Set(highlights);
  const runs: { text: string; matched: boolean }[] = [];
  for (let i = 0; i < title.length; ) {
    // Characters outside the basic plane take two UTF-16 units, which stay together.
    const char = String.fromCodePoint(title.codePointAt(i)!);
    const matched = marked.has(i);
    const last = runs[runs.length - 1];
    if (last && last.matched === matched) last.text += char;
    else runs.push({ text: char, matched });
    i += char.length;
  }
  return runs;
}
