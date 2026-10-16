/**
 * The terminal pane, docked at the bottom of the window: a strip of terminal tabs with New
 * terminal, the shells to choose from, "cd to this folder" and Hide, above the terminals
 * themselves (xterm.js, each talking to a shell in a pseudoconsole: `devtools/terminal.rs`).
 *
 * Keys typed into a terminal stay there: they never reach the window's own shortcuts (Backspace
 * would go back, Ctrl+W close the tab), apart from the few in `passesThrough` and the captured
 * Ctrl+K and Ctrl+`. Menus are drawn outside the terminals' box, whose key listener stops
 * events before Solid's delegated handlers see them.
 *
 * xterm.js is loaded when the first terminal opens, so the window starts without it.
 */
import type { ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { For, Show, createEffect, createSignal, on, onCleanup, onMount } from "solid-js";

import {
  MIN_HEIGHT,
  activeKey,
  cdToFolder,
  closeTerminal,
  currentFolder,
  focusTerminal,
  handleOf,
  hideTerminal,
  maxHeight,
  openDeveloperTools,
  openTerminal,
  paneHeight,
  paneShown,
  passesThrough,
  patchTerminal,
  registerTerminal,
  setActiveKey,
  setPaneHeight,
  terminals,
  terminalsReady,
  type TerminalTab,
} from "../devtools";
import { devtoolsApi, type Shell } from "../lib/devtoolsApi";
import { findPaths } from "../lib/terminalLinks";
import { revealInFolder } from "../parity";
import { activeTab, navigate, notify } from "../state";
import { ContextMenu, type MenuEntry } from "./ContextMenu";
import { Icon } from "./Icon";

const FONT = '"Cascadia Mono", "Cascadia Code", Consolas, "Courier New", monospace';

/** ANSI colours that read on each theme's surface; the rest comes from the app's tokens. */
const PALETTES: Record<"dark" | "light", ITheme> = {
  dark: {
    black: "#2c3541",
    red: "#e5484d",
    green: "#4cc38a",
    yellow: "#f0b429",
    blue: "#6f9fe0",
    magenta: "#c08ef0",
    cyan: "#4fb8c4",
    white: "#c5ccd6",
    brightBlack: "#6b7686",
    brightRed: "#ff6b70",
    brightGreen: "#6fdca5",
    brightYellow: "#ffd166",
    brightBlue: "#93b8f0",
    brightMagenta: "#d4adff",
    brightCyan: "#76d6e0",
    brightWhite: "#f2f4f7",
  },
  light: {
    black: "#1a2029",
    red: "#c62828",
    green: "#2e7d32",
    yellow: "#8a5d00",
    blue: "#2f5f9e",
    magenta: "#8e3fb4",
    cyan: "#0b7a87",
    white: "#5a6473",
    brightBlack: "#6b7483",
    brightRed: "#d84343",
    brightGreen: "#3c9a40",
    brightYellow: "#a06f00",
    brightBlue: "#3f74c0",
    brightMagenta: "#a456cc",
    brightCyan: "#12909f",
    brightWhite: "#8892a1",
  },
};

const lightScheme = window.matchMedia("(prefers-color-scheme: light)");

function theme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string) => style.getPropertyValue(name).trim();
  const scheme = lightScheme.matches ? "light" : "dark";
  return {
    ...PALETTES[scheme],
    background: token("--surface"),
    foreground: token("--text"),
    cursor: token("--accent"),
    cursorAccent: token("--surface"),
    selectionBackground: scheme === "light" ? "rgba(240, 180, 41, 0.35)" : "rgba(240, 180, 41, 0.28)",
    scrollbarSliderBackground: scheme === "light" ? "rgba(26, 32, 41, 0.2)" : "rgba(227, 231, 237, 0.18)",
    scrollbarSliderHoverBackground: scheme === "light" ? "rgba(26, 32, 41, 0.32)" : "rgba(227, 231, 237, 0.3)",
  };
}

// ---------- paths in the output ----------

type Kind = "folder" | "file" | null;
/** What paths are, as the backend said, for a few seconds: links are asked for on every hover. */
const known = new Map<string, { kind: Kind; at: number }>();
const KNOWN_FOR = 5000;

async function kindsOf(paths: string[]): Promise<Map<string, Kind>> {
  const now = Date.now();
  const ask = paths.filter((path) => (known.get(path)?.at ?? 0) < now - KNOWN_FOR);
  if (ask.length > 0) {
    const answers = await devtoolsApi.paths(ask);
    ask.forEach((path, at) => known.set(path, { kind: answers[at] ?? null, at: now }));
  }
  return new Map(paths.map((path) => [path, known.get(path)?.kind ?? null]));
}

/** Opens a path printed in a terminal in Kestrel: a folder in the active tab, a file selected in its folder. */
function openInKestrel(path: string, kind: Kind) {
  const tab = activeTab();
  if (!tab) return;
  if (kind === "folder") navigate(tab.id, path);
  else revealInFolder(path);
}

// ---------- one terminal ----------

type Xterm = typeof import("@xterm/xterm");
type Fit = typeof import("@xterm/addon-fit");
let loading: Promise<[Xterm, Fit]> | null = null;
/** xterm.js and its fit addon, loaded once, when the first terminal opens. */
const loadXterm = () => (loading ??= Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]));

function TerminalView(props: { tab: TerminalTab; visible: boolean; onMenu: (x: number, y: number) => void; focusList: () => void }) {
  let host!: HTMLDivElement;
  // Read once: a tab keeps the key it was made with.
  const key = props.tab.key;

  onMount(() => {
    let stop: (() => void) | null = null;
    let gone = false;
    loadXterm()
      .then(([{ Terminal }, { FitAddon }]) => {
        if (!gone) stop = start(Terminal, FitAddon);
      })
      .catch((error) => notify(error));
    onCleanup(() => {
      gone = true;
      stop?.();
    });
  });

  /** Starts the terminal and its shell; returns what ends them. */
  const start = (Terminal: Xterm["Terminal"], FitAddon: Fit["FitAddon"]): (() => void) => {
    const term = new Terminal({
      fontFamily: FONT,
      fontSize: 13,
      lineHeight: 1.15,
      cursorBlink: true,
      scrollback: 5000,
      theme: theme(),
      // ConPTY reflows lines itself; xterm leaves that to it.
      windowsPty: { backend: "conpty", buildNumber: 22621 },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    // A new terminal is the active one, and takes the keyboard as soon as it exists (xterm.js
    // may still have been loading when it was asked to).
    if (activeKey() === key && paneShown()) term.focus();

    let session: number | null = null;
    let disposed = false;
    /**
     * Keys typed and not yet sent. One write is in flight at a time, carrying everything typed
     * while the last was: commands run on a pool of threads, so writes sent one per key could
     * reach the shell out of order. Keys typed before the shell has started wait here too.
     */
    let typed = "";
    let writing = false;
    const flush = () => {
      if (writing || session === null || typed === "") return;
      const data = typed;
      typed = "";
      writing = true;
      void devtoolsApi
        .write(session, data)
        .catch(() => {})
        .finally(() => {
          writing = false;
          flush();
        });
    };

    const fitNow = () => {
      if (disposed || host.offsetWidth === 0 || host.offsetHeight === 0) return;
      try {
        fit.fit();
      } catch {
        // Not measurable yet (hidden); fitted again when shown.
      }
    };
    fitNow();

    const copy = () => {
      const text = term.getSelection();
      if (text) void navigator.clipboard.writeText(text).catch(notify);
      term.clearSelection();
    };
    const paste = async () => {
      const text = await devtoolsApi.clipboardText().catch(() => null);
      if (text) term.paste(text);
      term.focus();
    };
    const unregister = registerTerminal(key, {
      focus: () => term.focus(),
      fit: fitNow,
      copy,
      hasSelection: () => term.hasSelection(),
      size: () => ({ cols: term.cols, rows: term.rows }),
      paste: () => void paste(),
      selectAll: () => term.selectAll(),
      clear: () => term.clear(),
      text: () => {
        // Lines the terminal wrapped are joined again, as the shell wrote them.
        const buffer = term.buffer.active;
        const lines: string[] = [];
        for (let y = 0; y < buffer.length; y++) {
          const line = buffer.getLine(y);
          // A line that goes on in the next keeps its trailing spaces.
          const text = line?.translateToString(!buffer.getLine(y + 1)?.isWrapped) ?? "";
          if (line?.isWrapped && lines.length > 0) lines[lines.length - 1] += text;
          else lines.push(text);
        }
        return lines.join("\n").replace(/\n+$/, "");
      },
    });

    term.attachCustomKeyEventHandler((event) => {
      if (passesThrough(event)) return false;
      if (event.type !== "keydown") return true;
      const pressed = event.key.toLowerCase();
      const ctrl = event.ctrlKey && !event.altKey && !event.metaKey;
      // Ctrl+C copies a selection (and interrupts otherwise); Ctrl+Shift+C always copies.
      if (ctrl && pressed === "c" && (event.shiftKey || term.hasSelection())) {
        event.preventDefault();
        copy();
        return false;
      }
      // Ctrl+V and Ctrl+Shift+V paste through the web view's own paste, which xterm takes.
      if (ctrl && pressed === "v") return false;
      return true;
    });

    term.onData((data) => {
      typed += data;
      flush();
    });
    // Sizes go the same way, one at a time, so the shell ends with the last one.
    let sized = "";
    let sizing = false;
    const sendSize = () => {
      const wanted = `${term.cols}x${term.rows}`;
      if (sizing || session === null || wanted === sized) return;
      sized = wanted;
      sizing = true;
      void devtoolsApi
        .resize(session, term.cols, term.rows)
        .catch(() => {})
        .finally(() => {
          sizing = false;
          sendSize();
        });
    };
    term.onResize(sendSize);

    term.registerLinkProvider({
      provideLinks(y, callback) {
        const line = term.buffer.active.getLine(y - 1)?.translateToString(true) ?? "";
        const matches = findPaths(line);
        if (matches.length === 0) return callback(undefined);
        const paths = [...new Set(matches.flatMap((match) => match.candidates.map((candidate) => candidate.path)))];
        kindsOf(paths)
          .then((kinds) => {
            const links = matches.flatMap((match) => {
              const hit = match.candidates.find((candidate) => kinds.get(candidate.path));
              if (!hit) return [];
              const kind = kinds.get(hit.path) ?? null;
              return [
                {
                  range: { start: { x: match.start + 1, y }, end: { x: match.start + hit.length, y } },
                  text: hit.path,
                  decorations: { pointerCursor: true, underline: true },
                  activate: () => openInKestrel(hit.path, kind),
                  hover: () => (host.title = kind === "folder" ? "Open this folder in Kestrel" : "Show this file in Kestrel"),
                  leave: () => host.removeAttribute("title"),
                },
              ];
            });
            callback(links.length > 0 ? links : undefined);
          })
          .catch(() => callback(undefined));
      },
    });

    const onExit = (code: number | null) => {
      if (disposed) return;
      patchTerminal(key, { exited: code });
      if (code === 0) closeTerminal(key, props.focusList);
      else term.write(`\r\n\x1b[2m[The shell ended${code === null ? "" : ` with exit code ${code}`}. Close this tab to remove it.]\x1b[0m\r\n`);
    };

    void terminalsReady
      .then(() => {
        // The size the pseudoconsole starts with; any change after it is sent once it runs.
        sized = `${term.cols}x${term.rows}`;
        return devtoolsApi.open(
          { cwd: props.tab.cwd, shell: props.tab.shell, script: props.tab.script, cols: term.cols, rows: term.rows },
          (event) => {
            if (disposed) return;
            if (event.kind === "output") term.write(event.data);
            else onExit(event.code);
          },
        );
      })
      .then((opened) => {
        if (disposed) {
          void devtoolsApi.close(opened.id).catch(() => {});
          return;
        }
        session = opened.id;
        patchTerminal(key, { session: opened.id, pid: opened.pid, title: opened.title, shell: opened.shell });
        flush();
        // The box may have changed while the shell started.
        fitNow();
        sendSize();
      })
      .catch((error) => {
        if (disposed) return;
        term.write(`\x1b[31m${String(error)}\x1b[0m\r\n`);
        patchTerminal(key, { exited: null, title: "Couldn't start" });
      });

    // Follows the pane's size, and the box appearing when the pane or the tab is shown.
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fitNow);
    });
    observer.observe(host);
    const onScheme = () => (term.options.theme = theme());
    lightScheme.addEventListener("change", onScheme);

    // Keys typed into the terminal are the shell's, not the window's.
    const keep = (event: KeyboardEvent) => {
      if (!passesThrough(event)) event.stopPropagation();
    };
    host.addEventListener("keydown", keep);
    const onContextMenu = (event: MouseEvent) => {
      event.preventDefault();
      props.onMenu(event.clientX, event.clientY);
    };
    host.addEventListener("contextmenu", onContextMenu);

    return () => {
      disposed = true;
      unregister();
      observer.disconnect();
      cancelAnimationFrame(frame);
      lightScheme.removeEventListener("change", onScheme);
      host.removeEventListener("keydown", keep);
      host.removeEventListener("contextmenu", onContextMenu);
      if (session !== null) void devtoolsApi.close(session).catch(() => {});
      term.dispose();
    };
  };

  return <div ref={host} class="terminal-host" classList={{ shown: props.visible }} data-terminal-key={props.tab.key} />;
}

// ---------- the pane ----------

export function TerminalPane(props: { focusList: () => void }) {
  const [menu, setMenu] = createSignal<{ x: number; y: number; entries: MenuEntry[] } | null>(null);
  const [shells, setShells] = createSignal<Shell[]>([]);
  const [dragging, setDragging] = createSignal(false);

  createEffect(() => document.documentElement.style.setProperty("--terminal-height", `${paneHeight()}px`));
  // Shown again, or another tab chosen: the terminal fits its box, which was hidden.
  createEffect(
    on([paneShown, activeKey], ([shown, key]) => {
      if (shown) requestAnimationFrame(() => handleOf(key)?.fit());
    }),
  );

  onMount(() => {
    void devtoolsApi.shells().then((list) => setShells(list.shells)).catch(() => {});
    // The window grew shorter: the pane keeps within it.
    const onResize = () => paneHeight() > maxHeight() && setPaneHeight(maxHeight(), false);
    window.addEventListener("resize", onResize);
    onCleanup(() => window.removeEventListener("resize", onResize));
  });

  const startDrag = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const startY = event.clientY;
    const startHeight = paneHeight();
    handle.setPointerCapture(event.pointerId);
    setDragging(true);
    event.preventDefault();
    const move = (moved: PointerEvent) => setPaneHeight(startHeight + (startY - moved.clientY), false);
    const end = () => {
      setDragging(false);
      setPaneHeight(paneHeight(), true);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };

  const onHandleKey = (event: KeyboardEvent) => {
    const step = event.key === "ArrowUp" ? 24 : event.key === "ArrowDown" ? -24 : 0;
    if (step === 0) return;
    event.preventDefault();
    setPaneHeight(paneHeight() + step, true);
  };

  const shellMenu = (anchor: HTMLElement) => {
    const box = anchor.getBoundingClientRect();
    const entries: MenuEntry[] = [
      ...shells().map((shell) => ({ label: `New ${shell.name} terminal`, run: () => void openTerminal({ shell: shell.id }) })),
      "separator",
      { label: "Developer tools settings", run: () => openDeveloperTools() },
    ];
    setMenu({ x: box.left, y: box.bottom + 4, entries });
  };

  const terminalMenu = (x: number, y: number) => {
    const key = activeKey();
    const handle = handleOf(key);
    const tab = terminals.find((each) => each.key === key);
    const entries: MenuEntry[] = [
      { label: "Copy", hint: "Ctrl+C", disabled: !handle?.hasSelection(), run: () => handle?.copy() },
      { label: "Paste", hint: "Ctrl+V", run: () => handle?.paste() },
      { label: "Select all", run: () => handle?.selectAll() },
      { label: "Clear", run: () => handle?.clear() },
      "separator",
      { label: "cd to this folder", disabled: !currentFolder() || !tab?.session || tab.exited !== undefined, run: () => void cdToFolder() },
      "separator",
      { label: "Close terminal", run: () => key !== null && closeTerminal(key, props.focusList) },
    ];
    setMenu({ x, y, entries });
  };

  const closeMenu = () => {
    setMenu(null);
    focusTerminal();
  };

  /** A tab's name; the second of the same shell is "Windows PowerShell 2", and so on. */
  const label = (tab: TerminalTab) => {
    const same = terminals.filter((other) => other.title === tab.title);
    const at = same.findIndex((other) => other.key === tab.key);
    return at > 0 ? `${tab.title} ${at + 1}` : tab.title;
  };

  return (
    <Show when={terminals.length > 0}>
      <section class="terminal-pane" classList={{ dragging: dragging() }} hidden={!paneShown()} aria-label="Terminal">
        <div
          class="terminal-resizer"
          role="separator"
          aria-orientation="horizontal"
          aria-label="Resize the terminal"
          aria-valuenow={paneHeight()}
          aria-valuemin={MIN_HEIGHT}
          tabindex="0"
          title="Drag to resize"
          onPointerDown={startDrag}
          onKeyDown={onHandleKey}
        />
        <div class="terminal-bar">
          <div class="terminal-tabs" role="tablist" aria-label="Terminals">
            <For each={terminals}>
              {(tab) => (
                <div
                  class="terminal-tab"
                  classList={{ active: tab.key === activeKey(), ended: tab.exited !== undefined }}
                  role="tab"
                  aria-selected={tab.key === activeKey()}
                  data-terminal-key={tab.key}
                  title={tab.cwd ?? undefined}
                  onMouseDown={(event) => {
                    // Middle-click closes, as tabs elsewhere do.
                    if (event.button === 1) {
                      event.preventDefault();
                      closeTerminal(tab.key, props.focusList);
                    }
                  }}
                  onClick={() => {
                    setActiveKey(tab.key);
                    focusTerminal();
                  }}
                >
                  <Icon name="terminal" size={14} />
                  <span class="terminal-tab-title">{label(tab)}</span>
                  <button
                    class="terminal-tab-close"
                    aria-label={`Close ${tab.title}`}
                    title="Close terminal"
                    onClick={(event) => {
                      event.stopPropagation();
                      closeTerminal(tab.key, props.focusList);
                    }}
                  >
                    <Icon name="close" size={12} />
                  </button>
                </div>
              )}
            </For>
            <button class="terminal-button terminal-new" title="New terminal (Ctrl+Shift+`)" aria-label="New terminal" onClick={() => void openTerminal()}>
              <Icon name="plus" size={14} />
            </button>
            <button class="terminal-button terminal-shells" title="Choose a shell" aria-label="Choose a shell" onClick={(event) => shellMenu(event.currentTarget)}>
              <Icon name="chevron" size={12} />
            </button>
          </div>
          <div class="terminal-actions">
            <button
              class="terminal-button terminal-cd"
              title={currentFolder() ? `Change the terminal to ${currentFolder()}` : "Open a folder to change the terminal to it"}
              disabled={!currentFolder() || !terminals.find((tab) => tab.key === activeKey())?.session}
              onClick={() => void cdToFolder()}
            >
              <Icon name="folder" size={14} />
              <span>cd to this folder</span>
            </button>
            <button class="terminal-button terminal-hide" title="Hide the terminal (Ctrl+`)" aria-label="Hide the terminal" onClick={() => hideTerminal(props.focusList)}>
              <Icon name="minimize" size={14} />
            </button>
          </div>
        </div>
        <div class="terminal-body">
          <For each={terminals}>
            {(tab) => <TerminalView tab={tab} visible={tab.key === activeKey()} onMenu={terminalMenu} focusList={props.focusList} />}
          </For>
        </div>
        <Show when={menu()}>{(open) => <ContextMenu x={open().x} y={open().y} entries={open().entries} onClose={closeMenu} />}</Show>
      </section>
    </Show>
  );
}
