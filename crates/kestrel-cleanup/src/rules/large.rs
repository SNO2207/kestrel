//! Large old files: over 1 GB (`Context::large_file_bytes`) and untouched for a year, anywhere
//! the guard allows, that no other rule covers. Disk images are left to their own rule, the
//! system's own files (the page file, hibernation) are never counted, and nor is anything in
//! AppData: programs keep their own data there (a browser's on-device model, a game's assets),
//! which only they should remove; the cache rules cover what is safe to.

use kestrel_index::{EntryId, Index, attr};
use rayon::prelude::*;

use super::disks::EXTENSIONS as DISK_IMAGES;
use crate::locations::join;
use crate::tree::{find_dir, item_of};
use crate::{Claims, Context, DAY, Guard, Recommendation, Rule, Tier};

pub const AGE_DAYS: i64 = 365;

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let cutoff = context.now - AGE_DAYS * DAY;
    let disk_images: Vec<u16> = DISK_IMAGES.iter().filter_map(|ext| index.ext_id(ext)).collect();
    let min = context.large_file_bytes;
    let found: Vec<EntryId> = (0..index.len())
        .into_par_iter()
        .with_min_len(1 << 14)
        .filter(|&id| {
            let attrs = index.attrs(id);
            let mtime = index.mtime(id);
            attrs & (attr::DIRECTORY | attr::DELETED | attr::SYSTEM) == 0
                && index.size(id) > min
                && mtime > 0
                && mtime <= cutoff
                && !disk_images.contains(&index.ext(id))
        })
        .collect();
    let app_data = find_dir(index, &join(&context.locations.profile, "AppData"));
    let mut items = Vec::new();
    for entry in found {
        if app_data.is_some_and(|folder| index.is_under(entry, folder)) {
            continue;
        }
        if !claims.covers(index, entry) && guard.allows(index, entry) {
            claims.claim(entry);
            items.push(item_of(index, entry));
        }
    }
    if items.is_empty() {
        return Vec::new();
    }
    vec![
        Recommendation::new(
            "large-old",
            Rule::LargeOld,
            Tier::Review,
            "Large files untouched for a year",
            "Big files nothing has changed in a year or more. Check whether you still need them; some may be worth \
             moving to another drive instead.",
        )
        .with_items(items),
    ]
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Project, Tier, scan};

    #[test]
    fn large_files_untouched_for_a_year_are_offered_for_review_outside_protected_places() {
        let sandbox = Sandbox::new("large");
        // The threshold is 1 GB on a real drive; here 64 KB stands in for it.
        for (rel, len, days) in [
            (r"Users\me\Videos-old\film.mkv", 100_000, 400),
            (r"Users\me\backups\disk.bak", 100_000, 800),
            (r"Users\me\backups\recent.bak", 100_000, 100),
            (r"Users\me\backups\small.bak", 10_000, 800),
            (r"Users\me\Videos\holiday.mp4", 100_000, 900),
            (r"Users\me\Documents\thesis-data.csv", 100_000, 900),
            (r"Users\me\OneDrive\archive.zip", 100_000, 900),
            (r"Program Files\Game\data.pak", 100_000, 900),
            (r"Users\me\code\app\assets\big.bin", 100_000, 900),
            (r"Users\me\vms\old.vhdx", 100_000, 900),
            (r"Users\me\AppData\Local\Browser\User Data\Model\weights.bin", 100_000, 900),
        ] {
            sandbox.write(rel, len, days as u8);
            sandbox.age(rel, days);
        }
        let mut context = sandbox.context();
        context.large_file_bytes = 64 * 1024;
        context.projects = vec![Project { root: sandbox.text(r"Users\me\code\app"), dismissed: false }];
        let recs = scan(&sandbox.index(), &context);
        let large = recs.iter().find(|r| r.id == "large-old").unwrap();
        assert_eq!(large.tier, Tier::Review);
        assert_eq!(
            item_paths(&sandbox, std::slice::from_ref(large)),
            ["users/me/backups/disk.bak", "users/me/videos-old/film.mkv"]
        );
    }

    /// An index read from the drive's file table holds the whole drive: what is already in the
    /// Recycle Bin (which keeps its dates), a previous installation, and other accounts' files
    /// are never offered.
    #[test]
    fn the_recycle_bin_old_installations_and_other_accounts_are_left_alone() {
        let sandbox = Sandbox::new("large-elsewhere");
        for rel in [
            r"Users\me\backups\disk.bak",
            r"$Recycle.Bin\S-1-5-21-1-2-3-1001\$RAB12CD.mp4",
            r"Windows.old\Users\me\Videos\old.mkv",
            r"$Windows.~BT\Sources\install.esd",
            r"Users\mom\Videos\big.mp4",
            r"Users\Public\Videos\shared.mp4",
            r"Data\$RECYCLE.BIN\S-1-5-21-1-2-3-1001\$RXY.zip",
        ] {
            sandbox.write(rel, 100_000, 3);
            sandbox.age(rel, 800);
        }
        let mut context = sandbox.context();
        context.large_file_bytes = 64 * 1024;
        let recs = scan(&sandbox.index(), &context);
        let large = recs.iter().find(|r| r.id == "large-old").unwrap();
        assert_eq!(item_paths(&sandbox, std::slice::from_ref(large)), ["users/me/backups/disk.bak"]);
    }
}
