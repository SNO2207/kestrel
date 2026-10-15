//! The broker's named pipe: who may connect, and what it does for them.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::windows::io::FromRawHandle;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use kestrel_index::broker::{self, FAILED, OK, PIPE};
use kestrel_index::live::Tracker;
use kestrel_index::{Index, IndexBuilder, mft, snapshot, walk};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// The string SID of the account this process runs as.
pub fn current_user_sid() -> Result<String> {
    // SAFETY: the token handle is closed before returning; the buffer is sized by the first
    // call and the SID string is freed with LocalFree as documented.
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            bail!("could not open the process token: {}", io::Error::last_os_error());
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), needed, &mut needed);
        CloseHandle(token);
        if ok == 0 {
            bail!("could not read the process token: {}", io::Error::last_os_error());
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = null_mut::<u16>();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            bail!("could not format the account SID");
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        Ok(sid)
    }
}

/// A security descriptor that lets SYSTEM, Administrators and one account use the pipe, and
/// nobody else.
struct Security(PSECURITY_DESCRIPTOR);

impl Security {
    fn for_account(sid: &str) -> Result<Self> {
        // Reject anything but a plain SID, so the account cannot widen the access list.
        if !sid.starts_with("S-1-") || !sid[4..].chars().all(|c| c.is_ascii_digit() || c == '-') {
            bail!("{sid} is not an account SID");
        }
        let sddl = wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{sid})"));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `sddl` is NUL-terminated; the descriptor is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, null_mut())
        };
        if ok == 0 {
            bail!("could not build the pipe's access list: {}", io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// Unblocks a `serve` loop waiting for a client, so it can notice it should stop.
pub fn wake() {
    let _ = std::fs::OpenOptions::new().read(true).write(true).open(PIPE);
}

/// Serves clients one at a time until `stopping` is set.
pub fn serve(allowed: &str, stopping: &AtomicBool) -> Result<()> {
    let security = Security::for_account(allowed)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    };
    let name = wide(PIPE);
    while !stopping.load(Ordering::SeqCst) {
        // SAFETY: `name` and `attributes` outlive the call; the handle is owned below.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                1 << 20,
                64 * 1024,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            bail!("could not create {PIPE}: {}", io::Error::last_os_error());
        }
        // SAFETY: `pipe` is a valid server end; a client that connected before this call is
        // reported as ERROR_PIPE_CONNECTED, which is success too.
        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED };
        if !connected || stopping.load(Ordering::SeqCst) {
            // SAFETY: closing the handle created above.
            unsafe { CloseHandle(pipe) };
            continue;
        }
        // SAFETY: ownership of the handle moves to the File, which closes it.
        let mut client = unsafe { File::from_raw_handle(pipe as _) };
        if let Err(error) = answer(&mut client) {
            eprintln!("client failed: {error}");
        }
        // Wait until the client has read everything before disconnecting it.
        // SAFETY: the handle is still open; the File closes it when dropped below.
        unsafe {
            FlushFileBuffers(pipe);
            DisconnectNamedPipe(pipe);
        }
        drop(client);
    }
    Ok(())
}

fn answer(client: &mut File) -> io::Result<()> {
    let Some(roots) = broker::read_request(client)? else {
        client.write_all(&[FAILED])?;
        return client.write_all(b"unrecognised request");
    };
    let mut out = BufWriter::with_capacity(1 << 20, client);
    match build(&roots) {
        Ok(index) => {
            out.write_all(&[OK])?;
            snapshot::write_to(&index, &mut out)?;
        }
        Err(error) => {
            out.write_all(&[FAILED])?;
            out.write_all(error.to_string().as_bytes())?;
        }
    }
    out.flush()
}

/// Builds an index of the roots: from the MFT for whole NTFS drives, by crawling otherwise.
/// Journal positions are taken first so the app can replay changes made meanwhile.
fn build(roots: &[String]) -> io::Result<Index> {
    let cursors: Vec<_> = roots.iter().filter_map(|root| Tracker::open(root).and_then(|t| t.position()).ok()).collect();
    let mut builder = IndexBuilder::new();
    for root in roots {
        // Whole NTFS drives are read from their file table. A failure there happens before
        // anything is added (the drive cannot be opened, or is not NTFS), so crawling the same
        // root afterwards does not duplicate it.
        let is_drive = root.trim_end_matches(['\\', '/']).ends_with(':');
        let from_mft = is_drive && mft::scan_volume(root, &mut builder).is_ok();
        if !from_mft {
            walk::walk(root, &mut builder)?;
        }
    }
    let mut index = builder.finish();
    index.set_journal(cursors);
    Ok(index)
}
