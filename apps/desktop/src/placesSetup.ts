/**
 * Registers Home, This PC and the Recycle Bin as special locations (lib/locations.ts), with
 * the names Explorer's address bar takes for them, and starts what places need. Imported by
 * App.tsx, and by the quick launcher, whose palette lists them too.
 */
import { HomeView } from "./components/HomeView";
import { RecycleBinPage } from "./components/RecycleBin";
import { ThisPcPage } from "./components/ThisPC";
import { HOME_ALIASES, HOME_PATH } from "./lib/home";
import { registerLocation } from "./lib/locations";
import { RECYCLE_BIN, THIS_PC, isDriveRoot } from "./places";

// Explorer's start page: quick access, favorites and recent files (HomeView.tsx).
registerLocation({
  address: HOME_PATH,
  title: "Home",
  icon: "home",
  page: HomeView,
  aliases: HOME_ALIASES,
  keywords: "start page quick access favorites recent",
});

registerLocation({
  address: THIS_PC,
  title: "This PC",
  icon: "pc",
  page: ThisPcPage,
  aliases: [
    "This PC",
    "Computer",
    "My Computer",
    "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
    "shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
  ],
  // Drives sit under This PC: Up from C:\ comes here, and its crumb leads every drive's path.
  adopts: isDriveRoot,
  keywords: "computer drives disks devices",
});

registerLocation({
  address: RECYCLE_BIN,
  title: "Recycle Bin",
  icon: "bin",
  page: RecycleBinPage,
  aliases: ["Recycle Bin", "::{645FF040-5081-101B-9F08-00AA002F954E}", "shell:::{645FF040-5081-101B-9F08-00AA002F954E}"],
  keywords: "trash deleted restore",
});

export { startPlaces } from "./places";
