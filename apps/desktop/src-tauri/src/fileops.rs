//! File operations carried out by the Windows shell's own copy engine, so progress, conflict
//! prompts, elevation, the Recycle Bin and Explorer's undo all behave as they do in Explorer.

use std::path::Path;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI,
    FOF_RENAMEONCOLLISION, FOF_SILENT, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
    FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName,
};
use windows::core::{HRESULT, HSTRING, PCWSTR};

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// The user pressed Cancel in a shell dialog.
const CANCELLED: [HRESULT; 2] = [HRESULT(0x800704C7_u32 as i32), HRESULT(0x80270000_u32 as i32)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Copy,
    Move,
}

#[derive(Debug)]
pub enum Request {
    /// Moves items to the Recycle Bin, or erases them outright when `permanent`.
    Delete { paths: Vec<String>, permanent: bool },
    Rename { path: String, new_name: String },
    NewFolder { parent: String, name: String },
    Transfer { sources: Vec<String>, destination: String, kind: Transfer },
}

#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Window that owns the shell's dialogs, as a raw `HWND`.
    pub owner: Option<isize>,
    /// Suppress every dialog and answer prompts with their defaults. For tests.
    pub quiet: bool,
}

/// Balances `CoInitializeEx` on the operation's thread.
struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful CoInitializeEx on this thread.
        unsafe { CoUninitialize() };
    }
}

/// Checks a single file or folder name the way Windows would reject it.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        return Err("Type a name.".into());
    }
    if let Some(bad) = name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        return Err(format!("A name can't contain {bad}"));
    }
    Ok(())
}

/// The first of `base`, `base (2)`, `base (3)`… that does not exist in `parent`.
pub fn unused_name(parent: &str, base: &str) -> String {
    (1..)
        .map(|n| if n == 1 { base.to_owned() } else { format!("{base} ({n})") })
        .find(|name| !Path::new(parent).join(name).exists())
        .expect("an unused name exists")
}

/// Performs `request` and waits for it. Returns `Ok(false)` if the user cancelled.
///
/// The shell's copy engine needs a single-threaded apartment and pumps its own messages while
/// it works, so each operation gets a thread of its own and several can run at once.
pub fn perform(request: Request, options: Options) -> Result<bool, String> {
    std::thread::Builder::new()
        .name("kestrel-fileop".into())
        .spawn(move || run(&request, options))
        .map_err(|e| e.to_string())?
        .join()
        .map_err(|_| "The file operation crashed".to_owned())?
}

fn shell_item(path: &str) -> Result<IShellItem, String> {
    // The shell's parser rejects forward slashes, which other parts of Windows accept.
    let path = &path.replace('/', "\\");
    // SAFETY: the path is a valid wide string for the duration of the call.
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_str()), None) }
        .map_err(|e| format!("{path}: {}", e.message()))
}

fn run(request: &Request, options: Options) -> Result<bool, String> {
    let describe = |e: windows::core::Error| e.message();
    // SAFETY: COM is initialised for this thread before any COM call and uninitialised by
    // `Apartment` after the last interface pointer is dropped; every pointer passed to the
    // shell below outlives the call it is passed to.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(describe)?;
        let _apartment = Apartment;

        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(describe)?;
        let mut flags: FILEOPERATION_FLAGS = FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD;
        if options.quiet {
            flags |= FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI;
        }
        if let Some(owner) = options.owner {
            operation.SetOwnerWindow(HWND(owner as _)).map_err(describe)?;
        }

        match request {
            Request::Delete { paths, permanent } => {
                flags |= if *permanent { FOF_WANTNUKEWARNING } else { FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE };
                for path in paths {
                    operation.DeleteItem(&shell_item(path)?, None).map_err(describe)?;
                }
            }
            Request::Rename { path, new_name } => {
                validate_name(new_name)?;
                flags |= FOF_ALLOWUNDO;
                operation
                    .RenameItem(&shell_item(path)?, &HSTRING::from(new_name.as_str()), None)
                    .map_err(describe)?;
            }
            Request::NewFolder { parent, name } => {
                validate_name(name)?;
                flags |= FOF_ALLOWUNDO;
                operation
                    .NewItem(
                        &shell_item(parent)?,
                        FILE_ATTRIBUTE_DIRECTORY,
                        &HSTRING::from(name.as_str()),
                        PCWSTR::null(),
                        None,
                    )
                    .map_err(describe)?;
            }
            Request::Transfer { sources, destination, kind } => {
                flags |= FOF_ALLOWUNDO;
                let target = shell_item(destination)?;
                let in_place = |source: &String| {
                    Path::new(source).parent().is_some_and(|parent| same_folder(parent, destination))
                };
                // Copying into the folder the items already live in makes "name - Copy", as
                // Explorer does, instead of asking to overwrite each item with itself.
                if *kind == Transfer::Copy && sources.iter().all(in_place) {
                    flags |= FOF_RENAMEONCOLLISION;
                }
                let mut queued = 0;
                for source in sources {
                    // Moving an item to where it already is does nothing.
                    if *kind == Transfer::Move && in_place(source) {
                        continue;
                    }
                    let item = shell_item(source)?;
                    match kind {
                        Transfer::Copy => operation.CopyItem(&item, &target, PCWSTR::null(), None),
                        Transfer::Move => operation.MoveItem(&item, &target, PCWSTR::null(), None),
                    }
                    .map_err(describe)?;
                    queued += 1;
                }
                if queued == 0 {
                    return Ok(true);
                }
            }
        }

        operation.SetOperationFlags(flags).map_err(describe)?;
        match operation.PerformOperations() {
            Ok(()) => Ok(!operation.GetAnyOperationsAborted().map_err(describe)?.as_bool()),
            Err(e) if CANCELLED.contains(&e.code()) => Ok(false),
            Err(e) => Err(describe(e)),
        }
    }
}

/// Whether `folder` and `other` name the same folder, ignoring case and a trailing separator.
fn same_folder(folder: &Path, other: &str) -> bool {
    let trim = |text: &str| text.trim_end_matches(['\\', '/']).to_lowercase();
    folder.to_str().is_some_and(|folder| trim(folder) == trim(other))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const QUIET: Options = Options { owner: None, quiet: true };

    /// A scratch folder under the temp directory; every test stays inside its own.
    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kestrel-fileops-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn validates_names() {
        assert!(validate_name("notes.txt").is_ok());
        assert!(validate_name("  ").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("what?").is_err());
    }

    #[test]
    fn creates_renames_and_erases() {
        let dir = sandbox("basic");
        assert_eq!(unused_name(&text(&dir), "New folder"), "New folder");
        perform(Request::NewFolder { parent: text(&dir), name: "New folder".into() }, QUIET).unwrap();
        assert!(dir.join("New folder").is_dir());
        assert_eq!(unused_name(&text(&dir), "New folder"), "New folder (2)");

        perform(Request::Rename { path: text(&dir.join("New folder")), new_name: "Renamed".into() }, QUIET)
            .unwrap();
        assert!(dir.join("Renamed").is_dir() && !dir.join("New folder").exists());
        assert!(perform(Request::Rename { path: text(&dir.join("Renamed")), new_name: "a:b".into() }, QUIET).is_err());

        fs::write(dir.join("Renamed/file.txt"), b"x").unwrap();
        perform(Request::Delete { paths: vec![text(&dir.join("Renamed"))], permanent: true }, QUIET).unwrap();
        assert!(!dir.join("Renamed").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_and_moves() {
        let dir = sandbox("transfer");
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("dst")).unwrap();
        fs::write(dir.join("src/a.txt"), b"alpha").unwrap();
        fs::write(dir.join("src/nested/b.txt"), b"beta").unwrap();
        let sources = vec![text(&dir.join("src/a.txt")), text(&dir.join("src/nested"))];

        let copy = Request::Transfer { sources: sources.clone(), destination: text(&dir.join("dst")), kind: Transfer::Copy };
        assert!(perform(copy, QUIET).unwrap());
        assert_eq!(fs::read(dir.join("dst/a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(dir.join("dst/nested/b.txt")).unwrap(), b"beta");
        assert!(dir.join("src/a.txt").exists(), "a copy leaves the source in place");

        // Copying into the same folder keeps the original and adds a renamed duplicate.
        let duplicate = Request::Transfer {
            sources: vec![text(&dir.join("src/a.txt"))],
            destination: text(&dir.join("src")),
            kind: Transfer::Copy,
        };
        perform(duplicate, QUIET).unwrap();
        let copies = fs::read_dir(dir.join("src")).unwrap().count();
        assert_eq!(copies, 3, "a.txt, its duplicate and nested");

        fs::create_dir_all(dir.join("moved")).unwrap();
        let mv = Request::Transfer { sources, destination: text(&dir.join("moved")), kind: Transfer::Move };
        perform(mv, QUIET).unwrap();
        assert!(dir.join("moved/a.txt").exists() && dir.join("moved/nested/b.txt").exists());
        assert!(!dir.join("src/a.txt").exists() && !dir.join("src/nested").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Leaves one small file in the real Recycle Bin, so it only runs when asked for:
    /// `cargo test -p kestrel-desktop -- --ignored recycles`.
    #[test]
    #[ignore]
    fn recycles() {
        let dir = sandbox("recycle");
        let file = dir.join("kestrel-recycle-test.txt");
        fs::write(&file, b"safe to delete").unwrap();
        perform(Request::Delete { paths: vec![text(&file)], permanent: false }, QUIET).unwrap();
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
