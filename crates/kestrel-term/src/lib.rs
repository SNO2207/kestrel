//! Shells in pseudoconsoles, for Kestrel's terminal pane.
//!
//! - [`pty`]: a session: a shell in a Windows pseudoconsole (ConPTY), in a job object of its
//!   own, its output read on a thread, and everything it started ended with it;
//! - [`shells`]: the shells installed, how each is started in a folder, and what to type to
//!   take one to another folder;
//! - [`pe`]: console or windowed program, from the executable's header.

pub mod pe;
pub mod pty;
pub mod shells;

pub use pty::{Arg, Command, Event, Session, Size};
pub use shells::{Kind, Shell};

#[cfg(test)]
mod tests;
