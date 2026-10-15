# Kestrel

A fast file explorer for Windows 11. One in-memory index of every file on the drive powers
instant search, instant folder listings with folder sizes, and (later) disk analysis and
project discovery.

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

## Keyboard

| Keys | Action |
|---|---|
| Ctrl+L, Alt+D | Edit the address |
| Ctrl+F, Ctrl+E, F3 | Search |
| Alt+Left / Alt+Right / Alt+Up, Backspace | Back / forward / up |
| Ctrl+T / Ctrl+W / Ctrl+Tab | New tab / close tab / next tab |
| F5 | Refresh (folders also refresh themselves when their contents change) |
| Type letters | Jump to a name |
| F2 | Rename; with several items selected, batch rename |
| Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z) | Undo / redo the last file operation |
| Del / Shift+Del | Move to the Recycle Bin / delete permanently |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste, shared with Explorer's clipboard |
| Ctrl+Shift+N | New folder |
| Ctrl+Shift+C | Copy as path |
| Alt+Enter | Properties |
| Right-click, Menu key | Context menu |
| Shift+right-click, Shift+F10 | The Windows context menu, with every shell extension's commands |
| Ctrl+Shift+1 / 2 / 3 / 4 | Extra large / large / medium / small icons |
| Ctrl+Shift+5 / 6 / 8 | List / details / tiles |
| Ctrl+wheel | Larger or smaller icons |
| Alt+P | Show or hide the preview pane (drag its left edge to resize it) |
| Space | Quick Look; Space or Esc closes it, the arrow keys move to the previous or next item |
| Ctrl+K | Command palette: every action, and folders and files from the index |
| Ctrl+Alt+Space | The quick launcher, from anywhere in Windows (changeable) |

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
pastes, batch renames and drops all go into the history (up to 100 operations, kept while the
app runs); commands chosen from the Windows context menu are carried out by Windows and do not.

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
file extensions".
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
which serves regular files only, honours `Range`, shows HTML and other active content as
text, and whose frames may not call the app's commands. The window itself never leaves the
app's page (`src-tauri/src/navigation.rs`): web links open in the default browser.

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
or Shift to move. A hint at the bottom of the list says which will happen. Hovering a folder,
place or tab for a second opens it, and the list scrolls when the cursor nears its top or
bottom edge (its left or right edge in the list view, whose columns scroll sideways). Dropped
items are selected where they land, and Ctrl+Z puts them back.

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

`tauri build` does not build or bundle the helper yet. For a release, build it with
`cargo build --release -p kestrel-shellhost`, copy it to
`apps/desktop/src-tauri/binaries/kestrel-shellhost-x86_64-pc-windows-msvc.exe`, add
`"externalBin": ["binaries/kestrel-shellhost"]` under `bundle` in `tauri.conf.json`, and sign
it with the app; Tauri puts sidecars next to the main executable, where the app looks.

## Command palette and quick launcher

Ctrl+K opens the command palette over the window: one list, ranked by a fuzzy match as you
type, of every action the app has (with its shortcut), the folders and files in the index, and
whatever else other features add. Matched letters are highlighted, and what you open often and
lately ranks higher (folders you stay in count too). Start with a prefix to narrow it:

| Prefix | Lists |
|---|---|
| `>` | actions |
| `/` | a path: `/C:\Users\me\Doc` lists the folder typed so far, narrowed by the name after it; `/` alone offers drives, your folders and recent ones; `~` is your user folder |
| `@` | projects |
| `#` | tags |

Up and Down pick a result. Enter opens a folder in the current tab, a file in its default app,
or runs an action; Ctrl+Enter opens a folder in a new tab; Shift+Enter shows the item in its
folder, selected; Tab completes the chosen folder or file into the path, so its contents can be
listed. Words separated by spaces must all match, and `^word`, `word$`, `'word` and `!word`
work as in fzf.

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
tray option on, Kestrel then starts in the tray.

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
node e2e/batchrename.mjs    # batch rename and its preview
node e2e/locked.mjs         # a folder this account can't open shows locked
node e2e/preview.mjs        # preview pane and Quick Look
node e2e/dragdrop.mjs       # drops driven by synthetic drag events; drags out rehearsed
node e2e/shellmenu.mjs      # Windows menu entries start the helper
node e2e/integration.mjs    # the features together, in the icon, list and tile views
node e2e/palette.mjs        # command palette, quick launcher, shortcut settings and tray option
node e2e/dev.mjs stop
```

Run the suites one after another, not side by side: several use the clipboard or the window's
settings. Every suite works in its own scratch folder under `%TEMP%` and removes it afterwards;
`views.mjs` and `integration.mjs` save the window's view and preview settings first and put
them back at the end. Screenshots, in the dark and light themes, go to `e2e/out`.

`dragdrop.mjs` and `integration.mjs` need a dev build: they drive drops through the test hook
such a build exposes, since a real mouse drag can't be scripted. Drags out are rehearsed rather
than performed, so nothing is ever dropped on another program. `shellmenu.mjs` and
`integration.mjs` switch the app's development hook on, which makes the Windows menu helper
list the menu instead of showing it; showing the real menu and the Open With dialog are manual
checks.

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
