//! Temporary files: what lies in the owner's temp folder untouched for a week or more.

use kestrel_index::Index;

use crate::tree::{find_dir, item, newest};
use crate::{Claims, Context, DAY, Guard, Recommendation, Rule, Tier};

/// Days without a change before a temporary file is offered.
pub const AGE_DAYS: i64 = 7;

pub fn recommend(index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    let Some(temp) = find_dir(index, &context.locations.temp) else { return Vec::new() };
    let cutoff = context.now - AGE_DAYS * DAY;
    let mut items = Vec::new();
    for entry in index.children(temp).collect::<Vec<_>>() {
        if claims.covers(index, entry) || !guard.allows(index, entry) {
            continue;
        }
        // A folder counts as touched when anything inside it is.
        let changed = newest(index, entry);
        if changed <= 0 || changed > cutoff {
            continue;
        }
        claims.claim(entry);
        items.push(item(index, entry, changed));
    }
    if items.is_empty() {
        return Vec::new();
    }
    vec![
        Recommendation::new(
            "temp",
            Rule::Temp,
            Tier::Safe,
            "Temporary files",
            "Files programs left in your temp folder that nothing has changed for 7 days or more. Programs make these \
             while they run and make them again when they need them.",
        )
        .with_items(items)
        .with_note("Anything a program still has open is skipped."),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan;
    use crate::testing::{Sandbox, item_paths};

    #[test]
    fn temp_contents_untouched_for_a_week_are_offered() {
        let sandbox = Sandbox::new("temp");
        let temp = r"Users\me\AppData\Local\Temp";
        sandbox.write(&format!(r"{temp}\old.tmp"), 9000, 1);
        sandbox.write(&format!(r"{temp}\fresh.tmp"), 9000, 2);
        sandbox.write(&format!(r"{temp}\old-folder\a.log"), 9000, 3);
        sandbox.write(&format!(r"{temp}\busy-folder\old.log"), 9000, 4);
        sandbox.write(&format!(r"{temp}\busy-folder\new.log"), 9000, 5);
        sandbox.age(&format!(r"{temp}\old.tmp"), 8);
        sandbox.age_tree(&format!(r"{temp}\old-folder"), 30);
        sandbox.age(&format!(r"{temp}\busy-folder\old.log"), 30);
        sandbox.age(&format!(r"{temp}\busy-folder"), 30);
        // Six days is not yet a week.
        sandbox.write(&format!(r"{temp}\almost.tmp"), 9000, 6);
        sandbox.age(&format!(r"{temp}\almost.tmp"), 6);

        let recs = scan(&sandbox.index(), &sandbox.context());
        let temp_rec = recs.iter().find(|r| r.id == "temp").expect("a temp recommendation");
        assert_eq!(temp_rec.tier, Tier::Safe);
        assert!(temp_rec.note.as_deref().unwrap().contains("open"));
        let paths = item_paths(&sandbox, std::slice::from_ref(temp_rec));
        assert_eq!(paths, ["users/me/appdata/local/temp/old-folder", "users/me/appdata/local/temp/old.tmp"]);
        assert_eq!(temp_rec.bytes, temp_rec.items.iter().map(|i| i.bytes).sum::<u64>());
        assert!(temp_rec.bytes >= 2 * 9000);
        assert!(temp_rec.items.iter().all(|i| i.modified > 0));
    }

    #[test]
    fn kestrels_own_data_in_temp_is_never_offered() {
        let sandbox = Sandbox::new("temp-own");
        let temp = r"Users\me\AppData\Local\Temp";
        sandbox.write(&format!(r"{temp}\kestrel-slots\slot5\kestrel.db"), 9000, 1);
        sandbox.write(&format!(r"{temp}\other.tmp"), 9000, 2);
        sandbox.age_tree(temp, 30);
        let mut context = sandbox.context();
        context.protected = vec![sandbox.text(&format!(r"{temp}\kestrel-slots"))];
        let paths = item_paths(&sandbox, &scan(&sandbox.index(), &context));
        assert_eq!(paths, ["users/me/appdata/local/temp/other.tmp"]);
    }
}
