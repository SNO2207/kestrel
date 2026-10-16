//! Query language and search over a [`kestrel_index::Index`].
//!
//! A query is a list of space-separated clauses that must all match:
//!
//! | Clause | Meaning |
//! |---|---|
//! | `report` | name contains `report` (case-insensitive) |
//! | `"two words"` | quoted text is taken literally |
//! | `src\main` | full path contains `src\main` |
//! | `*.rs`, `img_????.jpg` | wildcard match on the whole name |
//! | `ext:rs;toml` | extension is one of these |
//! | `type:image` | image, video, audio, doc, archive, code, exe, folder or file |
//! | `size:>1gb`, `size:10mb..2gb`, `size:huge` | size filter |
//! | `dm:today`, `dm:7d`, `dm:>2026-01`, `dm:2025` | date-modified filter |
//! | `in:C:\Projects` | only beneath this folder |
//! | `regex:^a.*\d$` | regular expression on the name |
//! | `tag:urgent` | carries this tag (resolved by the caller with [`resolve_tags`]) |
//! | `width:>1920`, `height:1080`, `taken:2024`, `camera:pixel`, `duration:>1m`, `kind:photo`, `kind:video`, `geo:yes`, `text:costco` | facts the media library holds (resolved by the caller with [`resolve_media`]) |
//! | `like:"a dog on a sofa"`, `~a dog on a sofa` | photos and videos that look like it (resolved by the caller with [`resolve_likes`]) |
//! | `!clause` | negates any clause |

mod exec;
mod filter;
mod glob;
pub mod group;
mod parse;
mod sort;
mod time;

pub use exec::{Compiled, compile, search};
pub use filter::NameFilter;
pub use group::{DateEdges, Group, GroupBy, Grouping};
pub use parse::{
    Clause, ClauseKind, MediaClause, MediaKind, ParseError, Query, has_media, likes, parse, resolve_likes, resolve_media,
    resolve_tags,
};
pub use sort::{Sort, SortKey, sort};
pub use time::Clock;
