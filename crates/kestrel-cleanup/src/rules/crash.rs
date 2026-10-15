//! Crash dumps and error reports: `%LOCALAPPDATA%\CrashDumps`, `.dmp` files anywhere in the
//! profile, and Windows Error Reporting's per-user archive and queue.

use kestrel_index::Index;

use crate::locations::join;
use crate::tree::{find_dir, item, item_of, newest, with_extensions};
use crate::{Claims, Context, Guard, Recommendation, Rule, Tier, fold_path, within};

/// A dump written in the last ten minutes may still be being written.
const FRESH: i64 = 10 * 60 * 10_000_000;

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
            if within(&fold_path(&path), &wer_folded) || !guard.allows(index, entry) {
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

    #[test]
    fn dumps_and_error_reports_are_found_where_windows_keeps_them() {
        let sandbox = Sandbox::new("crash");
        let local = r"Users\me\AppData\Local";
        sandbox.write(&format!(r"{local}\CrashDumps\game.exe.1234.dmp"), 20_000, 1);
        sandbox.write(&format!(r"{local}\Programs\tool\tool.dmp"), 20_000, 2);
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
