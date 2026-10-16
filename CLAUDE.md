# Kestrel

A fast Windows 11 file explorer: Rust engine, Tauri 2 shell, SolidJS front end. See
`README.md` for layout, commands, search syntax and shortcuts.

The approved build plan is at
`C:\Users\levil\.claude\plans\pasted-content-id-3bd2-i-want-tranquil-whale.md`.
The repository is private on GitHub: https://github.com/SNO2207/kestrel (branch `main`).

## Status

Done and verified:

- **Index engine** (`kestrel-index`, `kestrel-query`): parallel directory crawl, snapshot,
  query language, search, sort. Measured on the owner's drive (3.16 M entries): crawl 38 s
  cold, snapshot load 164 ms, typical query about 1 ms.
- **Explorer core** (`apps/desktop`): tabs, navigation, breadcrumb address bar, sidebar,
  virtualised list with folder sizes, instant search, system icons, type-ahead.
- **File operations**: new folder, rename, delete, copy/cut/paste, properties, context menu,
  live folder refresh, selection that follows its files through refresh and sort.
- **Live index** (`kestrel-ntfs::journal`, `kestrel-index::{live, sync}`): the change journal
  is read without admin rights (names are stripped, so the folders it names are re-listed).
  Restart = snapshot load (~0.2 s) + catch-up (~0.2 s for ~20k journal records).
- **MFT reader and broker** (`kestrel-ntfs::{mft, volume}`, `kestrel-index::mft`,
  `apps/broker`): verified on the owner's drive. A full MFT read takes 5.7 s for 3.64 M
  records; against a crawl taken minutes apart, 99.99 % of entries match by file id and name,
  and the remaining size differences are files being written (the MFT is the more current).
  The broker is installed as the `KestrelBroker` service (LocalSystem, auto-start, binary in
  `C:\Program Files\Kestrel`); a non-admin request returns the whole drive in about 5 s, and
  the app's first launch without a saved index is ready about 2 s after its window opens.
- **Views** (`ViewMenu.tsx`, `ViewItem.tsx`, `lib/viewLayout.ts`, `lib/views.ts`, `thumbs.rs`,
  `visibility.rs`): details, list, tiles and icons from 32 to 256 px, each folder remembering
  its view and sort; shell thumbnails cached on disk and in memory; "Show hidden items" and
  "Show file extensions".
- **Exact results, undo and redo, batch rename** (`fileops.rs` progress sink, `undo.rs`,
  `recycle.rs`, `batchrename.rs`, `BatchRename.tsx`): every operation, drops included, goes
  into a journal of the newest 100, kept in `kestrel-db` (`kestrel_db::undo`) as it changes and
  restored at start without the operations whose items no longer match the disk; deletions to
  the Recycle Bin come back from it.
- **Locked results** (`access.rs`, `lib/access.ts`): rows in or being folders the account can't
  open are dimmed with a padlock in every view and explain themselves when opened.
- **Broker journal size** (`apps/broker/src/journal.rs`): `install --journal-mb N`, unit
  tested only; the elevated install that uses it has not been run.
- **Preview pane and Quick Look** (`preview.rs`, `PreviewPane.tsx`, `PreviewBody.tsx`,
  `QuickLook.tsx`, `previewState.ts`): Alt+P and Space; the `preview` protocol, and
  `preview::guard`, which refuses app commands to any webview but `main` and any frame that
  isn't the app's own. Images, media and PDF documents show in a second webview with no
  capabilities, laid over the preview area (`previewview.rs`, `lib/previewSurface.ts`,
  `PreviewSurface.tsx`).
- **Release build** (`pnpm tauri build`, bundling off): builds `kestrel-shellhost.exe` next to
  `kestrel.exe` first (`scripts/shellhost.mjs`); e2e slots run it with `KESTREL_RELEASE=1`.
- **Drag and drop** (`dragdrop.rs`, `dragdrop.ts`, `DropHint.tsx`): native drags out
  (`SHDoDragDrop` on the window's thread), drops in onto rows, icons, places, tabs or the open
  folder, with Explorer's copy and move rules.
- **Windows context menu and Open With** (`shellmenu.rs`, `apps/shellhost`): in a
  crash-isolated helper process.
- **Live index and drop fixes** (hardening): journal changes inside a folder moved in the same
  batch are applied (folders listed by file id), and a drop on a row still loading waits for it.
- **Shortcuts** (`src-tauri/src/shortcuts/`, `kestrel_db::shortcuts`, `shortcuts.ts`,
  `PinnedPlaces.tsx`, `ShortcutLists.tsx`, `GoToFolder.tsx`, `TagDots.tsx`): pinned places in
  groups with Ctrl+1 to Ctrl+9, frecent folders and Go to folder (Ctrl+G), saved searches,
  coloured tags that follow items by file id (`tag:` in queries), and workspaces of tabs.
- **Projects** (`src-tauri/src/projects/`, `kestrel-db::projects`, `ProjectsHub.tsx`,
  `ProjectDetail.tsx`, `projects.ts`, `lib/gitBadges.ts`, `projects.css`): discovery from the
  index, git facts from a read-only git pool, the hub (Ctrl+Shift+P), and git badges in every
  view of a folder inside a repository (not in search results, which would run git in every
  repository a search touches).
- **Command palette, quick launcher, tray** (`palette.rs` and `palette/`, `launcher.rs` and
  `launcher/`, `lib/palette*.ts`, `CommandPalette.tsx`, `PaletteOverlay.tsx`,
  `QuickLauncher.tsx`, `LauncherSettings.tsx`, `quick.html`): Ctrl+K, the Ctrl+Alt+Space
  launcher window, the tray icon and "keep running in the tray"; recency in kestrel-db
  (`kestrel_db::palette`), the shortcut and tray option in its settings table. Its providers
  cover the actions, the index, typed paths, `@` projects (`projects/provider.rs`), `#` tags,
  and the pins, frequent folders, saved searches and workspaces (`shortcuts/provider.rs`,
  front-end halves in `lib/paletteFeatures.ts`).
- **Places** (`lib/locations.ts`, `places.ts`, `placesSetup.ts`, `ThisPC.tsx`, `RecycleBin.tsx`,
  `FolderTree.tsx`, `AddressInput.tsx`, `ConfirmDialog.tsx`, `thispc.rs`, `recyclebin.rs`,
  `foldertree.rs`, `address.rs`, `kestrel_db::places`, `places.css`): the special locations
  registry, This PC (drives probed with a time limit each, mapped and remembered network
  drives, Eject), the Recycle Bin page (restore through `fileops` into the undo journal,
  delete permanently, empty), the sidebar's virtualised folder tree from the index with
  folding sections and a resizable edge, and address completion and resolution. Expanding all
  of C: in the tree (820,756 folders) took 0.7 s and scrolled at a 7.6 ms average frame.
- **Layout** (`layout.rs`, `kestrel_query::{group, filter}`, `kestrel_db::layout`, `panes.ts`,
  `lib/listLayout.ts`, `lib/columns.ts`, `Panes.tsx`, `ColumnHeaders.tsx`, `GroupHeader.tsx`,
  `FilterBar.tsx`, `layout.css`): a second pane (Ctrl+Shift+D) with tabs of its own, Copy and
  Move to other pane (Alt+C, Alt+X) through the undo journal, the layout restored on start;
  Explorer's Group by in every view and in search results (2.7 M results group in about 20 ms);
  configurable details columns; the filter box (Ctrl+Shift+L); the selection rectangle; and the
  Content view (Ctrl+Shift+8; Tiles is Explorer's Ctrl+Shift+7).
- **Explorer parity** (`src-tauri/src/parity/`, `kestrel_db::parity`, `parity.ts`,
  `CommandBar.tsx`, `HomeView.tsx`, `DetailsPane.tsx`, `FolderOptions.tsx`, `ParityLayer.tsx`,
  `parity.css`; checklist in `docs/explorer-parity.md`, kept current with `main`): the command
  bar, New > every `ShellNew` type and New > Shortcut, Home (Quick access, Favorites, Recent;
  a special location), the details pane (Alt+Shift+P), more windows (Ctrl+N), Alt+drop
  shortcuts, Folder Options (open to Home, This PC or the user folder; protected files,
  single-click, full path, sync status glyphs), Open in Terminal, Open file location, and
  Explorer's remaining shortcuts. Folder visits are recorded once (`frecency_visit`) and the
  palette reads them (`kestrel_db::palette::recent`).
- **Archives** (`src-tauri/src/archive/`, `kestrel_db::archives`, `archives.ts`,
  `lib/archivesApi.ts`, `ArchiveDialogs.tsx`, `archives.css`): zip, 7z, RAR, tar (plain, gzip,
  bzip2, xz) and single `.gz`/`.bz2`/`.xz` files browse as read-only folders (nested archives
  too), with the preview, Quick Look, opening a file from a temp copy, Copy and drags out;
  "Extract all…", "Extract here", "Extract…", "Compress to ZIP file" and "Compress to…" (zip,
  7z, tar.gz), with progress, cancel, undo, and the zip-slip, link, device-name, bomb, Mark of
  the Web and password rules, each unit tested (`archive/tests.rs`).
- **Disk usage** (`crates/kestrel-analyze`, `src-tauri/src/disk/`, `kestrel_db::disk`, `disk.ts`,
  `lib/diskApi.ts`, `DiskHub.tsx`, `DiskOverview.tsx`, `DiskTreemap.tsx`, `disk.css`): the hub
  (sidebar, Ctrl+Shift+U, the palette, the folder's and the command bar's menus, and the drive
  menus of This PC and the folder tree), from the index only, with four tabs: Overview, Map,
  Cleanup and Duplicates. Measured on the owner's index (3.47 M entries): treemap layout 1–2 ms,
  overview ~92 ms, daily growth sample ~7 ms (`kestrel-cli disk-bench`).
- **Cleanup recommendations and duplicates** (`crates/kestrel-cleanup`, `src-tauri/src/cleanup/`,
  `kestrel_db::cleanup`, `CleanupPanel.tsx`, `DuplicatesPanel.tsx`, `CleanupParts.tsx`,
  `cleanup.ts`, `lib/cleanupApi.ts`, `cleanup.css`): rules over the index in three tiers,
  recycled through `fileops` into the undo journal after a confirmation, tools run on a click;
  duplicates by size, then ends, then BLAKE3, hashes cached by file id. The two panels are the
  disk usage hub's Cleanup and Duplicates tabs, on the hub's drive or folder; the palette's
  "Cleanup recommendations" and "Find duplicate files" open the hub there.

Every feature branch is merged on `main`: views, undo, preview, dragdrop and shellmenu, then
hardening, shortcuts, projects and palette, then places, layout, parity, archives, disk and
cleanup (integrated 2026-10-15). Every unit test and all 22 e2e suites pass there in a dev build
(`cleanup-dryrun.mjs` is a read-only report, not a suite), and the suites that need no
development hook in a release build too (the list is in the README's Testing section);
`integration.mjs` and `connections.mjs` cover where the older features meet, and the newer
suites cover theirs (`places.mjs` opens the disk usage hub from This PC, `cleanup.mjs` drives
the hub's tabs and the Recycle Bin page).

Not done yet: an installer (bundling is off; the helper would then need a Tauri sidecar entry,
see README); what else Explorer does that Kestrel does not is listed in
`docs/explorer-parity.md`.

## How the app fits together

- `crates/kestrel-index`: the index (`index.rs`), its builder, snapshots, crawl (`walk.rs`),
  MFT build (`mft.rs`), folder reconciliation (`sync.rs`), journal following (`live.rs`) and
  the broker client (`broker.rs`).
- `crates/kestrel-query`: query parsing, parallel search and sorting over an index.
- `crates/kestrel-analyze`: disk usage from an index, no I/O: `treemap.rs` (squarified layout of
  only what is visible), `category.rs` (types by extension and location), `overview.rs` (totals,
  types, ages, largest items, recent changes, one walk split across threads), `growth.rs` (the
  daily sample, its retention and the comparison). The app's half is `src-tauri/src/disk/`
  (commands, the drive's own figures in `volume.rs`, the sampler in `history.rs`, and the
  `disk-changed` event while the hub is open); the samples live in `kestrel_db::disk`.
- `crates/kestrel-ntfs`: NTFS journal, MFT record parsing and raw volume reads.
- `apps/desktop/src-tauri/src`: the backend. `main.rs` registers every Tauri command in one
  `generate_handler!` list, wrapped in `preview::guard(...)`, and every custom protocol
  (`icon`, `thumb`, `preview`). `engine.rs` owns the index and the result sets the UI pages
  through; `indexer.rs` keeps the index current and owns the data folder (`data_dir()`,
  overridable with `KESTREL_DATA_DIR`); `fileops.rs` runs file operations through the shell's
  copy engine and reports exactly what each did; `undo.rs` keeps the journal they are recorded
  in (anything new that changes files calls `fileops::execute`, then `journal.record`);
  `clipboard.rs`, `watch.rs`, `icons.rs`, `places.rs`, `lru.rs` (the icon and thumbnail
  memory caches) and `navigation.rs` (the window stays on the app) do what their names say.
  `shellmenu.rs` runs the Windows context menu and Open With in `apps/shellhost`
  (`kestrel-shellhost.exe`), a helper process found next to `kestrel.exe`, so third-party
  shell extensions never load into the app. The main window holds two webviews: `main`, the
  app's page, and `preview` (`previewview.rs`), so commands take a `tauri::Window`, never a
  `WebviewWindow` (Tauri refuses to hand one to a command from a window with two webviews),
  the main window is looked up with `get_window`, never `get_webview_window` (which returns
  nothing once the preview's webview exists), and capabilities name webviews
  (`"webviews": ["main"]`), not windows. `preview::guard` admits the app's own two pages,
  `main` and the quick launcher's `quick` (`preview::APP_WEBVIEWS`), and nothing else.
- `apps/desktop/src`: the front end. `state.ts` holds tabs and every action; `lib/api.ts`
  wraps every backend command; `components/` has one file per piece of UI; `styles.css` holds
  the design tokens (slate surfaces, one ochre accent, light and dark) and all styles.
  `FileList.tsx` draws details rows itself and every other view's items through `ViewItem.tsx`;
  both share one set of item mouse handlers (`onItemMouseDown` and friends), so a change to
  how items respond to the mouse is made once and holds in every view.
- More than one app window (Ctrl+N; `src-tauri/src/parity/appwindows.rs`): `main`, then
  `main-2`, `main-3`... in the same process, each page with its label in its address
  (`index.html?window=main-2`). Each window has its own preview webview (`preview`,
  `preview-2`), folder watch (`Watcher::watch_for`) and drag hook; `preview::guard`,
  capabilities (`main-*`) and `navigation.rs` admit them by `is_app_window`/`is_preview_label`.
  The quick launcher, the tray and a second launch bring forward the window used last
  (`appwindows::target`); Kestrel exits when the last app window closes. A window opened with
  Ctrl+N gets its folder from `window_start` (`givenLocation` in `lib/home.ts`); only `main`
  restores and saves the pane layout (`keepsLayout` in `panes.ts`).
- `src-tauri/src/projects/`: `discover.rs` finds project roots in the index (marker files,
  excluded locations), `facts.rs` walks a project's part of the index (sizes, languages,
  activity, and a fingerprint of what git would see), `git.rs` runs git read-only with a
  timeout, `badges.rs` answers folder views, `scripts.rs` and `launch.rs` serve the detail
  panel. `mod.rs` owns the background look (debounced on `Engine::revision`, about 15 ms over
  3.6 M entries), the git pool and cache, and the commands; `project-git` and
  `projects-changed` events tell the UI. The hub's styles live in `src/projects.css`.
- Result sets: a listing or search produces a set id; the UI fetches rows by range. The UI
  releases a set only after switching to its replacement (`show` in `state.ts`).
- Layout: every tab belongs to a pane (`Tab.pane`); each pane shows one of its tabs
  (`shownTab`), the active tab is the one the pane with the keyboard shows, and while two panes
  are shown both shown panels carry `.tab-panel.current` (only one `.active`). The active
  tab's folder is watched by `list_dir`, the other pane's by `watch_other_pane`; both watches
  are per window (`Watcher::watch_for(window.label())`) and end with it. A new listing
  passes through
  `arrange` in `state.ts`, which groups it (`group_set`: the engine keeps the plain sorted order
  aside, `Arranged` in `engine.rs`, and the group starts) and narrows it by the tab's filter
  (`filter_set`, a new set made from the listing, which stays in `Tab.unfiltered`). The list
  places groups with `ListLayout` (`lib/listLayout.ts`), which the keyboard and the selection
  rectangle use too, and asks for the names of the groups in view (`group_headers`). Panes,
  split, tabs and columns are JSON documents in `kestrel_db::layout`, opened at start by
  `openLayout` (`panes.ts`).
- The command palette ranks items from *providers* in one place, `palette.rs` (nucleo-matcher,
  boosted by recency). A provider has a backend half (`palette::Provider`, registered in
  `main.rs`) when its items come from backend data, and always a front end half
  (`registerPaletteProvider` in `lib/palette.ts`, registered in `PaletteOverlay.tsx` and
  `QuickLauncher.tsx`) that says what choosing an item does. Both files' module documentation
  says how to add one; prefixes `>` `/` `@` `#` narrow the list to one provider. The same file
  or folder from several providers is listed once (`one_per_path`), and a provider whose
  items can be renamed or deleted implements `Provider::current`, which the empty palette's
  remembered items go through.
- `crates/kestrel-db`: the app's own data (pins, projects, tags, history, settings) in one
  SQLite file, `data_dir()/kestrel.db`, managed in Tauri as `State<Arc<Db>>`. A feature owns
  its tables: it adds a module with its `Migration`s and lists them in `MIGRATIONS`. Migrations
  run once in name order, so name them `<yyyy_mm_dd>_<feature>_<what>` and never edit one that
  has shipped.
- **Navigation: one mechanism.** Everything a tab can show goes through `navigate(tabId,
  address)` in `state.ts`, from every entry point (address bar, breadcrumb, sidebar, folder
  tree, palette, history, new tabs, new windows, the saved layout, workspaces, drops):
  - `resolveLocation(address)` (`lib/locations.ts`) first: a *special location* (Home, This PC,
    the Recycle Bin; registered with `registerLocation` in `placesSetup.ts`) under any of its
    spellings. `showLocation` keeps its canonical address in `Tab.path` and the history, with
    `listing` null, and the tab's panel in App.tsx draws the location's `page` (given
    `LocationPageProps`: tab id, address, whether active, a revision bumped by Refresh, and
    `focusRef`) over the hidden file list, in whichever pane the tab is.
  - anything else goes to `list_dir`: a folder, a share, or an *archive address*
    (`C:\x\a.zip\dir`), which `archive::list` answers before the engine with a read-only
    listing (`Listing.archive` set).
  Everything else follows `Tab.path`: titles (`tabTitle` in TitleBar.tsx, used by both panes'
  strips, and `titleOf`), the breadcrumb (`crumbsOf`), Up (`parentOf`; a drive's parent is This
  PC), the sidebar's and tree's highlight, searches (which go everywhere from a location), the
  saved layout and workspaces, and the palette (`lib/paletteLocations.ts` lists the locations;
  the files provider opens archives as folders and the `/` paths provider lists inside them).
  Code that takes a tab's path as a folder checks `locationOf(tab.path)` and
  `inArchive(tab.listing)` first, or uses the helpers that do: `inFolder` (parity.ts),
  `otherPaneFolder` (panes.ts), `analysableFolder` (disk.ts). The status bar shows a page's own
  `setLocationStatus` parts. Hubs (projects, disk usage) are overlays over the content, not
  locations; they close when the active tab moves. A family of addresses can be a location with
  `claims(address)` and its own `title`, `crumbs` and `parent`; `adopts(path)` makes a location
  the parent of file system roots (This PC adopts drive roots).
- Places (`places.ts`, backend `thispc.rs`, `recyclebin.rs`, `foldertree.rs`, `address.rs`):
  This PC's drives are probed on a thread each with a time limit (`thispc::Drives`) and a
  watcher emits `drives-changed`; late answers come as `drive-details`. The bin is read through
  the shell's Recycle Bin folder, and every command takes item identities (the item's `$R`
  path); restores run as `Op::Restore` through `fileops` and are recorded in the undo journal;
  `recycle::find` prefers an exact deletion time, so a restore of one listed item is exact.
  The tree's folders come from the index (`tree_children`, `tree_reveal`, `tree_subtree`), and
  `folders-changed` (the engine's revision) makes it ask again for the folders in view; it
  follows "Show hidden items" and "Hide protected operating system files", as the address bar's
  suggestions do. The disk usage hub opens on a drive through `setDiskUsageHandler` in
  places.ts, which App.tsx sets at startup. The sidebar's collapsed sections and "Expand to
  open folder" are in `kestrel_db::places`.
- Archives (`src-tauri/src/archive/`, see its module documentation): `address.rs` (formats by
  name, and addresses inside archives), `reader.rs` (one `Source` per format; RAR through
  `System32\tar.exe`, converted to a tar stream, so the same checks apply), `names.rs` (paths
  made safe or refused), `extract.rs` (plan, free-space check, a hidden staging folder beside
  or inside the destination, then a rename into place, or a merge whose clashes go to the
  shell's copy engine), `compress.rs`, `browse.rs` (archive trees cached by path, size and
  date; items copied out to `data_dir()\archive-cache` for the preview, drags, Copy and nested
  archives, and to `%TEMP%\Kestrel` for opening), `motw.rs`, `job.rs` (progress events
  `archive-progress`, cancel, failures as `{ kind, message }`). Jobs record what they created
  through `Journal::record_as`, so undo recycles it through the usual marks. Front end:
  `archives.ts` (menus, jobs panel, dialogs' state), `ArchiveDialogs.tsx`.
- Navigation into archives: an **archive address** is the archive's own path followed by the
  path inside it (`C:\x\photos.zip\2024\beach.jpg`). Nothing on disk has that path:
  `archive::address::split` walks up to the nearest item that exists, and if it is an archive
  file the rest is the inner path. `list_dir` hands such paths to `archive::list` before the
  engine (one `metadata` call for an ordinary folder), which lists them through
  `Engine::show_items` as live rows, with `Listing.archive` set to the archive's path; the front
  end treats such listings as read-only (`inArchive` in `archives.ts`). Rows inside carry
  addresses as their paths, so every command that takes paths checks them:
  `archive::refuse_entries`/`refuse_location` in delete, rename, batch rename, new folder,
  paste, cut, drops (`dragdrop::plan`) and the Windows menu; `archive::materialize` in Copy and
  drags out; `archive::open_entry` in `open_path`; `archive::preview_file`/`preview_details` in
  the preview's commands and protocol; `archive::icon_key` for icons; `archive::entries` for
  the palette's typed paths. Anything that lists things keeps archives working by calling
  `list_dir` with the address, and any new path-taking command goes past those checks (the
  Windows menu for paths, `shell_paths_menu`, does).
- Cleanup: `crates/kestrel-cleanup` holds the rules (`rules/`, one module each, run in order and
  claiming what they offer), `Locations` (every place they look; `Locations::under` lays a
  machine out in a scratch folder for tests), the `Guard` (what is never offered: system and
  owner's folders, also where OneDrive moved them from, projects' sources, links), `inuse`, and
  `dupes` with `hash`. `src-tauri/src/cleanup/` keeps the latest scan and checks every action
  against it; `cleanup_dev_sandbox` (dev builds) points the rules at a scratch folder and records
  tools instead of running them. Run a scan's rules one per index lock (`Scan`), never the whole
  scan under one. The panels are tabs of the disk usage hub (`TABS` in `DiskHub.tsx`), which
  gives them its root; `cleanup.ts` only holds the palette's ways in.
- Shortcuts: data in `kestrel_db::shortcuts` (pins and groups, frecency, saved searches, tags,
  workspaces), commands in `src-tauri/src/shortcuts/`, front end in `src/shortcuts.ts`,
  `lib/shortcutsApi.ts`, `shortcuts.css` and the `Pinned*`, `Shortcut*`, `GoToFolder`,
  `TagDots` components. Tagged items and pins are known by volume and file id, found again
  through the index; `tag:` clauses are resolved to entries by `Engine::search_resolving`.

## Working on Kestrel (agents read this)

Code:
- Match the surrounding style: Rust 2024, small functions, doc comments that explain why,
  `// SAFETY:` on every `unsafe` block, no `unwrap()` outside tests and proven invariants.
  `windows-sys` for plain Win32 calls, the `windows` crate for COM.
- TypeScript is strict; in Solid components never destructure props. UI copy is sentence
  case, plain verbs, no all-caps labels, no decorative arrows; reuse the tokens in
  `styles.css` rather than new colours.
- New backend features go in a new module with its commands; add them to the
  `generate_handler!` list in `main.rs`. New UI goes in a new component file. Keep edits to
  shared files (`main.rs`, `engine.rs`, `state.ts`, `App.tsx`, `api.ts`, `styles.css`)
  small and additive, because several branches are merged together.
- Write Rust edits with the editor tools: backslashes in Rust char literals are lost in
  shell heredocs.

Testing (every change ships with tests, and you run them):
- `cargo test --workspace` and, in `apps/desktop`, `pnpm install` once then `pnpm typecheck`.
- End-to-end: pick your slot number (given in your task; never 0 unless told) and run
  `KESTREL_SLOT=<n> node e2e/dev.mjs start`, then `KESTREL_SLOT=<n> node e2e/<suite>.mjs`,
  then `KESTREL_SLOT=<n> node e2e/dev.mjs stop`. Each slot has its own ports, app identity,
  build directory (`target/slots/slot<n>`) and data folder, so slots run side by side. The
  first start of a slot builds the app and takes a few minutes.
- New features get a new suite `e2e/<feature>.mjs` modelled on `fileops.mjs`: use
  `connectToKestrel()` from `e2e/connect.mjs` (it refuses any window not titled Kestrel),
  confine file changes to a fresh folder under `%TEMP%`, and save screenshots to `e2e/out`.
  Look at your screenshots; a passing test with a broken-looking screen is not done.
- `fileops.mjs` uses the system clipboard; do not run it while other agents may be running
  tests. Whoever integrates branches runs every suite, one after another.
- Things a script cannot do (a real mouse drag between apps, a native dialog) are listed as
  manual checks in your report rather than claimed as tested.

Safety rules:
- File changes in tests happen only in scratch folders under `%TEMP%`. Never touch the
  owner's real files, and never empty the Recycle Bin.
- No elevation: never start a UAC prompt, install services or change system settings.
- No large downloads (models, GPU runtimes, ffmpeg). Features that need one download only
  when the user clicks a button that states what, from where and how big; tests never click
  it. Ordinary `cargo` and `pnpm` dependencies are fine.
- Nothing is deleted automatically; deletions go to the Recycle Bin unless the user chose
  permanent deletion in a confirmation.

Git:
- When working in a git worktree, commit your work on that worktree's branch, ending each
  message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Never push, never
  rewrite history, never commit build output (`target/`, `node_modules/`, `e2e/out/`).

## Gotchas

- The quick launcher is a second window (`quick`, `quick.html`, titled "Kestrel quick
  launcher") that exists, hidden, from startup, so closing the main window would not end the
  process: `launcher::on_window_event` exits when the main window is destroyed. Test instances
  get `KESTREL_GLOBAL_HOTKEY=off` from `e2e/dev.mjs`, so they never register the system-wide
  shortcut; the launcher's development commands (`launcher_dev_*`) summon it, register a
  shortcut and stand in for tray clicks.
- After a Rust change, `tauri dev` once failed to link (`LNK2019` on a serde_json symbol); an
  incremental build gone stale. `cargo clean -p kestrel-desktop` with the slot's
  `CARGO_TARGET_DIR` fixed it.
- The preview's webview is drawn above the page, so anything of the app's that can lie over
  the preview area must be in `OVERLAYS` (`lib/previewSurface.ts`), or it ends up underneath
  the image or video: menus, dialogs, notices, Quick Look, the palette, Go to folder, the
  projects and disk usage hubs, the address bar's suggestions and the archive jobs panel are.
- Every key typed into the palette stops there (`CommandPalette.tsx`), so the window's own
  shortcuts never act behind it; window-wide keys that must work over it listen in the capture
  phase, as Ctrl+K does.
- A navigation that fails leaves the tab in its folder with the message in place of the rows
  (`Tab.failed` in `state.ts`), and Back shows that folder again rather than stepping through
  the history; tests that check a failed address should wait for the message to go.
- Result sets live in the backend and the UI holds handles to them. A set is released only
  after the tab has switched to its replacement (`show` in `src/state.ts`); releasing earlier
  makes in-flight row requests look like a stale index.
- The folder watcher is armed before the folder is listed (`list_dir` in `main.rs`), so no
  change can fall between the two. Keep that order.
- Avoid a CSS class named after a generic word on more than one component; `.search` on both
  the toolbar box and the results list once broke the layout.
- Vite's dev server can serve a stale transform when a file is written twice in quick
  succession. If the running window disagrees with the source, `touch` the file.
- The unprivileged journal read (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`) needs a handle to a
  folder on the volume (`C:\`), not to the volume itself, and a start position that is a real
  record boundary (a previous `next_usn`, or `first_usn`); anything else is error 87.
- Directories cannot be hard-linked, so a listed folder whose file id exists elsewhere is a
  move and keeps its subtree (`sync.rs`). Files with a known id are only moves if the original
  went missing in the same batch, because they may be new hard links.
- The broker's binary must live somewhere only administrators can write (it is copied to
  Program Files on install): a SYSTEM service run from a user-writable folder is a privilege
  escalation. Reinstalling (`kestrel-broker install`, elevated) replaces the copy.
- Processes started from Claude's shell run inside the Claude app's MSIX container: writes to
  `%LOCALAPPDATA%` land in `AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local`,
  and a crawl from inside sees that merged view. `%TEMP%` is not redirected (checked: a folder
  made there from inside is not in `LocalCache\Local\Temp`, and the journal names it by the
  same file ids a listing reports), so scratch folders there are on the real disk. An index
  or snapshot made from inside is not the one the owner's own launches use. The broker (a
  service, outside the container) sees the real disk, which makes it the way to check where
  files really are.
- Another of the owner's WebView2 apps (AI Binary Workbench) holds debugging port 9222, so
  the e2e harness uses 9333 and up, and `e2e/connect.mjs` refuses any page not titled
  Kestrel. Never point automation at a page without that check.
- Tauri compiles each instance's settings (identifier, dev URL) into its binary, which is
  why every test slot has its own build directory.
- A folder listing reports the size NTFS last recorded in the directory entry, which lags
  for files held open for writing; the MFT's own size is current.
- WebView2 copies Tauri's command bridge, and its key, into every frame, and Tauri treats
  every custom protocol host as local. Pages served by a custom protocol must never run
  script, and must never show in the app's page as documents: frames there get what the
  `main` webview's capabilities grant. Content goes in the `preview` webview, which has none
  and which `preview::guard` refuses.
- The live index lists the folders the change journal names by file id
  (`walk::Volume::list`), not by the paths the index holds: those are stale while a folder
  above has moved in the same batch, and a listing that fails silently loses the change. A
  folder that turns up as new is listed too (`Sync::take_new_dirs`): it may have arrived with
  contents nothing else names.
- A drop target read from the DOM must ignore rows still loading: a refreshed list fetches its
  rows again, and for a few milliseconds they carry no `data-path` (`dragdrop.ts`, `LOADING`).
- WebView2 leaves `document.visibilityState` alone when a webview is hidden; e2e checks of the
  preview webview's visibility ask Windows (`e2e/windows.mjs`).
- `dev.mjs stop` kills the app, so a setting saved in local storage just before may be lost;
  suites that restart the app (`undo-restart.mjs`) close the preview pane again themselves.
- The window refuses every top-level navigation away from the app's own origin
  (`navigation.rs`): a borderless window taken over by a web page has no way back. An e2e
  script that needs another page must use a frame, and Markdown links reach the browser only
  through `open_link`. The Bash tool also loses backslashes in heredocs, not only in Rust:
  write Windows paths in scripts with the editor tools.
- `styles.css` has no build-time check: a lost brace once hid every style after it (the
  browser reads the rest as one broken rule). After editing it, check the page's rule count
  (`document.styleSheets[0].cssRules.length`) or look at the screenshots.
- The e2e suites assume the details view and a closed preview pane. `views.mjs`,
  `preview.mjs` and `integration.mjs` change them and put them back in their `finally`
  blocks; if one of them is killed, reset the slot's `kestrel.views` and `preview-open` local
  storage entries.
- `pnpm dev` (and so `tauri dev` and every e2e slot) builds `kestrel-shellhost` before the
  app, but the dev watcher does not rebuild it when its code changes: run
  `cargo build -p kestrel-shellhost` (with the slot's `CARGO_TARGET_DIR`) yourself.
- Solid delegates `onKeyDown` and friends to a listener on the document, so stopping a key
  event on an ancestor element kills the handlers inside it. The projects hub keeps its keys
  from the window's shortcuts with a document listener added after Solid's (`guardKeys`).
- `git check-ignore` refuses a whole batch over one path inside a submodule; `git::ignored`
  drops the paths git names and asks again. `git status` without `--no-optional-locks` writes
  the index file; every command in `projects/git.rs` passes it.
- The sidebar's first `.place` is the user folder, named after the account, which
  `dragdrop.mjs` drops on; Home (`.home-entry`), Projects (`.projects-entry`) and Disk usage
  (`.disk-entry`) are drawn like places but are not, and take no drops.
- Shell commands outlive `InvokeCommand`: Properties opens its dialog on another thread
  without taking the process reference, and Copy leaves an OLE data object on the clipboard.
  The helper therefore waits for its windows to close and flushes the clipboard before it
  exits (`apps/shellhost/src/process.rs`).
- Junctions and directory symbolic links are indexed as entries but never read through
  (`DirItem::alias`, `walk::is_alias`). Reading through one would make the folders it leads to
  look like moves and re-parent them under the link. Any new crawl or listing must keep that.
- Any git command that reads the working tree's files (status, check-ignore, diff) goes through
  `git::run_on_worktree`, which switches off the filter drivers the repository's own config
  defines (every command already has the fsmonitor hook off): a repository must never run a
  program just because Kestrel looked at it.
- One process per data folder (`instance.rs`): a second launch of a slot's `kestrel.exe` asks
  the running one to show its window and exits. Stop a slot before starting it again with a
  new build, or the old binary keeps running.
- The sidebar has two scroll regions: the sections (`.sidebar-shelves`) and the folder tree
  (`.tree-scroller`); `.sidebar` itself does not scroll. Sections fold from their `h2` when
  they have an `aria-label` (`lib/sidebarSections.ts`), remembered in the slot's database: a
  suite that folds one unfolds it again in its `finally`.
- The folder tree draws only the rows in view: a row scrolled away is not in the DOM (the dev
  build's `window.__kestrelTree.rows()` lists them all). Deep in the tree a row's middle can
  fall on its chevron, so tests click a row's `.tree-label`.
- The Recycle Bin page acts on the owner's real bin. Tests select only rows whose original
  location is their own scratch folder, compare the bin's other items before and after, and
  never click Restore all or Empty Recycle Bin; emptying has no test hook.
- Explorer's view shortcuts, read from shell32.dll's accelerator table and View menu, are
  Ctrl+Shift+7 for Tiles and Ctrl+Shift+8 for Content (`lib/views.ts`); suites that switch to
  tiles press 7.
- The layout (panes, split, tabs) is restored when the app starts, not when its page reloads:
  a reload, as the suites do, opens one tab in the home folder as it always did (`starting` in
  `panes.ts` tells the two apart by a session storage entry; the dev hook `__kestrelLayout`
  can forget it). Columns are read from the database on every load.
- With two panes, `.tab-panel.active` is still unique; selectors for "the list on screen" that
  must cover both panes use `.tab-panel.current` (`dragdrop.ts`).
- Never take `main` to be the app window: there can be several (`parity::appwindows`). Look
  one up with `appwindows::target` (the one used last) or `app_windows`; a command acts on the
  `Window` it was called from. Tauri's JS `listen(event)` without a target receives events
  emitted to *any* window, so an event meant for one window is sent with `emitTo(label, ...)`
  and listened for with `{ target: getCurrentWindow().label }` (`paletteApi.onRun`). The e2e
  harness's `connectToKestrel()` returns `main`; `waitForKestrelWindow` finds the others.
- The live index releases its lock between reading folders and placing the new files it found
  (`live.rs`), so a folder opened meanwhile may add the same files first: `Sync::finish`
  updates an entry of the same name instead of adding a second (files made in quick succession
  were once listed twice).
- `FILE_ATTRIBUTE_RECALL_ON_OPEN` (0x40000) is also `FILE_ATTRIBUTE_EA`, which thousands of
  ordinary `System32` files carry (0x40020): it is no sign of a cloud file. OneDrive's online-only
  files carry `RECALL_ON_DATA_ACCESS` and `OFFLINE` (0x401620), in listings and in the file table
  alike (`kestrel_analyze::is_cloud_only`). Never open one: that downloads it.
- "Nothing allocated" is no sign of a cloud file either. In an index read from the drive's file
  table (the broker), a hard link's allocation is charged to one name only, files small enough
  to live in the table have none, and files Windows compresses into a stream of their own (WOF:
  Edge, Chrome and Brave under Program Files, 0x620) show none, since the index records the main
  stream's. A crawled index (or a folder re-listed since) charges every name of a hard link.
- On this machine Documents, Pictures and Desktop are backed up by OneDrive: their known folders
  point into `OneDrive`, and the profile's own `Documents` still holds the owner's files. Anything
  that protects "the owner's folders" must cover both (`Locations::protected`).
- The index hears of a deletion or a restore a moment later (the journal is read every 300 ms):
  a scan straight after recycling drops what is no longer on disk (`kestrel_cleanup::existing`),
  and tests that look for a restored file search again until it is there.
- Undo checks before acting that each item is still what the operation left: `undo_entries`
  stores a mark (file id, and for deletions a content fingerprint) per step. A file replaced by
  another program in the meantime is left alone rather than recycled.
- sevenz-rust2 0.20 (the newest that builds with the workspace's Rust 1.92) writes the 7z
  anti-item flags inverted, which made every folder a deletion marker; `compress::write_7z`
  sets `is_anti_item` on folders to cancel it, and a unit test checks with Windows' tar.exe.
  Its `Coder` properties are private, so a 7z (or xz) that declares a huge dictionary can't be
  refused before the decoder allocates it.
- RAR goes through `System32\tar.exe -P --format pax -cf - @-` with the archive on standard
  input (so no path is ever parsed by it, and `-P` keeps names as stored, to be judged by
  `names.rs`). libarchive reports an encrypted RAR only as "(null)", so `reader::rar_encrypted`
  reads the RAR 4/5 headers first. tar.exe prints names in the console code page, so tests that
  read its listing use ASCII names.
- Archive tests use a 64 MB expansion floor (`extract::MIN_BUDGET`) instead of 1 GB, and a
  cache and temp folder per test thread (`browse::test_scratch`), never the data folder.
- A dialog that stays open must not hold a result set's rows: any refresh of the folder meanwhile
  releases the set (one did while the archive suite took its screenshots), and the command then
  fails with "stale". The archive dialogs keep paths.
