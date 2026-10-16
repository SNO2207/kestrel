# Kestrel

A fast file explorer for Windows 11. One in-memory index of every file on the drive powers
instant search, instant folder listings with folder sizes, a command palette that reaches
every folder and file, and the projects on the drive with their git status; disk analysis is
to come.

*Kestrel is a working name.*

## Layout

| Path | What it is |
|---|---|
| `crates/kestrel-index` | The index: struct-of-arrays store, crawler, MFT builder, journal follower, snapshots |
| `crates/kestrel-ntfs` | NTFS access: change journal reader, MFT record parser, raw volume reader |
| `crates/kestrel-query` | Query language, parallel search, sorting |
| `crates/kestrel-cli` | Command-line front end and benchmark harness |
| `apps/desktop` | The app: Tauri 2 backend in `src-tauri`, SolidJS front end in `src` |
| `apps/broker` | Optional Windows service that reads drives' file tables for the app |
| `apps/shellhost` | Helper process that shows the Windows context menu and Open With dialog |

## Running

```powershell
cd apps\desktop
pnpm install
pnpm tauri dev
```

A release build, without an installer (bundling is off):

```powershell
cd apps\desktop
pnpm tauri build       # target\release\kestrel.exe, with kestrel-shellhost.exe beside it
```

The build also builds `kestrel-shellhost.exe` (`scripts/shellhost.mjs`, run before the front
end is built) into the same folder as `kestrel.exe`, which is where Kestrel looks for it. The
front end is built into the executable, so the folder's two programs are all it needs.

The first launch builds an index of every fixed drive and saves it to
`%LOCALAPPDATA%\Kestrel\index.kidx`: by crawling (25–40 seconds for 3 million files), or, with
the broker installed, by reading each drive's file table.

From then on the index is kept current from each drive's NTFS change journal, which needs no
administrator rights: the app reads it every 300 ms and re-reads only the folders it names. A
restart loads the saved index and replays the changes made while the app was closed, which
takes well under a second. Only if more changed than the journal holds (Windows keeps 32 MB
of history by default) is a drive scanned again, in the background.

## The broker (optional)

Reading a drive's master file table directly is much faster than crawling it, but needs
administrator rights. `kestrel-broker` is a small Windows service that does that one job for
the app. From an administrator terminal:

```powershell
cargo build --release -p kestrel-broker
.\target\release\kestrel-broker.exe install      # copies itself to Program Files and starts
.\target\release\kestrel-broker.exe uninstall    # stops and removes it
```

It serves a named pipe that only the installing account (plus SYSTEM and Administrators) can
open, accepts one kind of request (build an index of these drives), and never writes to disk.
Note that it can see every file name on a drive, including in folders your account cannot
open, and passes them to the app. The app shows such results dimmed, with a lock, and says
"Your account can't open this folder" rather than failing when you open one.

### A longer change journal

The app catches up after a restart from each drive's change journal, which Windows keeps at
32 MB by default; on a busy drive that can be used up within days, after which the drive is
scanned again. The install can enlarge every fixed NTFS drive's journal at the same time:

```powershell
.\target\release\kestrel-broker.exe install --journal-mb 512
```

This sends `FSCTL_CREATE_USN_JOURNAL` to each drive with a maximum size of N MB and an
allocation delta of an eighth of that (how much NTFS adds, and later trims from the oldest
records, at a time). N may be 32 to 16384. A journal that is already at least that large is
left as it is, so the option never shrinks one, and a drive that can't be changed is reported
and skipped. It is the one change the install makes to drives; the service itself only reads.

## Search syntax

Clauses are separated by spaces and must all match. Prefix any clause with `!` to negate it.

| Clause | Meaning |
|---|---|
| `report` | name contains `report` |
| `"two words"` | quoted text is literal |
| `src\main` | full path contains `src\main` |
| `*.rs`, `img_????.jpg` | wildcards over the whole name |
| `ext:rs;toml` | extension is one of these |
| `type:image` | `image`, `video`, `audio`, `doc`, `archive`, `code`, `exe`, `folder`, `file` |
| `size:>1gb`, `size:10mb..2gb`, `size:huge` | size |
| `dm:today`, `dm:7d`, `dm:>2026-01`, `dm:2025` | date modified |
| `in:C:\Projects` | only beneath this folder |
| `regex:^a.*\d$` | regular expression on the name |
| `tag:urgent`, `tag:"two words"` | carries this tag |

## Keyboard

| Keys | Action |
|---|---|
| Ctrl+L, Alt+D, F4 | Edit the address |
| Ctrl+F, Ctrl+E, F3 | Search |
| Alt+Left / Alt+Right / Alt+Up, Backspace | Back / forward / up |
| Ctrl+T / Ctrl+W / Ctrl+Tab | New tab / close tab / next tab |
| Ctrl+N | New window, in the same folder |
| F11 | Full screen |
| F6, Shift+F6 | Move between the address bar, the search box, the sidebar and the files |
| F5, Ctrl+R | Refresh (folders also refresh themselves when their contents change) |
| Type letters | Jump to a name |
| F2 | Rename; with several items selected, batch rename |
| Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z) | Undo / redo the last file operation |
| Del / Shift+Del | Move to the Recycle Bin / delete permanently |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste, shared with Explorer's clipboard |
| Ctrl+Shift+N | New folder |
| Ctrl+Shift+C | Copy as path |
| Alt+Enter | Properties; with nothing selected, the folder's |
| Right-click, Menu key | Context menu |
| Shift+right-click, Shift+F10 | The Windows context menu, with every shell extension's commands |
| Ctrl+Shift+1 / 2 / 3 / 4 | Extra large / large / medium / small icons |
| Ctrl+Shift+5 / 6 / 8 | List / details / tiles |
| Ctrl+wheel | Larger or smaller icons |
| Alt+P | Show or hide the preview pane (drag its left edge to resize it) |
| Alt+Shift+P | Show or hide the details pane, in the preview pane's place |
| Ctrl+Shift+P | Show or hide the projects hub |
| Space | Quick Look; Space or Esc closes it, the arrow keys move to the previous or next item |
| Ctrl+D | Pin the selection to the sidebar, or the folder when nothing is selected |
| Ctrl+1 … Ctrl+9 | Open the first nine pins, or the pins given those numbers |
| Ctrl+G | Go to folder: the folders you visit most, then every folder in the index |
| Ctrl+K | Command palette: every action, folders and files, pins, saved searches, workspaces, `@` projects, `#` tags |
| Ctrl+Alt+Space | The quick launcher, from anywhere in Windows (changeable) |
| Mouse back / forward buttons | Back / forward |
| Middle-click a folder | Open it in a new tab |
| Ctrl+Shift+E | Show the folder in the folder tree (passed to the tree as `kestrel:reveal-in-tree`) |

Every view (details, list, tiles and icons) has the same mouse and keyboard handling: click,
Ctrl+click and Shift+click to select, Ctrl+A, arrow keys, typing to jump to a name, rename in
place, both context menus, dragging out and dropping in.

## File operations, undo and batch rename

File operations run through the Windows shell's own copy engine, so progress and conflict
dialogs, the Recycle Bin and Explorer's undo behave exactly as they do in Explorer. A progress
sink records what each operation actually did (the name a copy got, where a moved item went,
what reached the Recycle Bin), which is what selects new items and what undo works from:

- a rename is undone by renaming back, a move by moving back;
- a copy or a new folder by sending what was created to the Recycle Bin, never erasing it;
- a deletion to the Recycle Bin by restoring exactly those items, found in the shell's
  Recycle Bin folder by original path and deletion time;
- a permanent deletion can't be undone, and the app says so.

Each undo or redo is one shell operation with the usual progress and conflict windows. Undo
and redo are also on the folder background's context menu. New folders, renames, deletions,
pastes, batch renames and drops all go into the history (the newest 100 operations); commands
chosen from the Windows context menu are carried out by Windows and do not. The history is
kept in the app's database as it changes, so it survives a restart, or a crash; when the app
starts again, operations none of whose items are still where they were left (renamed or
deleted since, or gone from the Recycle Bin) are dropped without a word. Items are known by
their file ids as well as their paths, and an undo that would send something to the Recycle
Bin only does so while it holds what the operation left: a new folder others have saved files
in since, or a copy edited since, is left alone and the undo says why.

Batch rename (F2 with several items selected) offers find and replace (plain or regular
expression, optionally case-sensitive), a prefix and suffix, numbering (`Photo ###` from a
start value; `*` stands for the current name) and case changes, applied to the name, the
extension or both. A live preview flags conflicts and invalid names, and Apply renames
everything as one undoable operation.

## Views

The View button in the toolbar picks the view (details, list, tiles, or icons from 32 to 256
pixels), the sort order, and two options: "Show hidden items" (off by default; hidden files,
protected system files among them, are then left out of folders and search results, while
files only marked system, such as `C:\Windows\Fonts`, show as they do in Explorer) and "Show
file extensions". As in Explorer, protected operating system files (hidden and system both)
stay out even while hidden items are shown, unless Folder Options say otherwise (see Explorer
parity below).
Each folder remembers its own view and sort, and search results have one of their own.

Icon and tile views show thumbnails rendered by the Windows shell, cached in
`%LOCALAPPDATA%\Kestrel\thumbs` (up to about 1 GB; the oldest go first). Files whose contents
are only in the cloud, such as OneDrive files not kept on this device, show their icon instead,
so browsing never downloads them; once such a file is downloaded, its thumbnail appears the next
time it comes into view.

## Preview pane and Quick Look

Alt+P (or the toolbar button at the right) shows the preview pane beside the list; drag its
left edge to resize it. It shows the focused item, or the folder itself when nothing is
focused: images (SVG only through `<img>`), video, audio, PDF (the web view's own viewer), text
and code with highlighting (the first megabyte), Markdown rendered and sanitised, JSON
re-indented, and details for folders and everything else. Online-only cloud files are read
only when you click to download them, and images in a Markdown file that are online only are
never downloaded; a Markdown file's images come only from its own drive or share. Space opens
the same preview large, as Quick Look; the arrow keys step through the items without closing
it.

File bytes reach the preview through the `preview` protocol (`src-tauri/src/preview.rs`),
which serves regular files only, honours `Range`, and shows HTML and other active content as
text. Images, video, sound and PDF documents are shown in a webview of their own, laid over the
preview area (`src-tauri/src/previewview.rs`), which has no capabilities: nothing shown there
can call any of the app's commands or Tauri's own (window, event, app, webview). It steps
aside while one of Kestrel's menus, dialogs or notices, the command palette, Go to folder or
the projects hub lies over it. The window itself never leaves the app's page
(`src-tauri/src/navigation.rs`): web links open in the default browser.

## Locked folders

Search results in folders your account can't open, and such folders themselves, show dimmed
with a padlock in every view. Opening one, typing its address or opening a file inside says
"Your account can't open this folder" instead of failing. Only the rows on screen are checked,
and answers are remembered for two minutes; F5 or Refresh checks again.

## Drag and drop

Drag selected items out of the list to Explorer, the desktop, an email or any other program: it
is a native Windows drag with the shell's drag image, and the target decides between copy,
move and shortcut as it would for Explorer. Files dragged onto Kestrel, from anywhere or from
Kestrel itself, go into the folder row, sidebar place or tab under the cursor, or into the open
folder. As in Explorer, they move within a drive and are copied to another; hold Ctrl to copy
or Shift to move, and Alt (or Ctrl and Shift together) to make shortcuts to them there
("report.docx - Shortcut"). A hint at the bottom of the list says which will happen. Hovering a folder,
place or tab for a second opens it, and the list scrolls when the cursor nears its top or
bottom edge (its left or right edge in the list view, whose columns scroll sideways). Dropped
items are selected where they land, and Ctrl+Z puts them back.

## Pins, frequent folders, saved searches, tags and workspaces

The sidebar starts with pinned places: any folder or file, pinned from its menu, with Ctrl+D,
or by dropping it on the pinned places (dropping on a pinned folder moves into it, as on any
place). Pins can go in named, coloured groups that collapse, be dragged into a new order (groups
too), and be given a name of their own and a number key. A pin whose target was renamed or
moved shows as missing; clicking it finds it again by its file id through the index.

Every folder you open counts as a visit except in scratch folders such as `%TEMP%`. A visit is
recorded once, and read by the Frequent list, Go to folder, Home's Quick access and the command
palette alike. Folders are
ranked by frecency, as zoxide ranks them, for the Frequent list and Go to folder (Ctrl+G), which
also searches every folder in the index: type part of a name (`kstrl` finds `Kestrel`), with
earlier words narrowing by path (`proj kes`).

The bookmark button in the search box saves the search, and where it searches, to the sidebar.
"Tags…" in an item's menu puts coloured tags on it; they show as dots in every view, follow the
item through renames and moves (by file id), and `tag:name` finds everything carrying one.
The + beside Workspaces saves the open tabs, with each folder's view and sort, to restore
later. All of it is kept in `kestrel.db` in the app's data folder, and all of it is in the
command palette too (see below): pins with their number keys, frequent folders, saved
searches and workspaces in every query, and the tags under `#`.

## The Windows context menu

"Show more options" at the end of Kestrel's context menu (or Shift+F10, or Shift+right-click)
shows the menu Explorer would, with the commands shell extensions add (archivers, version
control, cloud storage), and "Open with…" shows Windows' own app picker. Shell extensions
load into whichever process shows the menu, and a faulty one can crash it, so Kestrel never
loads them: `kestrel-shellhost.exe` does, once per menu, and Kestrel reports a crash as a
notice and carries on. The helper is built next to `kestrel.exe` (`pnpm dev` builds it before
the app). It has no console window, so pipe its output (`| Out-String`) when running it on
its own:

```powershell
kestrel-shellhost verbs -- C:\Notes\todo.txt          # the menu's items as JSON
kestrel-shellhost invoke --verb copy -- C:\Notes\todo.txt
kestrel-shellhost menu --x 400 --y 300 -- C:\Notes\todo.txt
kestrel-shellhost openwith C:\Notes\todo.txt
```

`pnpm tauri build` builds the helper next to `kestrel.exe` (see Running). An installer, when
bundling is turned on, has to carry it too: copy the built helper to
`apps/desktop/src-tauri/binaries/kestrel-shellhost-x86_64-pc-windows-msvc.exe`, add
`"externalBin": ["binaries/kestrel-shellhost"]` under `bundle` in `tauri.conf.json`, and sign
it with the app; Tauri puts sidecars next to the main executable, where the app looks. (With
`externalBin` set, every build of the app needs that file first, which is why it isn't yet.)

## Projects

"Projects" at the top of the sidebar (or Ctrl+Shift+P) opens a hub of every project on the
drive, found in the index: folders with `.git`, `Cargo.toml`, `package.json`,
`pyproject.toml`, `go.mod`, `CMakeLists.txt`, a `.sln` or `.csproj`, a `.uproject`,
`build.gradle`, `pom.xml`, `composer.json` or a `Gemfile`. A project's root is the outermost
folder with `.git` (a submodule or nested clone is part of its repository), otherwise the
topmost folder with a marker. Dependency, cache and system folders (`node_modules`, `target`,
`vendor`, `.venv`, `.cargo`, `AppData`, `Program Files`, `Windows` and the like), drive roots
and profile folders are never looked in. New projects appear while the hub is open; "Not a
project" hides a folder for good (it can be restored from "Show dismissed folders").

Each project shows its stack (from its build files and the share of source code by language),
branch, upstream, commits ahead and behind, staged, modified and untracked changes, last
commit, a link to its page on GitHub, GitLab or Bitbucket, last activity, size on disk and how
much of that is build output (`node_modules`, `target`, `bin` and `obj`, `.venv`, `dist`,
`build`, Unreal's `Intermediate`, Unity's `Library` and so on). Git facts come from the git
command line, run read-only (`--no-optional-locks`, so even `git status` leaves the index file
alone) in a pool of three with a timeout, and are refreshed when files in the project change or
the hub is opened. The status (active, paused, archived, idea), tags, Markdown notes and
pinning are the user's own, kept in `kestrel.db`.

Selecting a project shows its README, notes, scripts (`package.json` scripts, Cargo's build,
test and run, justfile recipes and Makefile targets, each run in a new Windows Terminal in the
project folder), launchers (VS Code, Visual Studio for a `.sln`, Windows Terminal, Claude Code,
the folder in Kestrel or Explorer, the web page), a year of activity (commits and changed
files per day) and the files changed most recently that git does not ignore.

In a folder inside a repository, every view marks items git sees as modified (a dot), added
(a plus), untracked (a ring) or conflicted (a warning sign), a folder showing the most pressing
change inside it, and dims what git ignores. Only the items on screen are asked about. Search
results show no git marks: they can span any number of repositories, and git would have to run
in each.

`@` in the command palette lists the projects too, matched by name, or by stack, tag, status
or branch (`@rust`, `@paused`): Enter opens the project's folder, Shift+Enter its page in the
hub. Typing `@` looks for projects as opening the hub does.

Kestrel never runs a program a repository's own configuration names, so opening a downloaded
or extracted repository is safe: git's file system monitor hook and every filter driver the
repository's configuration sets up are switched off for its commands (drivers you installed
yourself, such as Git LFS, stay on), and `git status` does not look inside submodules' working
trees, whose configurations it would have to trust too. A submodule on another commit is
marked; uncommitted changes inside one are not. Claude Code is started from the PATH only,
never from a `claude.cmd` in the project folder.

## Command palette and quick launcher

Ctrl+K opens the command palette over the window: one list, ranked by a fuzzy match as you
type, of every action the app has (with its shortcut), the folders and files in the index, the
pinned places (with their number keys), the frequent folders, saved searches and workspaces,
and the projects already found. A folder that comes from several of these is listed once.
Matched letters are highlighted, and what you open often and lately ranks higher (folders you
visit count too). Start with a prefix to narrow it:

| Prefix | Lists |
|---|---|
| `>` | actions |
| `/` | a path: `/C:\Users\me\Doc` lists the folder typed so far, narrowed by the name after it; `/` alone offers drives, your folders and recent ones; `~` is your user folder |
| `@` | projects, by name, stack, tag, status or branch; `@` alone lists them all, pinned first |
| `#` | tags; Enter shows everything carrying one |

Up and Down pick a result. Enter opens a folder in the current tab, a file in its default app,
or runs an action; Ctrl+Enter opens a folder in a new tab; Shift+Enter shows the item in its
folder, selected (a project's page in the projects hub); Tab completes the chosen folder or
file into the path, so its contents can be listed. Words separated by spaces must all match,
and `^word`, `word$`, `'word` and `!word` work as in fzf. Keys typed into the palette stay in
it: the window's own shortcuts wait until it closes. With nothing typed, the palette lists what
you used lately, as it is now (a renamed pin under its new name; a deleted saved search not at
all), then the actions.

The quick launcher is the same palette in a small window of its own, above every other app.
Ctrl+Alt+Space summons it from anywhere in Windows (change or turn off the shortcut with
"Quick launcher settings" in the palette, or from the tray icon); Enter acts and hides it,
Escape or a click elsewhere hides it. Folders and actions open in Kestrel's window, which comes
forward; files open in their own apps. If another app already owns the shortcut, the settings
say so and Kestrel carries on without it.

Kestrel has an icon in the notification area with Show Kestrel, the quick launcher, the
settings and Quit. "Keep running in the tray when the window is closed" (off by default) keeps
the index current and the shortcut working after the window closes. "Start Kestrel when you
sign in to Windows" (off by default) adds an entry to your account's startup programs; with the
tray option on, Kestrel then starts in the tray. Only one Kestrel runs at a time: starting it
again (from the Start menu, say) shows the one already running.

## Explorer parity

What Windows' File Explorer does that Kestrel also does now (the full checklist, with what is
left out and why, is `docs/explorer-parity.md`):

- **Command bar** under the address bar: New, Cut, Copy, Paste, Rename, Delete, Sort, and a "…"
  menu with Undo, Redo, Pin, Select all, Select none, Invert selection, Open in Terminal, New
  window, Full screen, Properties and Options; Details at its right.
- **New** offers every file type Windows registers for it (`ShellNew`: Office documents,
  compressed folders, and so on), made from the registered template or bytes and then named in
  place, plus Shortcut, which asks what it points at. A Text Document is always offered.
- **Home**: Quick access (the folders you open most), Favorites (your pins) and Recent (files
  opened lately, in any program as Windows records them, or through Kestrel). Type "Home" in the
  address bar, or click Home at the top of the sidebar; Folder Options can make Kestrel open to it.
- **Details pane** (Alt+Shift+P), in the preview pane's place: the selection's size, dates,
  attributes, availability and what Windows knows of it (dimensions, length, authors...), or the
  totals of several items, or the folder's own details.
- **More windows** (Ctrl+N, or "Open in new window"), in the same process, sharing the index,
  pins and undo history. Closing one leaves the others open.
- **Open in Terminal** opens Windows Terminal in a folder; **Open file location** shows a search
  result in its folder; **Alt+drop** makes shortcuts; **Shift+Delete** asks, as Explorer does,
  before deleting for good; Alt+Enter with nothing selected shows the folder's properties.
- **Folder Options** ("…" > Options): open to Home or your user folder, single- or double-click
  to open, the full path in the tabs, hidden files, protected operating system files (hidden by
  default, as in Explorer), extensions, and sync status.
- **Sync status**: in OneDrive and other synced folders, every view shows whether an item is
  online-only (a cloud), on this device (a ringed check) or always kept on it (a filled check),
  read from its attributes, so nothing is downloaded.
- The status bar adds up the selection's size and has Explorer's Details and Large icons buttons.

## Testing

```powershell
cargo test --workspace                 # engine and backend unit tests
cargo run --release -p kestrel-cli -- index C:\       # crawl and save, with a journal position
cargo run --release -p kestrel-cli -- catch-up        # apply journal changes since the save
cargo run --release -p kestrel-cli -- bench
cargo run --release -p kestrel-cli -- mft C:\ --compare   # as administrator: read the MFT, diff against the crawl
cargo run --release -p kestrel-cli -- remote C:\      # ask the installed broker for an index
```

End-to-end, against the real window. Each test instance ("slot") has its own ports, app
identity, build folder and data folder, so it never disturbs your own copy of Kestrel; the
first start of a slot builds the app and takes a few minutes:

```powershell
cd apps\desktop
$env:KESTREL_SLOT = 6
node e2e/dev.mjs start      # builds if needed and returns once the window is up
node e2e/smoke.mjs          # navigation, search, sorting; read-only
node e2e/fileops.mjs        # file operations, in a scratch folder under %TEMP%
node e2e/live-index.mjs     # files changed by another program show up in search
node e2e/views.mjs          # view modes, thumbnails, hidden items and extensions
node e2e/undo.mjs           # undo and redo
node e2e/undo-restart.mjs   # the undo history after a restart (restarts the slot itself)
node e2e/batchrename.mjs    # batch rename and its preview
node e2e/locked.mjs         # a folder this account can't open shows locked
node e2e/preview.mjs        # preview pane and Quick Look
node e2e/dragdrop.mjs       # drops driven by synthetic drag events; drags out rehearsed
node e2e/shellmenu.mjs      # Windows menu entries start the helper
node e2e/shortcuts.mjs      # pins, groups, Go to folder, saved searches, tags, workspaces
node e2e/integration.mjs    # the features together, in the icon, list and tile views
node e2e/projects.mjs       # the projects hub and git badges, with repositories it makes
node e2e/palette.mjs        # command palette, quick launcher, shortcut settings and tray option
node e2e/connections.mjs    # where the palette, shortcuts, projects and preview meet
node e2e/parity.mjs         # Explorer parity: New, Home, the details pane, a second window, shortcuts
node e2e/dev.mjs stop
```

With `$env:KESTREL_RELEASE = 1` set as well, `dev.mjs start` makes the slot a release build
instead: it runs `tauri build` (no installer) with the slot's identity and starts the
`kestrel.exe` that produced, whose front end is built in. `smoke.mjs`, `fileops.mjs`,
`live-index.mjs`, `views.mjs`, `undo.mjs`, `batchrename.mjs`, `locked.mjs`, `preview.mjs`,
`shellmenu.mjs` and `undo-restart.mjs` run against either; `dragdrop.mjs`,
`integration.mjs`, `shortcuts.mjs`, `projects.mjs`, `palette.mjs`, `connections.mjs` and
`parity.mjs` need a dev build, whose test hooks they use. A release build has the Windows
menu's test hook only when started with `KESTREL_TEST_HOOKS=1`, as `dev.mjs` starts it.

`parity.mjs` opens a second window and closes it, turns the window to full screen and back,
and opens two native dialogs (Shift+Delete's confirmation and a folder's Properties), which it
finds by their windows and closes, answering No. Outside its scratch folder it visits
`C:\Windows\Help` (forgotten again if the visit was its own), pins and unpins a folder of its
own, and records and forgets a file of its own as opened; Windows Terminal is not started (the
projects feature's hook records the launch). `connectToKestrel()` in `e2e/connect.mjs` returns
the first window, `main`, when there are several; `waitForKestrelWindow(browser, label)` finds
another.

Run the suites one after another, not side by side: several use the clipboard or the window's
settings. Every suite works in its own scratch folder under `%TEMP%` and removes it afterwards;
`views.mjs` and `integration.mjs` save the window's view and preview settings first and put
them back at the end. Screenshots, in the dark and light themes, go to `e2e/out`.

`dragdrop.mjs` and `integration.mjs` need a dev build: they drive drops through the test hook
such a build exposes, since a real mouse drag can't be scripted. `undo-restart.mjs` stops and
starts the slot's app itself, between two runs of operations. Drags out are rehearsed rather
than performed, so nothing is ever dropped on another program. `shellmenu.mjs` and
`integration.mjs` switch the app's development hook on, which makes the Windows menu helper
list the menu instead of showing it; showing the real menu and the Open With dialog are manual
checks.

`connections.mjs` checks the padlock, git marks and tag dots together in all seven views, the
palette's projects, tags, pins, frequent folders, saved searches and workspaces, the projects
hub closing when a pin opens, and the preview's webview hiding under the palette, Go to folder
and the hub. It makes a repository in its scratch folder, locks one folder in it with `icacls`
for a moment, and removes the pins, tags, saved search and workspace it makes.

`projects.mjs` makes repositories in its scratch folder with the git command line and needs
a dev build: projects are found by the location rules, which leave out the temp folder, so it
turns on a development hook that allows its folder and records what Run and the launchers
would start instead of starting it. Opening VS Code, Windows Terminal or a browser for real is
a manual check.

`fileops.mjs` replaces the clipboard's contents, and with `KESTREL_E2E_RECYCLE=1` it also
sends one small file to the Recycle Bin. `undo.mjs` and `batchrename.mjs` (undo, redo, batch
rename) and `locked.mjs` (a folder the account can't open) leave the clipboard alone; with
`KESTREL_E2E_RECYCLE=1`, `undo.mjs` also undoes a new folder and a deletion, which passes two
items from a scratch folder named `kestrel-test-*` through the Recycle Bin and brings both
back. The unit test `undo::tests::restores_recycled_items_from_the_recycle_bin` does the same
with two tiny files named `kestrel-test-*` and runs by default, and the helper's CLI test
`invoking_copy_leaves_the_file_on_the_clipboard` replaces the clipboard's contents.
`locked.mjs` and `integration.mjs` use `icacls` to remove, then restore, the permissions of
one scratch folder.
