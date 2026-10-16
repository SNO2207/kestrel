import type { Row } from "../lib/api";
import { formatDate } from "../lib/format";
import { formatLength } from "../lib/mediaApi";
import { factsOf } from "../lib/mediaFacts";

export type MediaColumn = "taken" | "dimensions" | "duration";

/** A details row's Date taken, Dimensions or Duration, from the media library. */
export function MediaCell(props: { column: MediaColumn; row: Row }) {
  const text = () => {
    const facts = factsOf(props.row);
    if (!facts) return "";
    switch (props.column) {
      case "taken":
        return facts.taken ? formatDate(facts.taken) : "";
      case "dimensions":
        return facts.width && facts.height ? `${facts.width} × ${facts.height}` : "";
      case "duration":
        return facts.duration_ms ? formatLength(facts.duration_ms / 1000) : "";
    }
  };
  return <div class={`cell ${props.column}`}>{text()}</div>;
}
