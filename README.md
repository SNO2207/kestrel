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
| Right-click, Shift+F10 | Context menu |
| Ctrl+Shift+1 / 2 / 3 / 4 | Extra large / large / medium / small icons |
| Ctrl+Shift+5 / 6 / 8 | List / details / tiles |
| Ctrl+wheel | Larger or smaller icons |

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
and redo are also on the folder background's context menu.

Batch rename (F2 with several items selected) offers find and replace (plain or regular
expression, optionally case-sensitive), a prefix and suffix, numbering (`Photo ###` from a
start value; `*` stands for the current name) and case changes, applied to the name, the
extension or both. A live preview flags conflicts and invalid names, and Apply renames
everything as one undoable operation.

## Views

The View button in the toolbar picks the view (details, list, tiles, or icons from 32 to 256
pixels), the sort order, and two options: "Show hidden items" (off by default; hidden and
system files are then left out of folders and search results) and "Show file extensions".
Each folder remembers its own view and sort, and search results have one of their own.

Icon and tile views show thumbnails rendered by the Windows shell, cached in
`%LOCALAPPDATA%\Kestrel\thumbs` (up to about 1 GB; the oldest go first). Files whose contents
are only in the cloud, such as OneDrive files not kept on this device, show their icon instead,
so browsing never downloads them.

## Testing

```powershell
cargo test --workspace                 # engine and backend unit tests
cargo run --release -p kestrel-cli -- index C:\       # crawl and save, with a journal position
cargo run --release -p kestrel-cli -- catch-up        # apply journal changes since the save
cargo run --release -p kestrel-cli -- bench
cargo run --release -p kestrel-cli -- mft C:\ --compare   # as administrator: read the MFT, diff against the crawl
cargo run --release -p kestrel-cli -- remote C:\      # ask the installed broker for an index
```

End-to-end, against the real window:

```powershell
cd apps\desktop
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9333"
pnpm tauri dev          # in one terminal
node e2e/smoke.mjs      # in another: navigation, search, sorting; read-only
node e2e/fileops.mjs    # file operations, confined to a scratch folder under %TEMP%
node e2e/live-index.mjs # files changed by another program show up in search
node e2e/undo.mjs       # undo and redo, in a scratch folder
node e2e/batchrename.mjs # batch rename and its preview, in a scratch folder
node e2e/locked.mjs     # a scratch folder this account can't open shows locked
```

`fileops.mjs` replaces the clipboard's contents, and with `KESTREL_E2E_RECYCLE=1` it also
sends one small file to the Recycle Bin. `undo.mjs` and `batchrename.mjs` (undo, redo, batch
rename) and `locked.mjs` (a folder the account can't open) work in scratch folders and leave
the clipboard alone; with `KESTREL_E2E_RECYCLE=1`, `undo.mjs` also undoes a new folder and a
deletion, which passes two items from a scratch folder named `kestrel-test-*` through the
Recycle Bin and brings both back. The unit test
`undo::tests::restores_recycled_items_from_the_recycle_bin` does the same with two tiny files
named `kestrel-test-*` and runs by default; `locked.mjs` uses `icacls` to remove, then
restore, the permissions of one scratch folder.
