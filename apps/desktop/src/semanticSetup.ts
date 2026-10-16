/**
 * Registers the AI search page as a special location (lib/locations.ts), so it opens from the
 * address bar ("AI search"), the palette and the describe bar, in either pane and in history.
 * Imported by App.tsx.
 */
import { AiSearchPage } from "./components/AiSearchPage";
import { registerLocation } from "./lib/locations";
import { AI_SEARCH } from "./semantic";

registerLocation({
  address: AI_SEARCH,
  title: "AI search",
  icon: "sparkle",
  page: AiSearchPage,
  aliases: ["AI search"],
  keywords: "describe photos videos pictures models siglip semantic similar settings",
});
