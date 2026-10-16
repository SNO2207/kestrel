/**
 * Registers the media library's two pages as special locations (lib/locations.ts): the Gallery,
 * under the names Explorer's address bar takes for its own, and the library's settings page,
 * which sits under it. Imported by App.tsx, and by the quick launcher, whose palette lists them.
 */
import { GalleryPage } from "./components/Gallery";
import { MediaLibraryPage } from "./components/MediaLibrary";
import { registerLocation } from "./lib/locations";
import { GALLERY, GALLERY_CLSID, MEDIA_LIBRARY } from "./media";

registerLocation({
  address: GALLERY,
  title: "Gallery",
  icon: "pictures",
  page: GalleryPage,
  aliases: ["Gallery\\", `shell:::${GALLERY_CLSID}`, `::${GALLERY_CLSID}`],
  keywords: "photos pictures videos media timeline camera",
});

registerLocation({
  address: MEDIA_LIBRARY,
  title: "Media library",
  icon: "pictures",
  page: MediaLibraryPage,
  aliases: ["Media library settings", "Gallery settings"],
  parent: () => GALLERY,
  crumbs: () => [
    { label: "Gallery", path: GALLERY, icon: "pictures" },
    { label: "Media library", path: MEDIA_LIBRARY },
  ],
  keywords: "gallery settings folders scopes pause resume text recognition ocr",
});
