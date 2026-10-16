//! Who may use the pipe: the account that runs Kestrel, and nobody else.
//!
//! The server creates the pipe with a protected access list naming only that account
//! ([`sddl_for`]), so other users' programs (and services running as other accounts) are refused
//! by Windows before they can send a byte, and the pipe refuses remote clients outright. The
//! client, in turn, checks that the process serving the pipe runs as its own account before
//! sending anything ([`process_user_sid`]): a program of another account that created the
//! pipe's name first could otherwise pose as Kestrel.

use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};

pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Whether `sid` is a plain string SID (`S-1-5-21-...`), and nothing that could add to an
/// access list when pasted into one.
pub fn is_plain_sid(sid: &str) -> bool {
    sid.len() > 4
        && sid.starts_with("S-1-")
        && sid[4..].chars().all(|c| c.is_ascii_digit() || c == '-')
        && !sid.ends_with('-')
        && !sid.contains("--")
}

/// The access list of the pipe for the account `sid`: a protected DACL (nothing inherited) with
/// one entry, full access for that account. Everyone else, other accounts, SYSTEM and
/// Administrators included, has none.
pub fn sddl_for(sid: &str) -> io::Result<String> {
    if !is_plain_sid(sid) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{sid} is not an account SID")));
    }
    Ok(format!("D:P(A;;GA;;;{sid})"))
}

/// A security descriptor built from SDDL, freed when dropped.
pub(crate) struct Descriptor(pub(crate) PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is plain memory owned by this value, read only by the calls it is
// passed to.
unsafe impl Send for Descriptor {}
unsafe impl Sync for Descriptor {}

impl Descriptor {
    pub(crate) fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `text` is NUL-terminated; the descriptor is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(text.as_ptr(), SDDL_REVISION_1, &mut descriptor, null_mut())
        };
        if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(Self(descriptor)) }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// A kernel handle, closed when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle was opened by this module and is closed exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The string SID of the account a token belongs to.
fn token_user_sid(token: HANDLE) -> io::Result<String> {
    // SAFETY: the buffer is sized by the first call and aligned for TOKEN_USER; the SID string
    // is freed with LocalFree as documented.
    unsafe {
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        if GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed) == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = null_mut::<u16>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// The string SID of the account this process runs as.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the pseudo handle needs no closing; the token is closed by `Owned`.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    token_user_sid(token.0)
}

/// The string SID of the account the process `pid` runs as.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: plain FFI; the handles are closed by `Owned`.
    let process = Owned(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) });
    if process.0.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut token: HANDLE = null_mut();
    // SAFETY: `process.0` is a valid process handle for the duration of the call.
    if unsafe { OpenProcessToken(process.0, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Owned(token);
    token_user_sid(token.0)
}

/// The access list of a kernel object (such as a pipe), as SDDL. For the tests: it shows what
/// Windows actually enforces, not what was asked for.
#[cfg(test)]
pub(crate) fn dacl_of(handle: HANDLE) -> io::Result<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `handle` is valid for the call; the descriptor is freed below with LocalFree.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let mut text = null_mut::<u16>();
    let mut length = 0u32;
    // SAFETY: `descriptor` came from GetSecurityInfo; both allocations are freed below.
    unsafe {
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        );
        LocalFree(descriptor);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sddl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_list_names_one_account_and_inherits_nothing() {
        let sid = "S-1-5-21-1111111111-2222222222-3333333333-1001";
        assert_eq!(sddl_for(sid).unwrap(), "D:P(A;;GA;;;S-1-5-21-1111111111-2222222222-3333333333-1001)");
        // Anything that is not a plain SID could widen the list.
        for bad in ["", "S-1-", "WD", "S-1-5-21)(A;;GA;;;WD", "S-1-5-21-1-", "S-1-5--21", "AU", "S-1-1-0 "] {
            assert!(sddl_for(bad).is_err(), "{bad:?} was accepted");
        }
        // Everyone (S-1-1-0) is a plain SID, but never one a process runs as; the server only
        // ever passes its own account's.
        assert!(Descriptor::from_sddl(&sddl_for(sid).unwrap()).is_ok());
    }

    #[test]
    fn this_process_is_its_own_account() {
        let me = current_user_sid().unwrap();
        assert!(is_plain_sid(&me), "{me}");
        assert_eq!(process_user_sid(std::process::id()).unwrap(), me);
        // A process that does not exist has no account.
        assert!(process_user_sid(u32::MAX - 2).is_err());
    }
}
