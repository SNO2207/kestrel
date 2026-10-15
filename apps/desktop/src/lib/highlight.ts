import type { HighlightReply, HighlightRequest } from "./highlight.worker";

let worker: Worker | null = null;
let nextId = 1;
const waiting = new Map<number, (html: string) => void>();

/** The worker, started on first use so highlight.js loads only when code is first shown. */
function highlighter(): Worker {
  if (!worker) {
    worker = new Worker(new URL("./highlight.worker.ts", import.meta.url), { type: "module" });
    worker.onmessage = (event: MessageEvent<HighlightReply>) => {
      waiting.get(event.data.id)?.(event.data.html);
      waiting.delete(event.data.id);
    };
  }
  return worker;
}

/** Highlights code in the background. Resolves to escaped HTML that is safe to insert. */
export function highlight(code: string, language: string): Promise<string> {
  const id = nextId++;
  return new Promise((resolve) => {
    waiting.set(id, resolve);
    highlighter().postMessage({ id, code, language } satisfies HighlightRequest);
  });
}
