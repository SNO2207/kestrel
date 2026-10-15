//! Talking to the Kestrel broker: a small Windows service that runs with the rights needed to
//! read drives' master file tables and hands the resulting index to the app over a named pipe
//! that only the user who installed it can open.
//!
//! Protocol: the client writes one line, `scan <root>|<root>…`. The broker answers with one
//! status byte: 0 followed by an index snapshot, or 1 followed by an error message. Then it
//! closes the connection.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::time::Duration;

use crate::index::Index;
use crate::snapshot;

pub const PIPE: &str = r"\\.\pipe\kestrel-broker";
pub const OK: u8 = 0;
pub const FAILED: u8 = 1;

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// Formats a scan request for the given roots.
pub fn request_line(roots: &[String]) -> String {
    format!("scan {}\n", roots.join("|"))
}

/// Parses a request line into the roots to scan.
pub fn parse_request(line: &str) -> Option<Vec<String>> {
    let roots: Vec<String> = line.trim().strip_prefix("scan ")?.split('|').filter(|r| !r.is_empty()).map(str::to_owned).collect();
    (!roots.is_empty()).then_some(roots)
}

/// Asks the broker to build an index of `roots`. Returns `Ok(None)` when no broker is
/// installed or running, so the caller can fall back to crawling.
pub fn scan(roots: &[String]) -> io::Result<Option<Index>> {
    let mut pipe = loop {
        match OpenOptions::new().read(true).write(true).open(PIPE) {
            Ok(pipe) => break pipe,
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => return Ok(None),
            // Another client is being served; scans are short, so wait for a turn.
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(error),
        }
    };
    pipe.write_all(request_line(roots).as_bytes())?;
    read_reply(pipe).map(Some)
}

fn read_reply(pipe: File) -> io::Result<Index> {
    let mut reply = BufReader::with_capacity(1 << 20, pipe);
    let mut status = [0u8];
    reply.read_exact(&mut status)?;
    if status[0] == OK {
        return snapshot::read_from(&mut reply, u64::MAX);
    }
    let mut message = String::new();
    reply.read_to_string(&mut message)?;
    Err(io::Error::other(format!("the broker could not scan: {message}")))
}

/// Reads a request line from a connected client.
pub fn read_request(client: &mut impl Read) -> io::Result<Option<Vec<String>>> {
    let mut line = String::new();
    BufReader::new(client.take(64 * 1024)).read_line(&mut line)?;
    Ok(parse_request(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let roots = vec![r"C:\".to_owned(), r"D:\".to_owned()];
        assert_eq!(parse_request(&request_line(&roots)), Some(roots));
        assert_eq!(parse_request("scan \n"), None);
        assert_eq!(parse_request("delete everything"), None);
    }
}
