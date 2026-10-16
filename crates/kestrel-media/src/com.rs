//! COM on the threads that read media: WIC, the property system, Media Foundation and the
//! Windows Runtime's OCR all need it.

use std::sync::OnceLock;

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_LITE, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM initialised on this thread (multithreaded apartment) for as long as it lives.
pub struct Apartment {
    owned: bool,
    /// Not `Send`: it must be dropped on the thread that made it.
    _thread: std::marker::PhantomData<*const ()>,
}

impl Apartment {
    /// Joins the thread to the multithreaded apartment. A thread already in an apartment of
    /// either kind keeps it, which works as well for everything here.
    pub fn enter() -> Self {
        // SAFETY: balanced by `CoUninitialize` in `drop` when it succeeds, on the same thread.
        let owned = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Apartment { owned, _thread: std::marker::PhantomData }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the successful `CoInitializeEx` in `enter`, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Starts Media Foundation once for the process; it is never shut down, as the process may read
/// videos at any time until it ends.
pub fn media_foundation() -> Result<(), String> {
    static STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    STARTED
        // SAFETY: MFStartup has no preconditions; the lite start leaves out network sources,
        // which nothing here reads.
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(|e| format!("Media Foundation isn't available: {}", e.message())))
        .clone()
}
