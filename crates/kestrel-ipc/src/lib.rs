//! Kestrel's local query service.
//!
//! The running app answers questions from other programs of the same account: searches of its
//! index, folder listings with sizes, folder sizes, projects, recent files and disk usage. It
//! never changes a file for them. `kestrel-cli --live` and the MCP server (`apps/mcp`) are its
//! clients.
//!
//! - [`protocol`]: the requests and answers, one JSON line each, and the result limits;
//! - [`server`]: the named pipe server, bounded in connections, sizes and time;
//! - [`client`]: connecting (and checking whose the pipe is) and asking;
//! - [`name`]: where the app listens, worked out from its data folder;
//! - [`security`]: the pipe's access list, which admits this account only.

pub mod client;
pub mod name;
mod pipe;
pub mod protocol;
pub mod security;
pub mod server;

pub use client::{CallError, Client, call, call_as};
pub use protocol::{Failure, FailureKind, Request};
pub use server::{Context, Limits, Server};

#[cfg(test)]
mod tests;
