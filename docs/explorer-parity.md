# Explorer parity

What Windows 11's File Explorer does, and whether Kestrel does it. Each line is **done**,
**partial** or **missing**, with where it lives in Kestrel; what the five branches built
alongside this one add is marked **in progress** with the branch's name:

- *places*: This PC, the Recycle Bin page, the folder tree and address completion;
- *layout*: a second pane, grouping, columns, a filter box, marquee selection, the Content view;
- *archives*: compressed folders;
- *disk usage* and *cleanup*.

Paths are relative to `apps/desktop` unless they start with `crates/`. The parity branch's own
work is in `src-tauri/src/parity/`, `src/parity.ts`, `src/lib/{home,folderOptions,syncStatus,selectionOps,parityApi}.ts`,
`src/components/{CommandBar,DetailsPane,HomeView,HomeEntry,FolderOptions,NewShortcutDialog,ParityLayer,ParityIcon,StatusExtras,SyncGlyph}.tsx`,
`src/parity.css`, `crates/kestrel-db/src/parity.rs`, and `e2e/parity.mjs`, which tests it.

## Command bar

| Explorer | Status | Where |
|---|---|---|
| New > Folder | done | `CommandBar.tsx` (New menu), Ctrl+Shift+N, `main.rs` `new_folder` |
| New > Shortcut | done | `NewShortcutDialog.tsx` asks for the target and name (Explorer's wizard, on one page); `parity/links.rs` writes the `.lnk` through `IShellLink` |
| New > every type Windows offers (`ShellNew`) | done | `parity/shellnew.rs`: `NullFile`, `Data` and `FileName` entries under `HKCR\.ext\<ProgID>\ShellNew` or `HKCR\.ext\ShellNew`, named by `ItemName` or "New" and the type's name, then renamed in place. On this machine: Office types, Compressed (zipped) Folder, WinRAR, Text Document. Entries made by a `Command` or a `Handler` (other than shortcuts) and opt-in entries (libraries) are left out: Kestrel never runs a program the registry names. Kestrel offers Text Document even where Windows no longer registers it |
| Cut, Copy, Paste | done | `CommandBar.tsx`; Paste greys out when the clipboard holds no files |
| Rename | done | `CommandBar.tsx` (F2's behaviour: in place, or batch rename for several) |
| Share | partial | The Windows menu ("Show more options") has Windows' Share. A button of Kestrel's own needs the WinRT share sheet (`DataTransferManager`); left out |
| Delete | done | `CommandBar.tsx`; Shift+Delete deletes permanently after Windows' own confirmation (`fileops.rs`, checked by `e2e/parity.mjs`) |
| Sort | done | `CommandBar.tsx` Sort menu, and the View menu (`ViewMenu.tsx`) |
| Group by | in progress: layout | |
| View (layouts, panes, extensions, hidden items) | partial | `ViewMenu.tsx`: every layout but Content (in progress: layout), hidden items, extensions; preview pane (Alt+P) and details pane (Alt+Shift+P) on their own buttons; compact view, item check boxes and hiding the navigation pane are missing |
| Filter | in progress: layout | |
| "..." menu: Undo, Pin to Quick access, Select all, Select none, Invert selection, Properties, Options | done | `CommandBar.tsx` More menu (Pin to sidebar stands for Pin to Quick access); also Redo, Open in Terminal, New window, Full screen |
| "..." menu on This PC: Map network drive, Disconnect, Add a network location | missing | Needs `WNetConnectionDialog`; This PC is in progress: places |
| "..." menu: Optimize, Compress to ZIP | in progress: archives (compress) | Optimize is the drive tool's; missing |

## Context menu

| Explorer | Status | Where |
|---|---|---|
| Cut, Copy, Rename, Delete (the icon row) | done | `FileList.tsx` item menu, as items |
| Share (icon row) | partial | Show more options |
| Open, Open with | done | `FileList.tsx`, `shellmenu.rs` |
| Open in new tab | done | `FileList.tsx` |
| Open in new window | done | `FileList.tsx` (parity), Ctrl+N |
| Open in Terminal | done | `FileList.tsx` item (folders) and background menus, More menu, palette; `parity/terminal.rs` |
| Open file location (search results) | done | `FileList.tsx`, palette; `parity.ts` `revealInFolder` |
| Pin to Quick access / Add to Favorites | done | Pin to sidebar (Ctrl+D), shown on Home as Favorites |
| Pin to Start, Edit, Set as background, Rotate, Print | partial | Show more options (the Windows menu, `shellmenu.rs`) |
| Compress to, Extract all | in progress: archives | |
| Copy as path | done | Ctrl+Shift+C |
| Properties | done | Alt+Enter; with nothing selected, the folder's (parity) |
| Show more options | done | Shift+F10, `shellmenu.rs` |
| Background: New > | done | `FileList.tsx` folder menu, "New" opens the New menu |
| Background: Paste, Undo, Refresh, Properties | done | `FileList.tsx` |
| Background: View, Sort by | partial | In the View and Sort menus, not the context menu |
| Background: Group by | in progress: layout | |

## Panes

| Explorer | Status | Where |
|---|---|---|
| Details pane (Alt+Shift+P) | done | `DetailsPane.tsx`: one item's type, size, dates, attributes, availability, location and what the Windows property system knows (dimensions, length, frame rate, bit rate, title, authors, pages, date taken, camera; `parity/details.rs`, not read for cloud-only files); several items' count, total size and dates; the folder's own when nothing is selected. Shares the right of the window with the preview pane: opening one closes the other |
| Preview pane (Alt+P) | done | `PreviewPane.tsx` |
| Navigation pane | partial | `Sidebar.tsx`: Home, pinned places, known folders, drives, frequent folders; the folder tree is in progress: places |
| Hide the navigation pane | missing | |

## Home, Gallery and the navigation pane's places

| Explorer | Status | Where |
|---|---|---|
| Home | done | `HomeView.tsx`, `lib/home.ts`: "Home" in the address bar, the sidebar's Home entry, Back and Forward; a place Kestrel can open to (Folder Options) |
| Home > Quick access | done | Frequent folders (`kestrel_db::shortcuts::frecency`), as tiles |
| Home > Favorites | done | Kestrel's pins (`shortcuts`) |
| Home > Recent | done | Windows' Recent items (shortcuts in `shell:recent`, read without touching their targets) and files opened through Kestrel (`kestrel_db::parity`); Remove from Recent hides an entry in Kestrel |
| Home > Shared, Recommended | missing | Need a Microsoft account's cloud data |
| Gallery | missing | A photo timeline over the Pictures folders; the icon views and `type:image in:<folder>` cover browsing; the planned photo search is its natural home |
| This PC | in progress: places | |
| Recycle Bin | in progress: places | |
| OneDrive node | partial | The OneDrive folder is reachable like any folder; sync status shows in it (below) |
| Network | partial | UNC paths open (listed live, not indexed); network drives show under Drives; browsing the network neighbourhood is missing |
| Libraries, Linux (WSL) | missing | |

## Windows and tabs

| Explorer | Status | Where |
|---|---|---|
| New window (Ctrl+N) | done | `parity/appwindows.rs`: windows `main-2`... in the same process, sharing the index, the database and the undo history; each has its own folder watch, drag hook and preview webview; the quick launcher, the tray and a second launch bring forward the window used last; closing one keeps the others running |
| Tabs, Ctrl+T, Ctrl+W, Ctrl+Tab | done | `TitleBar.tsx`, `state.ts` |
| Drag a tab to another window, duplicate tab | missing | |
| Full screen (F11) | done | `ParityLayer.tsx` |

## Status bar

| Explorer | Status | Where |
|---|---|---|
| Item count, selected count | done | `StatusBar.tsx` |
| Size of the selection | done | `StatusExtras.tsx` (folders count with the size the index knows) |
| Details and Large icons switches | done | `StatusExtras.tsx` |

## Keyboard shortcuts

Every File Explorer shortcut Microsoft documents, and what Kestrel does with it.

| Keys | Explorer | Status |
|---|---|---|
| Alt+D, Ctrl+L | Select the address bar | done |
| F4 | The address bar (its list) | done: edits the address; the list of completions is in progress: places |
| Ctrl+E, Ctrl+F, F3 | Select the search box | done (Ctrl+Shift+E no longer also does) |
| Ctrl+N | Open a new window | done (parity) |
| Ctrl+W | Close the tab (the window with its last tab) | done |
| Ctrl+T, Ctrl+Tab, Ctrl+Shift+Tab | Tabs | done |
| Ctrl+mouse wheel | Change the size of icons | done (up for larger: from Details to List) |
| Ctrl+Shift+E | Show all folders above the selected folder in the tree | partial: sends `kestrel:reveal-in-tree` for the folder tree (in progress: places) |
| Ctrl+Shift+N | New folder | done |
| Num Lock+*, Num Lock+plus, Num Lock+minus | Expand and collapse in the tree | in progress: places (the tree's keys) |
| Alt+P | Preview pane | done |
| Alt+Shift+P | Details pane | done (parity; Alt+P no longer takes it) |
| Alt+Enter | Properties | done; with nothing selected, the folder's (parity) |
| Alt+Left, Alt+Right, Alt+Up, Backspace | Back, forward, up, back | done |
| Mouse back and forward buttons | Back, forward | done (parity) |
| Right arrow, Left arrow | Expand or collapse in the tree | in progress: places; in the list they move between items |
| Home, End | Top, bottom | done |
| F11 | Full screen | done (parity) |
| F5, Ctrl+R | Refresh | done (Ctrl+R: parity) |
| F2 | Rename | done |
| F6, Shift+F6 | Move between the window's parts | done (parity): address bar, search box, sidebar, files |
| Del, Shift+Del | Delete, delete permanently | done |
| Ctrl+D | Delete (to the Recycle Bin) | differs: Kestrel's Ctrl+D pins (shortcuts); Del deletes |
| Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V, Ctrl+Z, Ctrl+Y | Select all, clipboard, undo, redo | done |
| Ctrl+Shift+C | Copy as path | done |
| Ctrl+Shift+1 … 8 | Layouts | done but 7 (Content: in progress: layout) |
| Shift+F10, Menu key | Context menu | done (Shift+F10 shows the Windows menu) |
| Middle-click a folder | Open in a new tab | done (parity) |
| Alt+drag, Ctrl+Shift+drag | Make shortcuts | done (parity; `dragdrop.rs`, `parity/links.rs`) |
| Ctrl+Space, Ctrl+arrows | Select and move without selecting | missing |
| Alt+F4 | Close the window | done (Windows) |

## Folder Options

| Explorer | Status | Where |
|---|---|---|
| Open File Explorer to (Home, This PC) | done | `FolderOptions.tsx`: Home or your user folder (the default, as Kestrel always opened); This PC is in progress: places |
| Browse folders (same or new window) | missing | Low value with tabs |
| Click items (single or double) | done | Single-click opens, names underline under the pointer; "point to select" is missing |
| Privacy: show recent files, frequent folders; clear history | partial | Remove from Recent and from Quick access per item; no switch to hide them or clear everything |
| Display the full path in the title bar | done | The tabs show the full path (the window's own title stays "Kestrel") |
| Hidden files and folders | done | `ViewMenu.tsx`, `FolderOptions.tsx` |
| Hide protected operating system files | done | `visibility.rs` (`protected`), on by default as in Explorer |
| Hide extensions for known file types | done | |
| Show sync status | done | `lib/syncStatus.ts`, `SyncGlyph.tsx` |
| Always show icons, never thumbnails | missing | |
| Hide empty drives, show drive letters | missing / done (always shown) | |
| Show encrypted or compressed files in colour | missing | |
| Use check boxes to select items | missing | |
| Restore previous folder windows at logon | partial | Workspaces save and restore tabs |
| Navigation pane: expand to open folder, show all folders, show libraries | in progress: places | |
| Apply to folders, reset folders | partial | Each folder remembers its view and sort (`lib/views.ts`) |

## Network and OneDrive

| Explorer | Status | Where |
|---|---|---|
| UNC paths (`\\server\share`) | partial | Opened and listed live; not indexed, so not searched |
| Mapped network drives | partial | Listed under Drives; mapping and disconnecting are missing |
| OneDrive status: online-only, available, always available | done | From each item's attributes inside sync roots (`parity/syncroots.rs`: OneDrive's variables and `SyncRootManager`), in every view and in the details pane. Folders show a status only when pinned or unpinned themselves |
| Always keep on this device, Free up space | partial | Show more options (OneDrive's own menu) |
| Sync progress and errors | missing | |

## Left out, and why

- **Share button**: needs the WinRT share sheet; Windows' own Share is one click away under Show more options.
- **Gallery**: a large view of its own, better built with the planned photo search.
- **Map network drive, Network neighbourhood, Libraries, WSL**: rarely used next to typed UNC paths and pins; This PC (where Explorer puts mapping) is the places branch's.
- **Ctrl+Space and Ctrl+arrows**: list-view selection refinements, little used with Ctrl+click.
- **Check boxes, compact view, colour for compressed files, "always show icons"**: cosmetic view options.
- **ShellNew `Command` and `Handler` entries** (other than shortcuts): they run a program or a shell extension the registry names; Kestrel never does that on its own.
