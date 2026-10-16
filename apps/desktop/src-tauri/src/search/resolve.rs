//! What the index can't answer itself, resolved to entries before it searches: tags (`tag:`),
//! the media library's facts (`width:`, `taken:`, `text:`...) and descriptions (`like:`, `~`),
//! which also rank the results best first.
//!
//! Every search goes through here, whichever way it comes: the search box (`search_run`), the
//! `search` command (the quick launcher and anything without the search box), the files a
//! search inside files reads, and the query service's `search` (devtools). A clause any of
//! them answered and another didn't would make the same query find different things.

use std::collections::HashMap;

use kestrel_db::Db;
use kestrel_index::{EntryId, Index};
use kestrel_query::Query;

use crate::engine::{Engine, Listing, SortSpec};
use crate::semantic::Semantic;
use crate::semantic::search::{Prepared, prepare};
use crate::visibility::Visibility;

/// One query's resolvers, made ready before the index is held: descriptions run the model then.
pub struct Resolver<'a> {
    db: &'a Db,
    described: Prepared,
}

impl<'a> Resolver<'a> {
    /// Runs the model for `text`'s descriptions, if it has any.
    pub fn prepare(db: &'a Db, semantic: &Semantic, text: &str) -> Result<Self, String> {
        Ok(Self { db, described: prepare(semantic, text)? })
    }

    /// A resolver without AI search's model: a description finds nothing (the query service in
    /// tests, which have no model).
    pub fn index_only(db: &'a Db) -> Self {
        Self { db, described: Prepared::default() }
    }

    /// Replaces every clause the index can't answer with the entries it stands for. Returns
    /// the descriptions' scores, for ranking, when the query has one.
    pub fn resolve(&mut self, query: &mut Query, index: &Index) -> Result<Option<HashMap<EntryId, f32>>, String> {
        crate::shortcuts::tags::resolve(self.db, query, index)?;
        kestrel_media::query::resolve(self.db, query, index)?;
        self.described.resolve(query, index)
    }

    /// Where in each video a description matched, by lower-case path.
    pub fn take_moments(&mut self) -> HashMap<String, u32> {
        self.described.take_moments()
    }
}

/// The index's search of `text`, every clause resolved; a description's results come best
/// first and their videos' moments are kept for the preview (`semantic_moments`).
pub fn index_search(
    engine: &Engine,
    db: &Db,
    semantic: &Semantic,
    text: &str,
    scope: Option<&str>,
    sort: SortSpec,
    visibility: Visibility,
) -> Result<Listing, String> {
    let mut resolver = Resolver::prepare(db, semantic, text)?;
    let listing = engine.search_ranked(text, scope, sort, visibility, |query, index| resolver.resolve(query, index))?;
    semantic.remember_moments(listing.set, resolver.take_moments());
    Ok(listing)
}
