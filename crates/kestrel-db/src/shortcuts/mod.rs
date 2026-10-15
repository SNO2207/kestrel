//! Shortcuts: the ways back to places the user cares about. Pinned places in named groups,
//! folders ranked by how often and how recently they were visited, saved searches, coloured
//! tags on files and folders, and saved sets of tabs.
//!
//! Each kind of data has its own submodule, tables and migration. Functions take the [`Db`]
//! rather than extending it, so other features' methods can never collide with these.
//!
//! [`Db`]: crate::Db

pub mod frecency;
pub mod pins;
pub mod searches;
pub mod tags;
pub mod workspaces;

use rusqlite::{Connection, params};

use crate::{Error, Migration};

/// Every migration of the shortcuts feature, listed once in [`crate::MIGRATIONS`].
pub const MIGRATIONS: &[Migration] = &[
    pins::MIGRATION,
    frecency::MIGRATION,
    searches::MIGRATION,
    tags::MIGRATION,
    workspaces::MIGRATION,
];

/// Longest name a group, tag, saved search, workspace or alias may have.
pub const MAX_NAME: usize = 120;

/// Colours a group or tag may have. The front end maps each to a swatch that suits both
/// themes; storing names rather than values lets the palette change without migrating data.
pub const COLORS: &[&str] = &["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "gray"];

/// A trimmed, non-empty name of reasonable length.
pub(crate) fn clean_name(name: &str) -> crate::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error("A name can't be empty".into()));
    }
    if name.chars().count() > MAX_NAME {
        return Err(Error(format!("A name can be at most {MAX_NAME} characters long")));
    }
    if name.chars().any(char::is_control) {
        return Err(Error("A name can't contain control characters".into()));
    }
    Ok(name.to_owned())
}

/// A colour from [`COLORS`], or an error naming the bad one.
pub(crate) fn check_color(color: &str) -> crate::Result<String> {
    COLORS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(color))
        .map(|known| (*known).to_owned())
        .ok_or_else(|| Error(format!("Unknown colour \"{color}\"")))
}

/// Gives the rows `ids` of `table` the positions 0, 1, 2... in that order.
pub(crate) fn renumber(connection: &Connection, table: &str, ids: &[i64]) -> rusqlite::Result<()> {
    // `table` is always one of this module's own table names, never user input.
    let mut statement = connection.prepare(&format!("UPDATE {table} SET position = ?1 WHERE id = ?2"))?;
    for (position, id) in ids.iter().enumerate() {
        statement.execute(params![position as i64, id])?;
    }
    Ok(())
}

/// The position after the last row of `table` matching `filter` (an SQL condition).
pub(crate) fn next_position(connection: &Connection, table: &str, filter: &str) -> rusqlite::Result<i64> {
    connection.query_row(&format!("SELECT COALESCE(MAX(position) + 1, 0) FROM {table} WHERE {filter}"), [], |row| row.get(0))
}

/// Reports a missing row the way the front end shows it.
pub(crate) fn not_found(what: &str) -> Error {
    Error(format!("That {what} no longer exists"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(clean_name("  Work  ").unwrap(), "Work");
        assert!(clean_name("   ").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME + 1)).is_err());
        assert!(clean_name("tab\there").is_err());
        assert_eq!(check_color("Blue").unwrap(), "blue");
        assert!(check_color("chartreuse").is_err());
    }
}
