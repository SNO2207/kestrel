//! Crash dumps and error reports: `%LOCALAPPDATA%\CrashDumps`, crash dumps anywhere in the
//! profile, and Windows Error Reporting's per-user archive and queue.
//!
//! Outside the folders Windows writes dumps to, `.dmp` says little: database exports (Oracle's
//! Data Pump and `exp`, `pg_dump`) and other dumps kept on purpose share it. There a file counts
//! as a crash dump only if it starts as one does: a minidump's `MDMP` (which WER's and
//! ProcDump's full dumps are too) or a kernel dump's `PAGE`.

use kestrel_index::Index;

use crate::locations::join;
use crate::tree::{find_dir, item, item_of, newest, with_extensions};
use crate::{Claims, Context, Guard, Recommendation, Rule, Tier, fold_path, within};

/// A dump written in the last ten minutes may still be being written.
const FRESH: i64 = 10 * 60 * 10_000_000;

/// How crash dumps begin: a minidump (user-mode dumps, full ones included) and a kernel dump.
const SIGNATURES: [&[u8; 4]; 2] = [b"MDMP", b"PAGE"];

/// Whether the file at `path` begins as a crash dump does. Only its first bytes are read, from
/// the file itself (never what a link leads to), and nothing is read from a cloud file.
#[cfg(windows)]
fn is_crash_dump(path: &str) -> bool {
    use std::io::Read;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let Ok(mut file) = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path) else {
        return false;
    };
    let cloud_or_link = crate::tree::PLACEHOLDER | kestrel_index::attr::REPARSE_POINT;
    if file.metadata().is_ok_and(|meta| meta.file_attributes() & cloud_or_link != 0) {
        return false;
    }
    let mut head = [0u8; 4];
    file.read_exact(&mut head).is_ok() && SIGNATURES.iter().any(|signature| head == **signature)
}

#[cfg(not(windows))]
fn is_crash_dump(_path: &str) -> bool {
    false
}

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let places = &context.locations;
    let settled = context.now - FRESH;
    let wer = join(&places.local, r"Microsoft\Windows\WER");
    let mut recs = Vec::new();

    let mut dumps = Vec::new();
    if let Some(folder) = find_dir(index, &join(&places.local, "CrashDumps")) {
        for entry in index.children(folder).collect::<Vec<_>>() {
            let changed = newest(index, entry);
            if changed <= settled && !claims.covers(index, entry) && guard.allows(index, entry) {
                claims.claim(entry);
                dumps.push(item(index, entry, changed));
            }
        }
    }
    if let Some(profile) = find_dir(index, &places.profile) {
        let wer_folded = fold_path(&wer);
        for entry in with_extensions(index, &["dmp", "mdmp", "hdmp"]) {
            if !index.is_under(entry, profile) || index.mtime(entry) > settled || claims.covers(index, entry) {
                continue;
            }
            let path = index.path(entry);
            // Error reports' own dumps go with their reports, below.
            if within(&fold_path(&path), &wer_folded) || !guard.allows(index, entry) || !is_crash_dump(&path) {
                continue;
            }
            claims.claim(entry);
            dumps.push(item_of(index, entry));
        }
    }
    if !dumps.is_empty() {
        recs.push(
            Recommendation::new(
                "crash-dumps",
                Rule::Crash,
                Tier::Safe,
                "Crash dumps",
                "Memory dumps written when programs crashed. They help a program's developers find out why; you only need \
                 one if someone asked you to send it.",
            )
            .with_items(dumps),
        );
    }

    let mut reports = Vec::new();
    for queue in ["ReportArchive", "ReportQueue"] {
        let Some(folder) = find_dir(index, &join(&wer, queue)) else { continue };
        for entry in index.children(folder).collect::<Vec<_>>() {
            let changed = newest(index, entry);
            if changed <= settled && !claims.covers(index, entry) && guard.allows(index, entry) {
                claims.claim(entry);
                reports.push(item(index, entry, changed));
            }
        }
    }
    if !reports.is_empty() {
        recs.push(
            Recommendation::new(
                "error-reports",
                Rule::Crash,
                Tier::Safe,
                "Windows error reports",
                "Reports Windows kept after programs crashed or stopped responding: those already sent, kept as a record, and \
                 those still waiting to be sent.",
            )
            .with_items(reports),
        );
    }
    recs
}

#[cfg(test)]
mod tests {
    use crate::scan;
    use crate::testing::{Sandbox, item_paths};

    /// `len` bytes beginning with `head`.
    fn starting_with(head: &[u8], len: usize) -> Vec<u8> {
        let mut bytes = head.to_vec();
        bytes.resize(len, 7);
        bytes
    }

    /// Outside Windows' own folders, only what begins as a crash dump is one: a database export
    /// a colleague sent, or a backup kept on purpose, is not offered however it is named.
    #[test]
    fn files_named_dmp_that_are_not_crash_dumps_are_left_alone() {
        let sandbox = Sandbox::new("crash-exports");
        sandbox.write_bytes(r"Users\me\Downloads\app.dmp", &starting_with(b"MDMP\x93\xa7", 20_000));
        sandbox.write_bytes(r"Users\me\Downloads\prod_export.dmp", &starting_with(b"\x03\x06\x00EXPORT:V19.00.00", 20_000));
        sandbox.write_bytes(r"Users\me\backups\db.dmp", &starting_with(b"PGDMP\x01\x0e\x00", 20_000));
        sandbox.write_bytes(r"Users\me\backups\kernel.dmp", &starting_with(b"PAGEDU64", 20_000));
        sandbox.age_tree("Users", 2);
        let recs = scan(&sandbox.index(), &sandbox.context());
        let dumps = recs.iter().find(|r| r.id == "crash-dumps").unwrap();
        assert_eq!(
            item_paths(&sandbox, std::slice::from_ref(dumps)),
            ["users/me/backups/kernel.dmp", "users/me/downloads/app.dmp"]
        );
    }

    #[test]
    fn dumps_and_error_reports_are_found_where_windows_keeps_them() {
        let sandbox = Sandbox::new("crash");
        let local = r"Users\me\AppData\Local";
        sandbox.write(&format!(r"{local}\CrashDumps\game.exe.1234.dmp"), 20_000, 1);
        sandbox.write_bytes(&format!(r"{local}\Programs\tool\tool.dmp"), &starting_with(b"MDMP", 20_000));
        sandbox.write(&format!(r"{local}\Microsoft\Windows\WER\ReportArchive\AppCrash_game_1\Report.wer"), 9000, 3);
        sandbox.write(&format!(r"{local}\Microsoft\Windows\WER\ReportArchive\AppCrash_game_1\memory.hdmp"), 9000, 4);
        sandbox.write(&format!(r"{local}\Microsoft\Windows\WER\ReportQueue\AppHang_x_2\Report.wer"), 9000, 5);
        // Never: a dump the owner keeps in Documents, one in OneDrive, and one being written.
        sandbox.write(r"Users\me\Documents\keep.dmp", 20_000, 6);
        sandbox.write(r"Users\me\OneDrive\synced.dmp", 20_000, 7);
        sandbox.write(&format!(r"{local}\CrashDumps\writing.dmp"), 20_000, 8);
        sandbox.age_tree("Users", 2);
        sandbox.age(&format!(r"{local}\CrashDumps\writing.dmp"), 0);

        let recs = scan(&sandbox.index(), &sandbox.context());
        let dumps = recs.iter().find(|r| r.id == "crash-dumps").unwrap();
        assert_eq!(
            item_paths(&sandbox, std::slice::from_ref(dumps)),
            ["users/me/appdata/local/crashdumps/game.exe.1234.dmp", "users/me/appdata/local/programs/tool/tool.dmp"]
        );
        let reports = recs.iter().find(|r| r.id == "error-reports").unwrap();
        assert_eq!(
            item_paths(&sandbox, std::slice::from_ref(reports)),
            [
                "users/me/appdata/local/microsoft/windows/wer/reportarchive/appcrash_game_1",
                "users/me/appdata/local/microsoft/windows/wer/reportqueue/apphang_x_2",
            ]
        );
        let all = item_paths(&sandbox, &recs);
        assert!(!all.iter().any(|p| p.contains("documents") || p.contains("onedrive") || p.contains("writing")), "{all:?}");
    }
}
