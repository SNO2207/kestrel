//! Windows' own cleanup: the update cache, Delivery Optimization's cache and previous Windows
//! installations, as information with a way to open Storage Sense or Disk Cleanup. Only Windows'
//! tools should remove these (some need an administrator), so Kestrel never does.

use kestrel_index::Index;

use crate::locations::join;
use crate::tree::find_dir;
use crate::{Context, Link, Part, Recommendation, Rule, Tier};

pub const STORAGE_SENSE: &str = "ms-settings:storagesense";
pub const DISK_CLEANUP: &str = "cleanmgr";

pub fn recommend(index: &Index, context: &Context) -> Vec<Recommendation> {
    let places = &context.locations;
    let candidates = [
        ("Windows Update downloads", join(&places.windows, r"SoftwareDistribution\Download")),
        (
            "Delivery Optimization cache",
            join(&places.windows, r"ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization\Cache"),
        ),
        ("Previous Windows installation", join(&places.system_drive, "Windows.old")),
        ("Windows upgrade files", join(&places.system_drive, "$Windows.~BT")),
        ("Windows upgrade files", join(&places.system_drive, "$Windows.~WS")),
    ];
    let mut parts: Vec<Part> = Vec::new();
    for (label, path) in candidates {
        let Some(folder) = find_dir(index, &path) else { continue };
        let bytes = index.alloc(folder);
        if bytes == 0 {
            continue;
        }
        match parts.iter_mut().find(|part| part.label == label) {
            Some(part) => part.bytes += bytes,
            None => parts.push(Part { label: label.into(), bytes }),
        }
    }
    let mut rec = Recommendation::new(
        "windows",
        Rule::Windows,
        Tier::Info,
        "Windows' own cleanup",
        "Windows keeps update downloads, a cache of updates it shares with other PCs and, after an upgrade, your previous \
         installation. Its own tools remove them safely.",
    )
    .with_advice(
        "Storage Sense and Disk Cleanup remove these. In Disk Cleanup, \"Clean up system files\" reaches the rest and asks \
         for an administrator.",
    )
    .with_note("Kestrel can't see inside some of these folders, so there may be more.");
    rec.bytes = parts.iter().map(|part| part.bytes).sum();
    rec.parts = parts;
    rec.links = vec![
        Link { label: "Open Storage Sense".into(), target: STORAGE_SENSE.into() },
        Link { label: "Open Disk Cleanup".into(), target: DISK_CLEANUP.into() },
    ];
    vec![rec]
}

#[cfg(test)]
mod tests {
    use crate::testing::{Sandbox, item_paths};
    use crate::{Action, Tier, scan};

    #[test]
    fn windows_own_caches_are_counted_but_left_to_windows() {
        let sandbox = Sandbox::new("windows");
        sandbox.write(r"Windows\SoftwareDistribution\Download\abc\update.cab", 50_000, 1);
        sandbox.write(r"Windows.old\Windows\System32\kernel32.dll", 30_000, 2);
        sandbox.write(r"$Windows.~BT\Sources\setup.exe", 20_000, 3);
        let recs = scan(&sandbox.index(), &sandbox.context());
        let windows = recs.iter().find(|r| r.id == "windows").unwrap();
        assert_eq!((windows.tier, &windows.action), (Tier::Info, &Action::None));
        let labels: Vec<&str> = windows.parts.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["Windows Update downloads", "Previous Windows installation", "Windows upgrade files"]);
        assert_eq!(windows.bytes, windows.parts.iter().map(|p| p.bytes).sum::<u64>());
        assert!(windows.bytes >= 100_000);
        let targets: Vec<&str> = windows.links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, ["ms-settings:storagesense", "cleanmgr"]);
        // Nothing in Windows' folders is ever an item, whatever the rule.
        assert!(item_paths(&sandbox, &recs).is_empty());
    }
}
