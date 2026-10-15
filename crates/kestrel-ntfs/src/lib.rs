//! Direct access to NTFS metadata: the change journal, which works for any user, and the
//! master file table, which needs administrator rights.

pub mod journal;
pub mod mft;
pub mod volume;
