import { createEffect, onCleanup, onMount } from "solid-js";

import { claimSurface, surfaceUrl, type SurfaceKind } from "../lib/previewSurface";

/**
 * The place in a preview where an image, a video, a sound or a PDF shows. The content itself
 * is drawn by the preview's own webview, laid over this element (see `lib/previewSurface.ts`);
 * the element only holds the space and says what is shown there.
 */
export function PreviewSurface(props: {
  kind: SurfaceKind;
  path: string;
  version: number;
  download: boolean;
  /** Quick Look's preview wins the webview over the pane's. */
  priority: number;
  class: string;
  label: string;
}) {
  let element!: HTMLDivElement;
  const url = () => surfaceUrl(props.kind, props.path, props.version, props.download);
  onMount(() => {
    const claim = claimSurface(element, url(), props.priority, props.kind);
    createEffect(() => claim.setUrl(url()));
    onCleanup(() => claim.release());
  });
  return (
    <div
      ref={element}
      class={`preview-surface ${props.class}`}
      role="img"
      aria-label={props.label}
      data-kind={props.kind}
      data-src={url()}
    />
  );
}
