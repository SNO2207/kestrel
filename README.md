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
open, and passes them to the app.

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
| F2 | Rename |
| Del / Shift+Del | Move to the Recycle Bin / delete permanently |
| Ctrl+C / Ctrl+X / Ctrl+V | Copy / cut / paste, shared with Explorer's clipboard |
| Ctrl+Shift+N | New folder |
| Ctrl+Shift+C | Copy as path |
| Alt+Enter | Properties |
| Right-click, Shift+F10 | Context menu |
| Alt+P | Show or hide the preview pane (drag its left edge to resize it) |
| Space | Quick Look; Space or Esc closes it, the arrow keys move to the previous or next item |

File operations run through the Windows shell's own copy engine, so progress and conflict
dialogs, the Recycle Bin and Explorer's undo behave exactly as they do in Explorer.

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
node e2e/preview.mjs    # preview pane and Quick Look, confined to a scratch folder under %TEMP%
```

The preview pane shows images (SVG only through `<img>`), video, audio, PDF (the web view's own
viewer), text and code with highlighting (the first megabyte), Markdown rendered and sanitised,
JSON re-indented, and details for folders and everything else. File bytes reach it through the
`preview` protocol (`src-tauri/src/preview.rs`), which serves regular files only, honours
`Range`, shows HTML and other active content as text, and whose frames may not call the app's
commands.

`fileops.mjs` replaces the clipboard's contents, and with `KESTREL_E2E_RECYCLE=1` it also
sends one small file to the Recycle Bin. One unit test does the same and is ignored by
default: `cargo test -p kestrel-desktop -- --ignored recycles`.
