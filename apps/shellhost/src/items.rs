//! Turns paths into the shell's item ID lists and asks their folder for a context menu.

use std::path::Path;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IShellFolder, ILClone, ILFindLastID, ILFree, ILIsEqual, ILRemoveLastID,
    SHBindToObject, SHBindToParent, SHParseDisplayName,
};
use windows::core::HSTRING;

/// An absolute item ID list owned by this process, freed when dropped.
struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &str) -> Result<Self, String> {
        let mut pidl = std::ptr::null_mut();
        // SAFETY: the path is a valid wide string for the duration of the call, and `pidl`
        // receives an allocation that this `Pidl` frees.
        unsafe { SHParseDisplayName(&HSTRING::from(path), None, &mut pidl, 0, None) }
            .map_err(|e| format!("Couldn't find {path}: {}", e.message()))?;
        Ok(Self(pidl))
    }

    /// The list for the item's parent folder.
    fn parent(&self) -> Result<Self, String> {
        // SAFETY: `self.0` is a valid absolute list; the clone is owned by the new `Pidl`.
        let parent = Self(unsafe { ILClone(self.0) });
        if parent.0.is_null() {
            return Err("Out of memory".into());
        }
        // SAFETY: `parent.0` is a valid list owned here, shortened in place.
        unsafe {
            let _ = ILRemoveLastID(Some(parent.0));
        }
        Ok(parent)
    }

    /// The last ID in the list: the item relative to its parent folder. Valid while `self` is.
    fn child(&self) -> *const ITEMIDLIST {
        // SAFETY: `self.0` is a valid list; the result points inside it.
        unsafe { ILFindLastID(self.0) }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: the list was allocated by the shell and is freed exactly once.
        unsafe { ILFree(Some(self.0)) };
    }
}

/// Items that share one parent folder: the only kind of selection a shell context menu can
/// describe, because the menu comes from that folder.
pub struct Items {
    folder: IShellFolder,
    pidls: Vec<Pidl>,
    /// Working directory for the commands: the folder the items are in.
    pub directory: String,
}

pub const DIFFERENT_FOLDERS: &str = "These items are in different folders. The Windows menu works on items from \
     one folder at a time, so select items from a single folder.";

impl Items {
    pub fn parse(paths: &[String]) -> Result<Self, String> {
        let pidls = paths.iter().map(|path| Pidl::parse(path)).collect::<Result<Vec<_>, _>>()?;
        let first = pidls.first().ok_or("No items given")?;
        let parent = first.parent()?;
        for pidl in &pidls[1..] {
            // SAFETY: both lists are valid absolute lists.
            if !unsafe { ILIsEqual(parent.0, pidl.parent()?.0) }.as_bool() {
                return Err(DIFFERENT_FOLDERS.into());
            }
        }
        // SAFETY: `first.0` is a valid absolute list; the child pointer it returns is not kept.
        let folder: IShellFolder = unsafe { SHBindToParent(first.0, None) }
            .map_err(|e| format!("Couldn't open the folder of {}: {}", paths[0], e.message()))?;
        let path = Path::new(&paths[0]);
        // A drive root's parent is This PC, which has no path; commands then run in the root.
        let directory = path.parent().unwrap_or(path).to_string_lossy().into_owned();
        Ok(Self { folder, pidls, directory })
    }

    /// The folder's context menu for these items.
    pub fn context_menu(&self, owner: HWND) -> Result<IContextMenu, String> {
        let children: Vec<*const ITEMIDLIST> = self.pidls.iter().map(Pidl::child).collect();
        // SAFETY: every child pointer points into a list in `self.pidls`, which outlives the call.
        unsafe { self.folder.GetUIObjectOf(owner, &children, None) }
            .map_err(|e| format!("Windows has no menu for these items: {}", e.message()))
    }
}

/// The context menu of a folder's background, as Explorer shows for a click on empty space.
pub fn background_menu(path: &str, owner: HWND) -> Result<IContextMenu, String> {
    let pidl = Pidl::parse(path)?;
    // SAFETY: `pidl.0` is a valid absolute list, bound relative to the desktop.
    let folder: IShellFolder = unsafe { SHBindToObject(None, pidl.0, None) }
        .map_err(|e| format!("Couldn't open {path}: {}", e.message()))?;
    // SAFETY: plain COM call on a live interface.
    unsafe { folder.CreateViewObject(owner) }
        .map_err(|e| format!("Windows has no menu for {path}: {}", e.message()))
}
