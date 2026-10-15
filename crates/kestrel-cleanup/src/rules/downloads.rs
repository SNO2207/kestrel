//! Old installers and archives in Downloads: programs' setup files and archives downloaded 90
//! days or more ago and untouched since.

use kestrel_index::Index;

use crate::tree::{find_dir, item_of, with_extensions};
use crate::{Claims, Context, DAY, Guard, Recommendation, Rule, Tier};

pub const AGE_DAYS: i64 = 90;
const EXTENSIONS: &[&str] = &["exe", "msi", "msix", "msixbundle", "zip", "7z", "iso"];

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let Some(downloads) = find_dir(index, &context.locations.downloads) else { return Vec::new() };
    let cutoff = context.now - AGE_DAYS * DAY;
    let mut items = Vec::new();
    for entry in with_extensions(index, EXTENSIONS) {
        let mtime = index.mtime(entry);
        if mtime > 0 && mtime <= cutoff && index.is_under(entry, downloads) && !claims.covers(index, entry) && guard.allows(index, entry) {
            claims.claim(entry);
            items.push(item_of(index, entry));
        }
    }
    if items.is_empty() {
        return Vec::new();
    }
    vec![
        Recommendation::new(
            "downloads",
            Rule::Downloads,
            Tier::Review,
            "Old installers and archives in Downloads",
            "Installers and archives you downloaded 90 days or more ago and haven't changed since. Once a program is \
             installed or an archive unpacked, the download is usually not needed.",
        )
        .with_items(items),
    ]
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Tier, scan};

    #[test]
    fn installers_and_archives_untouched_for_90_days_are_offered_for_review() {
        let sandbox = Sandbox::new("downloads");
        let downloads = r"Users\me\Downloads";
        for (name, days) in [
            ("setup.exe", 100),
            ("tool.msi", 400),
            ("app.msix", 91),
            ("photos.zip", 200),
            (r"old\backup.7z", 120),
            ("ubuntu.iso", 365),
            ("recent.zip", 10),
            ("report.pdf", 500),
        ] {
            sandbox.write(&format!(r"{downloads}\{name}"), 9000, days as u8);
            sandbox.age(&format!(r"{downloads}\{name}"), days);
        }
        // Elsewhere is not Downloads.
        sandbox.write(r"Users\me\Desktop\setup.exe", 9000, 1);
        sandbox.age(r"Users\me\Desktop\setup.exe", 400);
        let recs = scan(&sandbox.index(), &sandbox.context());
        let rec = recs.iter().find(|r| r.id == "downloads").unwrap();
        assert_eq!(rec.tier, Tier::Review);
        assert_eq!(
            item_paths(&sandbox, &recs),
            [
                "users/me/downloads/app.msix",
                "users/me/downloads/old/backup.7z",
                "users/me/downloads/photos.zip",
                "users/me/downloads/setup.exe",
                "users/me/downloads/tool.msi",
                "users/me/downloads/ubuntu.iso",
            ]
        );
    }
}
