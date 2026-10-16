/**
 * Collapsible sidebar sections. Any `section` with an `aria-label` and an `h2` heading folds
 * away when its heading is clicked (or Enter or Space pressed on it), and is remembered folded
 * (`places.ts`, kept in kestrel.db). It works from the DOM, so the sections other features
 * draw (pins, frequent folders, saved searches, tags, workspaces) need nothing of their own:
 * a new section is collapsible as soon as it has a label and a heading.
 */
import { createEffect, onCleanup } from "solid-js";

const SECTIONS = "section[aria-label]";

/** The section whose heading an event is on, if any; buttons in the heading keep their own clicks. */
function headingSection(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof Element) || target.closest("button")) return null;
  const heading = target.closest("h2");
  const section = heading?.closest<HTMLElement>(SECTIONS);
  return section && section.querySelector("h2") === heading ? section : null;
}

/**
 * Makes the sections inside `root` collapsible. `collapsed` says which are folded, by label;
 * `setCollapsed` is told when one is folded or unfolded. Call inside a component.
 */
export function collapsibleSections(
  root: HTMLElement,
  collapsed: () => ReadonlySet<string>,
  setCollapsed: (section: string, folded: boolean) => void,
) {
  const apply = () => {
    const folded = collapsed();
    for (const section of root.querySelectorAll<HTMLElement>(SECTIONS)) {
      const heading = section.querySelector("h2");
      if (!heading) continue;
      const name = section.getAttribute("aria-label") ?? "";
      const closed = folded.has(name);
      section.classList.toggle("collapsed", closed);
      section.classList.add("collapsible");
      heading.setAttribute("role", "button");
      heading.setAttribute("tabindex", "0");
      heading.setAttribute("aria-expanded", String(!closed));
      heading.title = closed ? `Show ${name}` : `Hide ${name}`;
    }
  };
  const toggle = (section: HTMLElement) => {
    const name = section.getAttribute("aria-label") ?? "";
    setCollapsed(name, !collapsed().has(name));
  };
  const onClick = (event: MouseEvent) => {
    const section = headingSection(event.target);
    if (section && event.button === 0) toggle(section);
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    const section = headingSection(event.target);
    if (!section) return;
    event.preventDefault();
    toggle(section);
  };
  // Sections come and go (Frequent appears with its first folder), so new ones are set up too.
  let queued = false;
  const observer = new MutationObserver(() => {
    if (queued) return;
    queued = true;
    queueMicrotask(() => {
      queued = false;
      apply();
    });
  });
  observer.observe(root, { childList: true, subtree: true });
  root.addEventListener("click", onClick);
  root.addEventListener("keydown", onKeyDown);
  createEffect(apply);
  onCleanup(() => {
    observer.disconnect();
    root.removeEventListener("click", onClick);
    root.removeEventListener("keydown", onKeyDown);
  });
}
