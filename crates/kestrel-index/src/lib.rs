//! In-memory index of every file on a volume.
//!
//! The index is a struct of arrays: one column per attribute, one element per entry. Names live
//! in two NUL-separated byte arenas that share offsets, one for display and one case-folded for
//! search. Entries are append-only; deletion sets a tombstone bit and compaction happens by
//! rebuilding through [`IndexBuilder`].

#[cfg(windows)]
pub mod broker;
mod builder;
mod fold;
mod index;
#[cfg(windows)]
pub mod live;
#[cfg(windows)]
pub mod mft;
mod natural;
pub mod snapshot;
pub mod sync;
#[cfg(windows)]
pub mod walk;

pub use builder::IndexBuilder;
pub use fold::{fold, fold_into};
pub use index::{DirItem, EntryId, Index, JournalCursor, Meta, NO_ENTRY, attr};
pub use natural::natural_cmp;
