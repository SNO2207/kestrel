//! The rules, each in a module of its own, run in the order of [`ALL`]: earlier rules claim
//! what they recommend, so a file in a cache inside the temp folder is counted once.

mod apps;
mod bin;
mod build;
mod caches;
mod crash;
mod disks;
mod downloads;
mod large;
mod temp;
mod windows;

use kestrel_index::Index;

use crate::{Claims, Context, Guard, Recommendation, Rule};

pub const ALL: &[Rule] = &[
    Rule::Temp,
    Rule::Crash,
    Rule::Build,
    Rule::PackageCache,
    Rule::AppCache,
    Rule::Downloads,
    Rule::LargeOld,
    Rule::DiskImages,
    Rule::RecycleBin,
    Rule::Windows,
];

pub(crate) fn run(rule: Rule, index: &Index, context: &Context, guard: &Guard, claims: &mut Claims) -> Vec<Recommendation> {
    match rule {
        Rule::Temp => temp::recommend(index, context, guard, claims),
        Rule::Crash => crash::recommend(index, context, guard, claims),
        Rule::Build => build::recommend(index, context, guard, claims),
        Rule::PackageCache => caches::recommend(index, context, guard, claims),
        Rule::AppCache => apps::recommend(index, context, guard, claims),
        Rule::Downloads => downloads::recommend(index, context, guard, claims),
        Rule::LargeOld => large::recommend(index, context, guard, claims),
        Rule::DiskImages => disks::recommend(index, context, guard, claims),
        Rule::RecycleBin => bin::recommend(context),
        Rule::Windows => windows::recommend(index, context),
    }
}

/// "1 day", "12 days".
pub(crate) fn days(n: i64) -> String {
    if n == 1 { "1 day".into() } else { format!("{n} days") }
}

/// The last component of a path.
pub(crate) fn file_name(path: &str) -> &str {
    path.trim_end_matches('\\').rsplit('\\').next().unwrap_or(path)
}
