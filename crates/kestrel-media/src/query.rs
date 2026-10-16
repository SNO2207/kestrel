//! The media clauses of a search (`width:`, `height:`, `taken:`, `camera:`, `duration:`,
//! `kind:photo`, `kind:video`, `geo:`, `text:`), answered from the library and turned into the
//! index entries they name, as `tag:` clauses are.

use kestrel_db::Db;
use kestrel_db::media::{self as store, Where};
use kestrel_index::{EntryId, Index, NO_ENTRY};
use kestrel_query::{MediaClause, MediaKind, ParseError, Query, resolve_media};

/// The volume an entry is on, from the name of its root (`C:` or a crawled folder's path).
fn volume_of_entry(index: &Index, id: EntryId) -> Option<String> {
    let mut at = id;
    loop {
        let parent = index.parent(at);
        if parent == NO_ENTRY {
            return crate::volume_of(index.name(at));
        }
        at = parent;
    }
}

/// The live entries that are these files, known by volume and file id.
pub fn entries(index: &Index, keys: &[(String, u64)]) -> Vec<EntryId> {
    let mut found = Vec::with_capacity(keys.len());
    for (volume, file_id) in keys {
        for entry in index.entries_with_file_id(*file_id) {
            if volume_of_entry(index, entry).as_deref() == Some(volume.as_str()) {
                found.push(entry);
            }
        }
    }
    found
}

/// The files a media clause names, as volume and file id.
pub fn keys(db: &Db, clause: &MediaClause) -> Result<Vec<(String, u64)>, String> {
    let condition = match clause {
        MediaClause::Text(words) => {
            let Some(query) = store::fts_query(words) else { return Ok(Vec::new()) };
            return Ok(store::find_text(db, &query)?);
        }
        &MediaClause::Width(min, max) => Where::Width(min, max),
        &MediaClause::Height(min, max) => Where::Height(min, max),
        &MediaClause::Taken(from, to) => Where::Taken(from, to),
        MediaClause::Camera(text) => Where::Camera(text.clone()),
        &MediaClause::Duration(min, max) => Where::Duration(min, max),
        MediaClause::Kind(MediaKind::Photo) => Where::Kind(store::PHOTO),
        MediaClause::Kind(MediaKind::Video) => Where::Kind(store::VIDEO),
        &MediaClause::Geo(known) => Where::Geo(known),
    };
    Ok(store::select(db, &condition)?)
}

/// Replaces each media clause of a search with the entries it names.
pub fn resolve(db: &Db, query: &mut Query, index: &Index) -> Result<(), String> {
    resolve_media(query, |clause| {
        let keys = keys(db, clause).map_err(ParseError)?;
        Ok(entries(index, &keys))
    })
    .map_err(|error| error.to_string())
}
