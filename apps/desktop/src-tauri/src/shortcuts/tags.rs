//! Commands for tags, and the resolution of `tag:` searches.
//!
//! The front end shows tags by path, so [`tags_state`] works out where every tagged item is
//! now: by its file id in the index first, then at its last known path, then renamed within
//! its folder. Whatever moved is recorded at its new path, so the fallback stays useful.

use std::sync::Arc;

use kestrel_db::Db;
use kestrel_db::shortcuts::tags::{self, Item, Tag, Target};
use kestrel_index::{EntryId, Index};
use kestrel_query::{ParseError, Query, resolve_tags};
use serde::Serialize;
use tauri::State;

use super::{Data, entries_by_id, exists, identity, is_network};
use crate::engine::Engine;

/// Most items one tagging takes at once.
const MAX_AT_ONCE: usize = 10_000;

#[derive(Serialize)]
pub struct Tagged {
    path: String,
    tags: Vec<i64>,
}

#[derive(Serialize)]
pub struct TagsState {
    tags: Vec<Tag>,
    /// Every tagged item that could be found, at its current path.
    items: Vec<Tagged>,
}

/// Where a tagged item is now, and whether its record needs bringing up to date.
fn current(index: &Index, item: &Item) -> (Vec<String>, Option<(String, u64)>) {
    let by_id: Vec<String> = entries_by_id(index, &item.volume, item.file_id).into_iter().map(|id| index.path(id)).collect();
    if let Some(first) = by_id.first() {
        let moved = !by_id.iter().any(|path| *path == item.path);
        return (by_id.clone(), moved.then(|| (first.clone(), item.file_id)));
    }
    if is_network(&item.path) {
        // Asking a server can stall for most of a minute, and this runs on every refresh: a
        // network item is taken to be where it was last seen.
        return (vec![item.path.clone()], None);
    }
    if exists(&item.path) {
        // Outside the index, or saved by replacing the file, which gives it a new id.
        let (_, file_id) = identity(&item.path);
        let renewed = (file_id != 0 && file_id != item.file_id).then(|| (item.path.clone(), file_id));
        return (vec![item.path.clone()], renewed);
    }
    match super::locate(index, &item.path, &item.volume, item.file_id) {
        Some(path) => (vec![path.clone()], Some((path, item.file_id))),
        None => (Vec::new(), None),
    }
}

/// Every tag, and every tagged item that can be found, at its current path.
#[tauri::command(async)]
pub fn tags_state(db: Data, engine: State<'_, Arc<Engine>>) -> Result<TagsState, String> {
    let all = tags::items(&db)?;
    let mut items = Vec::with_capacity(all.len());
    let mut moves = Vec::new();
    {
        let index = engine.index().read();
        for item in &all {
            let (paths, moved) = current(&index, item);
            if let Some(moved) = moved {
                moves.push((item.id, moved));
            }
            items.extend(paths.into_iter().map(|path| Tagged { path, tags: item.tags.clone() }));
        }
    }
    for (id, (path, file_id)) in moves {
        tags::relocate(&db, id, &path, file_id)?;
    }
    Ok(TagsState { tags: tags::list(&db)?, items })
}

#[tauri::command(async)]
pub fn tags_create(db: Data, name: String, color: String) -> Result<Tag, String> {
    Ok(tags::create(&db, &name, &color)?)
}

/// Renames a tag, recolours it, or both.
#[tauri::command(async)]
pub fn tags_update(db: Data, id: i64, name: Option<String>, color: Option<String>) -> Result<(), String> {
    if let Some(name) = name {
        tags::rename(&db, id, &name)?;
    }
    if let Some(color) = color {
        tags::set_color(&db, id, &color)?;
    }
    Ok(())
}

/// Deletes a tag, taking it off everything. No file is touched.
#[tauri::command(async)]
pub fn tags_delete(db: Data, id: i64) -> Result<(), String> {
    Ok(tags::delete(&db, id)?)
}

/// Puts tag `tag` on every one of `paths`, or with `on` false, takes it off them.
#[tauri::command(async)]
pub fn tags_assign(db: Data, paths: Vec<String>, tag: i64, on: bool) -> Result<(), String> {
    if paths.len() > MAX_AT_ONCE {
        return Err(format!("Tag at most {MAX_AT_ONCE} items at a time."));
    }
    for path in &paths {
        let path = crate::engine::normalize(path);
        let (volume, file_id) = identity(&path);
        let target = Target { path: &path, volume: &volume, file_id };
        if on {
            tags::assign(&db, tag, &target)?;
        } else {
            tags::unassign(&db, tag, &target)?;
        }
    }
    Ok(())
}

/// The index entries an item is: by file id, or failing that, at its last known path.
fn entries_of(index: &Index, item: &Item) -> Vec<EntryId> {
    let by_id = entries_by_id(index, &item.volume, item.file_id);
    if !by_id.is_empty() {
        return by_id;
    }
    index.lookup(&item.path).into_iter().collect()
}

/// Replaces each `tag:` clause of a search with the entries carrying that tag.
pub fn resolve(db: &Db, query: &mut Query, index: &Index) -> Result<(), String> {
    resolve_tags(query, |name| {
        let failed = |error: kestrel_db::Error| ParseError(error.to_string());
        let tag = tags::by_name(db, name).map_err(failed)?.ok_or_else(|| ParseError(format!("No tag is named \"{name}\"")))?;
        let items = tags::items_with(db, tag.id).map_err(failed)?;
        Ok(items.iter().flat_map(|item| entries_of(index, item)).collect())
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_index::{IndexBuilder, walk};
    use std::fs;

    #[test]
    fn tags_survive_a_rename_and_are_found_by_tag_searches() {
        let root = std::env::temp_dir().join(format!("kestrel-shortcuts-tags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("drafts")).unwrap();
        fs::create_dir_all(root.join("final")).unwrap();
        fs::write(root.join("drafts").join("report.txt"), b"r").unwrap();
        fs::write(root.join("drafts").join("other.txt"), b"o").unwrap();
        let text = |path: std::path::PathBuf| path.to_str().unwrap().to_owned();
        let report = text(root.join("drafts").join("report.txt"));

        let db = Db::open_in_memory().unwrap();
        let urgent = tags::create(&db, "Urgent", "red").unwrap();
        let (volume, file_id) = identity(&report);
        tags::assign(&db, urgent.id, &Target { path: &report, volume: &volume, file_id }).unwrap();

        // The file is renamed and moved to another folder, then the index catches up.
        let moved = text(root.join("final").join("report v2.txt"));
        fs::rename(&report, &moved).unwrap();
        let mut builder = IndexBuilder::new();
        walk::walk(root.to_str().unwrap(), &mut builder).unwrap();
        let index = builder.finish();

        // The tag followed it: shown at the new path, and the record learns where it went.
        let item = &tags::items(&db).unwrap()[0];
        let (paths, relocation) = current(&index, item);
        assert_eq!(paths, [moved.clone()]);
        assert_eq!(relocation, Some((moved.clone(), file_id)));
        tags::relocate(&db, item.id, &moved, file_id).unwrap();
        assert_eq!(current(&index, &tags::items(&db).unwrap()[0]).1, None, "nothing more to update");

        // A tag: search finds it, alone or with other clauses, and an unknown tag is an error.
        let found = |text: &str| -> Result<Vec<String>, String> {
            let mut query = kestrel_query::parse(text, kestrel_query::Clock::local()).map_err(|e| e.to_string())?;
            resolve(&db, &mut query, &index)?;
            let compiled = kestrel_query::compile(&query, &index).map_err(|e| e.to_string())?;
            let ids = kestrel_query::search(&index, &compiled, &std::sync::atomic::AtomicBool::new(false)).unwrap();
            Ok(ids.into_iter().map(|id| index.path(id)).collect())
        };
        assert_eq!(found("tag:urgent").unwrap(), [moved.clone()]);
        assert_eq!(found("tag:Urgent ext:txt v2").unwrap(), [moved.clone()]);
        assert!(found("tag:urgent other").unwrap().is_empty());
        assert!(found("tag:nope").unwrap_err().contains("No tag is named"));

        // Taken off again, the item is forgotten.
        let (volume, file_id) = identity(&moved);
        tags::unassign(&db, urgent.id, &Target { path: &moved, volume: &volume, file_id }).unwrap();
        assert!(found("tag:urgent").unwrap().is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
