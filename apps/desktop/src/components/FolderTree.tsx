/**
 * The sidebar's folder tree, as in Explorer's navigation pane: This PC, its drives, and the
 * folders on them.
 *
 * Folders come from the index (`foldertree.rs`): instant, and with no disk access on indexed
 * drives; folders the index lacks (shares, removable drives) are listed from disk. Hidden
 * folders follow "Show hidden items". The tree is virtualised: the expanded tree is kept as a
 * flat list of its visible rows, and only the rows in view are drawn, so a drive expanded all
 * the way down (hundreds of thousands of folders) scrolls as smoothly as a short list.
 *
 * Keys, as in Explorer: Up and Down move, Left collapses or goes to the parent, Right expands
 * or goes to the first folder inside, Home, End, Page Up and Page Down, typing jumps to a name,
 * `*` expands everything below, Enter opens (Ctrl+Enter in a new tab). Moving with the keys
 * opens the folder once the keys rest, as Explorer does. The folder the tab shows is
 * highlighted and, with "Expand to open folder" (on by default), revealed. Every row is a drop
 * target (`data-place`, see dragdrop.ts) and has a context menu. When the index changes, the
 * folders in view are asked for again.
 */
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack } from "solid-js";

import { isDropPlace } from "../dragdrop";
import { placesApi, type DriveInfo, type TreeFolder, type TreeListing } from "../lib/placesApi";
import { showProtected } from "../lib/folderOptions";
import { showHidden } from "../lib/views";
import {
  THIS_PC,
  analyseDiskUsage,
  canAnalyse,
  canEject,
  ejectDrive,
  expandToOpen,
  openLocation,
  samePlace,
  setExpandToOpen,
  thisPc,
} from "../places";
import { closeDiskUsage, diskOpen } from "../disk";
import { closeProjects, projectsOpen } from "../projects";
import { pinPaths } from "../shortcuts";
import { activeTab, isSearch, notify } from "../state";
import { api } from "../lib/api";
import type { MenuEntry } from "./ContextMenu";
import { Icon, type IconName } from "./Icon";
import { showMenu } from "./ShortcutDialogs";

/** Height of a row, in pixels; rows are placed by it. */
const ROW = 26;
/** Indentation per level. */
const INDENT = 12;
/** Rows drawn beyond each edge of the view. */
const OVERSCAN = 12;
/** How long the keys must rest on a folder before the tab opens it, as in Explorer. */
const FOLLOW_MS = 550;
const TYPE_AHEAD_RESET_MS = 700;
/** Folders asked for in one request when many need asking at once. */
const BATCH = 200;

interface TreeNode {
  kind: "pc" | "drive" | "folder";
  name: string;
  /** Set for This PC and drives; folders work theirs out from their parents. */
  fixedPath?: string;
  parent: TreeNode | null;
  depth: number;
  hidden: boolean;
  /** Whether it holds folders: null until known. */
  hasChildren: boolean | null;
  expanded: boolean;
  loading: boolean;
  /** Its folders once asked for; null before. */
  children: TreeNode[] | null;
  /** The index revision its folders were asked for at. */
  stamp: number;
  error: string | null;
  drive?: DriveInfo;
}

const join = (folder: string, name: string) => (folder.endsWith("\\") ? `${folder}${name}` : `${folder}\\${name}`);

function pathOf(node: TreeNode): string {
  if (node.fixedPath !== undefined) return node.fixedPath;
  return join(pathOf(node.parent!), node.name);
}

function folderNode(folder: TreeFolder, parent: TreeNode): TreeNode {
  return {
    kind: "folder",
    name: folder.name,
    parent,
    depth: parent.depth + 1,
    hidden: folder.hidden,
    hasChildren: folder.children,
    expanded: false,
    loading: false,
    children: folder.children === false ? [] : null,
    stamp: 0,
    error: null,
  };
}

function iconFor(node: TreeNode): IconName {
  if (node.kind === "pc") return "pc";
  if (node.kind === "folder") return "folder";
  switch (node.drive?.kind) {
    case "removable":
      return "usb";
    case "optical":
      return "disc";
    case "network":
      return "network-drive";
    default:
      return "drive";
  }
}

export function FolderTree() {
  let scroller!: HTMLDivElement;
  let tree!: HTMLDivElement;
  const root: TreeNode = {
    kind: "pc",
    name: "This PC",
    fixedPath: THIS_PC,
    parent: null,
    depth: 0,
    hidden: false,
    hasChildren: true,
    expanded: true,
    loading: false,
    children: [],
    stamp: 0,
    error: null,
  };
  /** The rows of the expanded tree, in order. */
  let rows: TreeNode[] = [root];
  /** Bumped whenever rows or any row's state change. */
  const [version, setVersion] = createSignal(0);
  const changed = () => setVersion((n) => n + 1);
  const [viewTop, setViewTop] = createSignal(0);
  const [viewHeight, setViewHeight] = createSignal(400);
  const [focusNode, setFocusNode] = createSignal<TreeNode | null>(null, { equals: false });
  const [hasFocus, setHasFocus] = createSignal(false);
  /** The index revision last announced by the backend. */
  let revision = 0;
  let followTimer: number | undefined;
  const typeAhead = { prefix: "", at: 0 };

  /** Rebuilds the flat list of rows from the tree. */
  const rebuild = () => {
    const next: TreeNode[] = [];
    const stack: TreeNode[] = [root];
    while (stack.length > 0) {
      const node = stack.pop()!;
      next.push(node);
      if (node.expanded && node.children) {
        for (let i = node.children.length - 1; i >= 0; i--) stack.push(node.children[i]);
      }
    }
    rows = next;
    changed();
  };

  // ---------- drives ----------

  /** Keeps This PC's children in step with the drives, keeping each drive's state. */
  createEffect(() => {
    const drives = thisPc().drives;
    const before = new Map((root.children ?? []).map((node) => [node.fixedPath!.toLowerCase(), node]));
    const readable = (drive: DriveInfo) => drive.state === "ready" || drive.state === "reading";
    root.children = drives.map((drive) => {
      const known = before.get(drive.path.toLowerCase());
      if (known) {
        // A disc put in, or a network drive connecting, is looked at afresh.
        if (known.drive && readable(known.drive) !== readable(drive)) {
          known.hasChildren = readable(drive) ? null : false;
          known.children = null;
          known.expanded = false;
        }
        known.name = drive.name;
        known.drive = drive;
        return known;
      }
      return {
        kind: "drive",
        name: drive.name,
        fixedPath: drive.path,
        parent: root,
        depth: 1,
        hidden: false,
        hasChildren: readable(drive) ? null : false,
        expanded: false,
        loading: false,
        children: null,
        stamp: 0,
        error: null,
        drive,
      } satisfies TreeNode;
    });
    rebuild();
    // A drive that has just come is where the open folder may be.
    untrack(() => void revealCurrent(false));
  });

  // ---------- asking for folders ----------

  /**
   * Takes in a folder's listing, keeping the state of folders already known. Returns whether
   * the rows shown may have changed, which only a rebuild of the rows puts right; most answers
   * to a change elsewhere on the drive change nothing here, and need only a redraw.
   */
  const merge = (node: TreeNode, listing: TreeListing, stamp = revision): boolean => {
    const wasExpanded = node.expanded;
    const old = node.children;
    node.stamp = stamp;
    node.loading = false;
    node.error = listing.error;
    if (listing.error) {
      node.children = [];
      node.hasChildren = false;
      node.expanded = false;
      return wasExpanded;
    }
    const before = new Map((node.children ?? []).map((child) => [child.name.toLowerCase(), child]));
    node.children = listing.folders.map((folder) => {
      const known = before.get(folder.name.toLowerCase());
      if (!known) return folderNode(folder, node);
      known.name = folder.name;
      known.hidden = folder.hidden;
      if (folder.children !== null) known.hasChildren = folder.children;
      if (folder.children === false && !known.children?.length) {
        known.children = [];
        known.expanded = false;
      }
      return known;
    });
    node.hasChildren = node.children.length > 0;
    if (!node.hasChildren) node.expanded = false;
    const children = node.children;
    const same = old !== null && old.length === children.length && old.every((child, i) => child === children[i]);
    return (wasExpanded || node.expanded) && (!same || wasExpanded !== node.expanded);
  };

  /** Asks for the folders of these nodes, in batches, and takes them in. */
  const load = async (nodes: TreeNode[]) => {
    for (let at = 0; at < nodes.length; at += BATCH) {
      const batch = nodes.slice(at, at + BATCH);
      batch.forEach((node) => (node.loading = true));
      changed();
      try {
        const listings = await placesApi.treeChildren(batch.map(pathOf), showHidden());
        batch.forEach((node, i) => listings[i] && merge(node, listings[i]));
      } catch (error) {
        batch.forEach((node) => (node.loading = false));
        console.error("folder tree:", error);
      }
    }
    rebuild();
  };

  const expand = async (node: TreeNode) => {
    if (node.hasChildren === false) return;
    if (node.children === null) await load([node]);
    if (node.children && node.children.length > 0) node.expanded = true;
    rebuild();
  };

  const collapse = (node: TreeNode) => {
    node.expanded = false;
    // The keyboard's place can't be inside what was folded away.
    let focused = focusNode();
    while (focused && focused !== node) focused = focused.parent;
    if (focused === node) setFocusNode(node);
    rebuild();
  };

  const toggle = (node: TreeNode) => (node.expanded ? collapse(node) : void expand(node));

  /** Expands everything beneath a drive or folder, from the index in one answer. */
  const expandAll = async (node: TreeNode) => {
    if (node.kind === "pc") return;
    node.loading = true;
    changed();
    try {
      const text = await placesApi.treeSubtree(pathOf(node), showHidden());
      const stack: TreeNode[] = [node];
      node.children = [];
      for (const line of text.split("\n")) {
        if (!line) continue;
        const [depthText, hidden, holds, ...rest] = line.split("\t");
        const depth = Number(depthText);
        const parent = stack[depth - 1];
        if (!parent) continue;
        const child = folderNode({ name: rest.join("\t"), hidden: hidden === "1", children: holds === "?" ? null : holds === "1" }, parent);
        child.stamp = revision;
        if (holds === "1") {
          child.children = [];
          child.expanded = true;
        }
        parent.children!.push(child);
        stack[depth] = child;
        stack.length = depth + 1;
      }
      node.stamp = revision;
      node.hasChildren = node.children.length > 0;
      node.expanded = node.hasChildren;
    } catch (error) {
      notify(error);
    }
    node.loading = false;
    rebuild();
  };

  // ---------- finding and showing ----------

  /** The node for a path, walking down from This PC through what is loaded. */
  const nodeAt = (path: string): TreeNode | null => {
    if (samePlace(path, THIS_PC)) return root;
    const drive = root.children?.find((node) => samePlace(node.fixedPath!, path.slice(0, 3)));
    if (!drive) return null;
    let node: TreeNode = drive;
    const parts = path.replace(/\//g, "\\").slice(3).split("\\").filter(Boolean);
    for (const part of parts) {
      const next = node.children?.find((child) => child.name.toLowerCase() === part.toLowerCase());
      if (!next) return null;
      node = next;
    }
    return node;
  };

  const indexOf = (node: TreeNode | null) => (node ? rows.indexOf(node) : -1);

  /** Scrolls just far enough to show a row; `centre` puts one far out of view in the middle. */
  const reveal = (index: number, centre = false) => {
    if (index < 0) return;
    const top = index * ROW;
    const view = scroller.clientHeight;
    if (top >= scroller.scrollTop && top + ROW <= scroller.scrollTop + view) return;
    scroller.scrollTop = centre ? top - view / 2 + ROW / 2 : top < scroller.scrollTop ? top : top + ROW - view;
  };

  /** The folder the active tab shows, or This PC when it shows that; null otherwise. */
  const currentPath = createMemo(() => {
    if (projectsOpen() || diskOpen()) return null;
    const tab = activeTab();
    if (!tab) return null;
    if (samePlace(tab.path, THIS_PC)) return THIS_PC;
    if (isSearch(tab) && tab.query) return null;
    return tab.listing?.path ?? tab.path;
  });

  let revealing = 0;
  /**
   * Expands the tree down to the open folder and marks it, scrolling it into view if asked.
   * `always` does so even with "Expand to open folder" off, as Ctrl+Shift+E does.
   */
  const revealCurrent = async (scroll: boolean, always = false) => {
    const path = currentPath();
    if (!path || (!expandToOpen() && !always)) return;
    const request = ++revealing;
    if (path !== THIS_PC && /^[a-z]:\\/i.test(path)) {
      const listings = await placesApi.treeReveal(path, showHidden()).catch(() => [] as TreeListing[]);
      if (request !== revealing) return;
      const drive = root.children?.find((node) => samePlace(node.fixedPath!, path.slice(0, 3)));
      if (drive && listings.length > 0) {
        root.expanded = true;
        for (const listing of listings) {
          const node = nodeAt(listing.path);
          if (!node) break;
          merge(node, listing);
          if (node.children && node.children.length > 0) node.expanded = true;
        }
        rebuild();
      }
    }
    const target = nodeAt(path);
    if (!target) return;
    if (!hasFocus() || always) setFocusNode(target);
    if (scroll) reveal(indexOf(target), true);
  };

  /**
   * Ctrl+Shift+E (ParityLayer.tsx): Explorer's "Expand to current folder", which takes the
   * keyboard, unless the keyboard has moved on elsewhere while the tree was being expanded.
   */
  const onRevealRequest = () => {
    const holder = document.activeElement;
    void revealCurrent(true, true).then(() => {
      const target = nodeAt(currentPath() ?? THIS_PC);
      if (target) setFocusNode(target);
      if (document.activeElement === holder) tree.focus();
    });
  };

  createEffect(on([currentPath, expandToOpen], () => void revealCurrent(true)));

  // ---------- keeping up ----------

  /** Folders shown whose listing is older than the index: those in view are asked again. */
  let refreshing = false;
  const refreshVisible = async () => {
    if (refreshing) return;
    const stale = new Set<TreeNode>();
    for (const node of visible()) {
      const holder = node.parent;
      if (holder && holder.kind !== "pc" && holder.stamp < revision && holder.children !== null) stale.add(holder);
      if (node.kind !== "pc" && node.expanded && node.stamp < revision && node.children !== null) stale.add(node);
    }
    if (stale.size === 0) return;
    refreshing = true;
    // What is asked for now is as current as the index now; a change meanwhile asks again.
    const asked = revision;
    try {
      const nodes = [...stale];
      const listings = await placesApi.treeChildren(nodes.map(pathOf), showHidden());
      let moved = false;
      nodes.forEach((node, i) => {
        if (listings[i] && merge(node, listings[i], asked)) moved = true;
      });
      if (moved) rebuild();
      else changed();
    } catch (error) {
      console.error("folder tree:", error);
    } finally {
      refreshing = false;
    }
    if (revision !== asked) void refreshVisible();
  };

  /** Everything loaded is asked again, as when "Show hidden items" changes. */
  const reloadAll = () => {
    const loaded: TreeNode[] = [];
    const walk = (node: TreeNode) => {
      if (node.kind !== "pc" && node.children !== null && (node.expanded || node.children.length > 0)) loaded.push(node);
      node.children?.forEach(walk);
    };
    walk(root);
    // Folders folded away are forgotten and asked for when next expanded.
    const shown = loaded.filter((node) => node.expanded);
    loaded.filter((node) => !node.expanded).forEach((node) => (node.children = null));
    void load(shown);
  };
  // "Show hidden items" and "Hide protected operating system files" (Folder Options).
  createEffect(on([showHidden, showProtected], reloadAll, { defer: true }));

  // ---------- the view ----------

  const firstIndex = () => Math.max(0, Math.floor(viewTop() / ROW) - OVERSCAN);
  const lastIndex = () => Math.min(rows.length - 1, Math.ceil((viewTop() + viewHeight()) / ROW) + OVERSCAN);
  /** The rows drawn now, with where each goes. */
  const visible = createMemo(() => {
    version();
    const shown: TreeNode[] = [];
    for (let i = firstIndex(); i <= lastIndex(); i++) shown.push(rows[i]);
    return shown;
  });
  const positions = createMemo(() => {
    const map = new Map<TreeNode, number>();
    const first = firstIndex();
    visible().forEach((node, i) => map.set(node, first + i));
    return map;
  });

  createEffect(on(visible, () => void refreshVisible(), { defer: true }));

  onMount(() => {
    const measure = () => {
      setViewTop(scroller.scrollTop);
      setViewHeight(scroller.clientHeight);
    };
    const observer = new ResizeObserver(measure);
    observer.observe(scroller);
    scroller.addEventListener("scroll", measure, { passive: true });
    window.addEventListener("kestrel:reveal-in-tree", onRevealRequest);
    measure();
    const stopChanges = placesApi.onFoldersChanged((next) => {
      revision = next;
      void refreshVisible();
      // A folder opened before the index had it is revealed once it does.
      const path = currentPath();
      if (path && expandToOpen() && !nodeAt(path)) void revealCurrent(false);
    });
    onCleanup(() => {
      observer.disconnect();
      scroller.removeEventListener("scroll", measure);
      window.removeEventListener("kestrel:reveal-in-tree", onRevealRequest);
      clearTimeout(followTimer);
      void stopChanges.then((stop) => stop());
    });
    if (import.meta.env.DEV) {
      Object.assign(window, {
        __kestrelTree: {
          rows: () => rows.map((node) => ({ path: pathOf(node), depth: node.depth, expanded: node.expanded })),
          count: () => rows.length,
          expandAll: (path: string) => {
            const node = nodeAt(path);
            return node ? expandAll(node) : Promise.resolve();
          },
          scrollTo: (index: number) => void (scroller.scrollTop = index * ROW),
        },
      });
    }
  });

  // ---------- acting on rows ----------

  const open = (node: TreeNode, inNewTab = false) => {
    clearTimeout(followTimer);
    closeProjects();
    closeDiskUsage();
    openLocation(pathOf(node), inNewTab);
  };

  /** Moves the keyboard's place, and opens the folder once the keys rest there. */
  const focusAt = (index: number) => {
    const node = rows[Math.max(0, Math.min(rows.length - 1, index))];
    if (!node) return;
    setFocusNode(node);
    reveal(indexOf(node));
    clearTimeout(followTimer);
    followTimer = window.setTimeout(() => {
      const current = currentPath();
      if (focusNode() === node && (!current || !samePlace(current, pathOf(node)))) open(node);
    }, FOLLOW_MS);
  };

  const menuFor = (node: TreeNode, x: number, y: number) => {
    const path = pathOf(node);
    const dark = !window.matchMedia("(prefers-color-scheme: light)").matches;
    const entries: MenuEntry[] = [
      { label: "Open", run: () => open(node) },
      { label: "Open in new tab", run: () => open(node, true) },
    ];
    if (node.kind !== "pc" && node.hasChildren !== false) {
      entries.push("separator", node.expanded ? { label: "Collapse", run: () => collapse(node) } : { label: "Expand", run: () => void expand(node) });
    }
    const drive = node.drive;
    if (drive && (canAnalyse() || canEject(drive))) {
      entries.push("separator");
      if (canAnalyse()) entries.push({ label: "Show disk usage", run: () => analyseDiskUsage(path) });
      if (canEject(drive)) entries.push({ label: "Eject", run: () => void ejectDrive(drive) });
    }
    if (node.kind !== "pc") {
      entries.push(
        "separator",
        { label: "Pin to sidebar", run: () => void pinPaths([path]) },
        { label: "Properties", run: () => void api.showProperties(path).catch(notify) },
        "separator",
        { label: "Show more options", hint: "Shift+F10", run: () => void placesApi.shellPathsMenu([path], x, y, dark, false).catch(notify) },
      );
    }
    entries.push("separator", { label: "Expand to open folder", checked: expandToOpen(), run: () => setExpandToOpen(!expandToOpen()) });
    showMenu(x, y, entries);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    const node = focusNode() ?? rows[0];
    const at = Math.max(0, indexOf(node));
    const page = Math.max(1, Math.floor(scroller.clientHeight / ROW) - 1);
    const handled = () => {
      event.preventDefault();
      event.stopPropagation();
    };
    if (event.key.length !== 1) typeAhead.prefix = "";
    switch (event.key) {
      case "ArrowDown":
        return handled(), focusAt(at + 1);
      case "ArrowUp":
        return handled(), focusAt(at - 1);
      case "Home":
        return handled(), focusAt(0);
      case "End":
        return handled(), focusAt(rows.length - 1);
      case "PageDown":
        return handled(), focusAt(at + page);
      case "PageUp":
        return handled(), focusAt(at - page);
      case "ArrowRight":
        handled();
        if (node.expanded && node.children && node.children.length > 0) return focusAt(at + 1);
        return void expand(node);
      case "ArrowLeft":
        handled();
        if (node.expanded) return collapse(node);
        if (node.parent) return focusAt(indexOf(node.parent));
        return;
      case "Enter":
        handled();
        return open(node, event.ctrlKey);
      case "ContextMenu":
      case "F10": {
        if (event.key === "F10" && !event.shiftKey) return;
        handled();
        const box = tree.querySelector<HTMLElement>(`[data-row="${at}"]`)?.getBoundingClientRect();
        return menuFor(node, (box?.left ?? 0) + 40, (box?.bottom ?? 0) - 4);
      }
    }
    if ((event.key === "*" || event.code === "NumpadMultiply") && !event.ctrlKey && !event.altKey) {
      handled();
      return void expandAll(node);
    }
    if (event.key.length === 1 && event.key !== " " && !event.ctrlKey && !event.altKey && !event.metaKey) {
      handled();
      const now = performance.now();
      const letter = event.key.toLowerCase();
      const fresh = now - typeAhead.at > TYPE_AHEAD_RESET_MS || typeAhead.prefix === letter;
      typeAhead.prefix = fresh ? letter : typeAhead.prefix + letter;
      typeAhead.at = now;
      // A new letter looks after the current row; more letters of a name keep looking from it.
      const start = fresh ? at + 1 : at;
      for (let step = 0; step < rows.length; step++) {
        const index = (start + step) % rows.length;
        if (rows[index].name.toLowerCase().startsWith(typeAhead.prefix)) return focusAt(index);
      }
    }
  };

  return (
    <section class="tree-section" aria-label="This PC" classList={{ folded: !root.expanded && version() >= 0 }}>
      <div ref={scroller} class="tree-scroller">
        <div
          ref={tree}
          class="tree"
          role="tree"
          aria-label="Folders"
          tabindex="0"
          style={{ height: `${(version(), rows.length) * ROW}px` }}
          onKeyDown={onKeyDown}
          onFocus={() => {
            setHasFocus(true);
            if (!focusNode()) setFocusNode(nodeAt(currentPath() ?? THIS_PC) ?? root);
          }}
          onBlur={() => setHasFocus(false)}
        >
          <For each={visible()}>
            {(node) => {
              const path = () => (version(), pathOf(node));
              const index = () => positions().get(node) ?? 0;
              const current = () => {
                const now = currentPath();
                return now !== null && samePlace(now, path());
              };
              const chevron = () => (version(), node.kind === "pc" || node.hasChildren !== false);
              return (
                <div
                  class="tree-row"
                  classList={{
                    current: current(),
                    focused: focusNode() === node && hasFocus(),
                    dimmed: (version(), node.hidden),
                    "drop-target": node.kind !== "pc" && isDropPlace(path()),
                  }}
                  role="treeitem"
                  aria-level={node.depth + 1}
                  aria-expanded={chevron() ? (version(), node.expanded) : undefined}
                  aria-selected={current()}
                  data-row={index()}
                  data-tree-path={path()}
                  data-place={node.kind === "pc" ? undefined : path()}
                  data-place-name={node.kind === "pc" ? undefined : (version(), node.name)}
                  title={(version(), node.error) ?? path()}
                  style={{ transform: `translateY(${index() * ROW}px)`, "padding-left": `${6 + node.depth * INDENT}px` }}
                  onMouseDown={(event) => {
                    if (event.button === 0) setFocusNode(node);
                  }}
                  onClick={(event) => {
                    if ((event.target as Element).closest(".tree-chevron")) return;
                    open(node, event.ctrlKey);
                  }}
                  onAuxClick={(event) => {
                    if (event.button === 1) open(node, true);
                  }}
                  onDblClick={(event) => {
                    if (!(event.target as Element).closest(".tree-chevron")) toggle(node);
                  }}
                  onContextMenu={(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    setFocusNode(node);
                    if (event.shiftKey && node.kind !== "pc") {
                      const dark = !window.matchMedia("(prefers-color-scheme: light)").matches;
                      void placesApi.shellPathsMenu([path()], event.clientX, event.clientY, dark, true).catch(notify);
                    } else {
                      menuFor(node, event.clientX, event.clientY);
                    }
                  }}
                >
                  <span
                    class="tree-chevron"
                    classList={{ open: (version(), node.expanded), none: !chevron(), busy: (version(), node.loading) }}
                    onClick={(event) => {
                      event.stopPropagation();
                      toggle(node);
                    }}
                  >
                    <Show when={chevron()}>
                      <Icon name="chevron" size={12} />
                    </Show>
                  </span>
                  <Icon name={iconFor(node)} />
                  <span class="tree-label">{(version(), node.name)}</span>
                </div>
              );
            }}
          </For>
        </div>
      </div>
    </section>
  );
}
