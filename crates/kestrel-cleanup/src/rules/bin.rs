//! The Recycle Bin's size, as information, with a way to open it. Kestrel never empties it.

use crate::{Context, Link, Recommendation, Rule, Tier};

/// What opening the Recycle Bin navigates to.
pub const RECYCLE_BIN: &str = "shell:RecycleBinFolder";

pub fn recommend(context: &Context) -> Vec<Recommendation> {
    let Some(bin) = context.recycle_bin.filter(|bin| bin.items > 0) else { return Vec::new() };
    let things = if bin.items == 1 { "1 item".to_owned() } else { format!("{} items", bin.items) };
    let mut rec = Recommendation::new(
        "recycle-bin",
        Rule::RecycleBin,
        Tier::Info,
        "Recycle Bin",
        format!("{things} you deleted are still in the Recycle Bin, where they can be restored. Emptying it frees their space for good."),
    )
    .with_advice("Open the Recycle Bin to restore anything you still want, then empty it there.");
    rec.bytes = bin.bytes;
    rec.links.push(Link { label: "Open the Recycle Bin".into(), target: RECYCLE_BIN.into() });
    vec![rec]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BinContents;
    use crate::testing::Sandbox;

    #[test]
    fn the_recycle_bin_is_shown_with_a_way_to_open_it() {
        let sandbox = Sandbox::new("bin");
        let mut context = sandbox.context();
        assert!(recommend(&context).is_empty(), "nothing known, nothing shown");
        context.recycle_bin = Some(BinContents { bytes: 0, items: 0 });
        assert!(recommend(&context).is_empty(), "an empty bin isn't worth a word");
        context.recycle_bin = Some(BinContents { bytes: 5 << 30, items: 12 });
        let recs = crate::scan(&sandbox.index(), &context);
        let bin = recs.iter().find(|r| r.id == "recycle-bin").unwrap();
        assert_eq!((bin.tier, bin.bytes, bin.items.len()), (Tier::Info, 5 << 30, 0));
        assert!(bin.reason.starts_with("12 items"));
        assert_eq!(bin.links, [Link { label: "Open the Recycle Bin".into(), target: "shell:RecycleBinFolder".into() }]);
    }
}
