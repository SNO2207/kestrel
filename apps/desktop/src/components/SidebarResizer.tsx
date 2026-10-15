import { createSignal, onMount } from "solid-js";

const WIDTH_KEY = "kestrel.sidebar-width";
const DEFAULT_WIDTH = 228;
const MIN_WIDTH = 180;
const MAX_WIDTH = 520;
const KEY_STEP = 16;

function storedWidth(): number {
  try {
    const width = Number(localStorage.getItem(WIDTH_KEY));
    return Number.isFinite(width) && width >= MIN_WIDTH ? Math.min(width, MAX_WIDTH) : DEFAULT_WIDTH;
  } catch {
    return DEFAULT_WIDTH;
  }
}

/**
 * The sidebar's right edge, dragged to widen it, as Explorer's navigation pane is, so a deep
 * folder tree can show its names. Arrow keys move it too; a double-click puts it back. The
 * width lives in `--sidebar-width` on the root, which the window's grid reads (places.css),
 * and is remembered in this window's storage.
 */
export function SidebarResizer() {
  const [width, setWidthSignal] = createSignal(storedWidth());
  const [dragging, setDragging] = createSignal(false);

  const setWidth = (next: number, persist: boolean) => {
    const clamped = Math.round(Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, next, window.innerWidth - 360)));
    setWidthSignal(clamped);
    document.documentElement.style.setProperty("--sidebar-width", `${clamped}px`);
    if (!persist) return;
    try {
      localStorage.setItem(WIDTH_KEY, String(clamped));
    } catch {
      // Remembered for this session only.
    }
  };

  onMount(() => setWidth(width(), false));

  const startDrag = (event: PointerEvent) => {
    if (event.button !== 0) return;
    const handle = event.currentTarget as HTMLElement;
    const startX = event.clientX;
    const startWidth = width();
    handle.setPointerCapture(event.pointerId);
    setDragging(true);
    event.preventDefault();
    const move = (moved: PointerEvent) => setWidth(startWidth + moved.clientX - startX, false);
    const end = () => {
      setDragging(false);
      setWidth(width(), true);
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", end);
      handle.removeEventListener("pointercancel", end);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", end);
    handle.addEventListener("pointercancel", end);
  };

  return (
    <div
      class="sidebar-resizer"
      classList={{ dragging: dragging() }}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the sidebar"
      aria-valuenow={width()}
      aria-valuemin={MIN_WIDTH}
      aria-valuemax={MAX_WIDTH}
      tabindex="0"
      title="Drag to resize. Double-click to reset."
      onPointerDown={startDrag}
      // A drag leaves the keyboard where it was; Tab still reaches the handle.
      onMouseDown={(event) => event.preventDefault()}
      onDblClick={() => setWidth(DEFAULT_WIDTH, true)}
      onKeyDown={(event) => {
        const step = event.key === "ArrowRight" ? KEY_STEP : event.key === "ArrowLeft" ? -KEY_STEP : 0;
        if (step === 0) return;
        event.preventDefault();
        event.stopPropagation();
        setWidth(width() + step, true);
      }}
    />
  );
}
