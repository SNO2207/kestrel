/**
 * Registers the Developer tools page as a special location (lib/locations.ts): the terminal's
 * settings, the query service, and how to add Kestrel's MCP server to Claude Code. Imported by
 * App.tsx; the palette lists it with the other locations.
 */
import { DevToolsPage } from "./components/DevToolsPage";
import { DEVELOPER_TOOLS } from "./devtools";
import { registerLocation } from "./lib/locations";

registerLocation({
  address: DEVELOPER_TOOLS,
  title: "Developer tools",
  icon: "terminal",
  page: DevToolsPage,
  aliases: ["kestrel:devtools", "kestrel:developer-tools"],
  keywords: "settings terminal shell powershell mcp claude code command line cli query service pipe",
});
