//! Reading repositories with the git command line, strictly without writing to them.
//!
//! Every command runs with `--no-optional-locks` (and `GIT_OPTIONAL_LOCKS=0`), so `git status`
//! never refreshes the index file as it otherwise would, and with the repository's file system
//! monitor hook switched off, since that hook is a program named by the repository's own
//! configuration. Commands run without a window and are killed if they outlast their timeout.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;

/// `CREATE_NO_WINDOW`: git is a console program and would otherwise flash a console.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long one git command may take before it is abandoned.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Options that keep every command read-only and free of the repository's own hooks.
const SAFE_ARGS: &[&str] = &[
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.quotepath=off",
    "-c",
    "color.ui=never",
    "-c",
    "log.showSignature=false",
];

#[derive(Debug)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The first line of what git said went wrong.
    pub fn reason(&self) -> String {
        let stderr = String::from_utf8_lossy(&self.stderr);
        stderr.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("git failed").to_owned()
    }
}

/// Runs `git <args>` in `dir`, feeding it `input`, and gives up after `timeout`.
pub fn run(dir: &Path, args: &[&str], input: Option<&[u8]>, timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new("git")
        .args(SAFE_ARGS)
        .args(args)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "Git isn't installed, or isn't on the PATH".to_owned(),
            _ => format!("Couldn't run git: {e}"),
        })?;

    // The pipes are drained on their own threads so a large output can never fill a pipe and
    // stall git while this thread waits for it to exit.
    let feeder = match (input, child.stdin.take()) {
        (Some(input), Some(mut stdin)) => {
            let input = input.to_vec();
            Some(std::thread::spawn(move || {
                let _ = stdin.write_all(&input);
            }))
        }
        _ => None,
    };
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {} took longer than {} s", args.first().unwrap_or(&""), timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("Couldn't wait for git: {e}")),
        }
    };
    if let Some(feeder) = feeder {
        let _ = feeder.join();
    }
    Ok(Output {
        code: status.code(),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// What a changed path is, as the badges in folder views show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    // Ordered by how loudly a folder containing several kinds should say so.
    Untracked,
    Added,
    Modified,
    Conflicted,
}

/// One path git reports, relative to the repository root, with `\` separators. Untracked
/// folders are reported once, with a trailing `\`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub change: Change,
    /// Staged in the index (the X column of `git status`).
    pub staged: bool,
    /// Changed in the working tree (the Y column).
    pub unstaged: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Branch {
    /// `None` when HEAD is detached.
    pub name: Option<String>,
    /// The commit HEAD points at, abbreviated; `None` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Branch,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl Status {
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for entry in &self.entries {
            match entry.change {
                Change::Untracked => counts.untracked += 1,
                Change::Conflicted => counts.conflicted += 1,
                _ => {
                    counts.staged += u32::from(entry.staged);
                    counts.modified += u32::from(entry.unstaged);
                }
            }
        }
        counts
    }
}

/// Parses `git status --porcelain=v2 --branch -z`.
pub fn parse_status(raw: &[u8]) -> Status {
    let mut status = Status::default();
    let mut fields = raw.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let to_windows = |path: &str| path.replace('/', "\\");
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => status.branch.head = Some(value.chars().take(12).collect()),
                "branch.head" if value != "(detached)" => status.branch.name = Some(value.to_owned()),
                "branch.upstream" => status.branch.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for part in value.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            status.branch.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.branch.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = record.as_bytes()[0];
        let (path, xy) = match kind {
            b'1' => {
                let parts: Vec<&str> = record.splitn(9, ' ').collect();
                (parts.get(8).copied(), parts.get(1).copied())
            }
            b'2' => {
                let parts: Vec<&str> = record.splitn(10, ' ').collect();
                // The original path of a rename or copy follows as a field of its own.
                fields.next();
                (parts.get(9).copied(), parts.get(1).copied())
            }
            b'u' => {
                let parts: Vec<&str> = record.splitn(11, ' ').collect();
                (parts.get(10).copied(), parts.get(1).copied())
            }
            b'?' => (record.get(2..), None),
            _ => continue,
        };
        let Some(path) = path else { continue };
        let entry = match (kind, xy) {
            (b'?', _) => Entry { path: to_windows(path), change: Change::Untracked, staged: false, unstaged: false },
            (b'u', _) => Entry { path: to_windows(path), change: Change::Conflicted, staged: false, unstaged: false },
            (_, Some(xy)) if xy.len() == 2 => {
                let (x, y) = (xy.as_bytes()[0], xy.as_bytes()[1]);
                let added = matches!(x, b'A' | b'R' | b'C');
                Entry {
                    path: to_windows(path),
                    change: if added && y == b'.' { Change::Added } else { Change::Modified },
                    staged: x != b'.',
                    unstaged: y != b'.',
                }
            }
            _ => continue,
        };
        status.entries.push(entry);
    }
    status
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub subject: String,
    pub author: String,
    /// Milliseconds since the Unix epoch.
    pub time: i64,
}

/// Parses `git log -1 --format=%s%x00%an%x00%at`.
pub fn parse_last_commit(raw: &str) -> Option<Commit> {
    let mut parts = raw.trim_end_matches(['\r', '\n']).splitn(3, '\0');
    let subject = parts.next()?.to_owned();
    let author = parts.next()?.to_owned();
    let time: i64 = parts.next()?.trim().parse().ok()?;
    Some(Commit { subject, author, time: time * 1000 })
}

/// The URL of a remote with any credentials taken out, for showing.
pub fn display_remote(url: &str) -> String {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        // `ssh://git@host` names a user, not a secret; keep it as git shows it.
        let user = match authority.rsplit_once('@') {
            Some((user, _)) if scheme.starts_with("ssh") && !user.contains(':') => format!("{user}@"),
            _ => String::new(),
        };
        return if path.is_empty() { format!("{scheme}://{user}{host}") } else { format!("{scheme}://{user}{host}/{path}") };
    }
    url.to_owned()
}

/// The web page of a repository hosted on GitHub, GitLab or Bitbucket (including self-hosted
/// GitLab and GitHub Enterprise hosts named as such), from its remote URL.
pub fn web_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (host, path) = if let Some((scheme, rest)) = remote.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        // An SSH port belongs to SSH, not to the web site.
        let host = if scheme == "https" || scheme == "http" { host } else { host.split(':').next()? };
        (host.to_owned(), path.to_owned())
    } else {
        // scp-like syntax: `git@github.com:owner/repo.git`.
        let (authority, path) = remote.split_once(':')?;
        if authority.contains('/') || authority.contains('\\') || authority.len() == 1 {
            return None; // a local path such as C:\repos\x.git
        }
        let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        (host.to_owned(), path.to_owned())
    };
    let lower = host.to_lowercase();
    if !(lower.contains("github") || lower.contains("gitlab") || lower.contains("bitbucket")) {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    // Bitbucket Server puts repositories under /scm/; their pages live elsewhere, so only the
    // plain owner/name layout of the hosted services is turned into a link.
    if path.split('/').filter(|p| !p.is_empty()).count() < 2 || path.starts_with("scm/") {
        return None;
    }
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~')) {
        return None;
    }
    Some(format!("https://{lower}/{path}"))
}

/// Reads the status of the repository at `root`.
pub fn status(root: &Path) -> Result<Status, String> {
    let output = run(root, &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=normal"], None, TIMEOUT)?;
    if !output.ok() {
        return Err(output.reason());
    }
    Ok(parse_status(&output.stdout))
}

/// Parses the `!` (ignored) records of `git status --porcelain=v2 -z --ignored`: lower-cased
/// paths relative to the root with `/` separators, whole ignored folders ending in `/`.
pub fn parse_ignored(raw: &[u8]) -> HashSet<String> {
    raw.split(|&b| b == 0)
        .filter_map(|record| record.strip_prefix(b"! "))
        .map(|path| String::from_utf8_lossy(path).to_lowercase())
        .collect()
}

/// What git ignores in the working tree, as [`parse_ignored`] gives it. Ignored folders are
/// reported once, not file by file.
pub fn ignored_paths(root: &Path) -> Result<HashSet<String>, String> {
    let args = ["status", "--porcelain=v2", "-z", "--ignored=traditional", "--untracked-files=normal"];
    let output = run(root, &args, None, TIMEOUT)?;
    if !output.ok() {
        return Err(output.reason());
    }
    Ok(parse_ignored(&output.stdout))
}

/// The newest commit on HEAD, or `None` before the first.
pub fn last_commit(root: &Path) -> Result<Option<Commit>, String> {
    let output = run(root, &["log", "-1", "--format=%s%x00%an%x00%at"], None, TIMEOUT)?;
    // An empty repository has no HEAD to log.
    Ok(if output.ok() { parse_last_commit(&output.text()) } else { None })
}

/// The URL of the `origin` remote, if there is one.
pub fn origin(root: &Path) -> Result<Option<String>, String> {
    let output = run(root, &["config", "--get", "remote.origin.url"], None, TIMEOUT)?;
    Ok(if output.ok() { Some(output.text().trim().to_owned()).filter(|url| !url.is_empty()) } else { None })
}

/// A Unix time as git's date parser reads it unambiguously: `2026-01-31 12:00:00 +0000`.
pub fn git_date(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} +0000", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Commit times (Unix seconds) on HEAD since `since` (Unix seconds), newest first, at most
/// `limit` of them.
pub fn commit_times(root: &Path, since: i64, limit: u32) -> Result<Vec<i64>, String> {
    let since = format!("--since={}", git_date(since));
    let limit = format!("--max-count={limit}");
    let output = run(root, &["log", &since, &limit, "--format=%at"], None, TIMEOUT)?;
    if !output.ok() {
        return Ok(Vec::new());
    }
    Ok(output.text().lines().filter_map(|line| line.trim().parse().ok()).collect())
}

/// Which of `paths` (relative to `root`, with `/` separators; folders end in `/`) git ignores.
/// Returned in the form given.
///
/// git refuses a whole batch over a single path inside a submodule (or beyond a symbolic
/// link), so such paths are dropped, as not ignored by this repository, and the rest asked
/// again.
pub fn ignored(root: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    let mut asking: Vec<String> = paths.to_vec();
    for _ in 0..16 {
        if asking.is_empty() {
            return Ok(Vec::new());
        }
        let reason = match check_ignore(root, &asking)? {
            Ok(found) => return Ok(found),
            Err(reason) => reason,
        };
        let Some(refused) = refused_path(&reason) else { return Err(reason) };
        let before = asking.len();
        asking.retain(|path| !path_within(path, &refused));
        if asking.len() == before {
            return Err(reason);
        }
    }
    Err("git refused too many of the paths asked about".into())
}

fn check_ignore(root: &Path, paths: &[String]) -> Result<Result<Vec<String>, String>, String> {
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let output = run(root, &["check-ignore", "-z", "--stdin"], Some(&input), TIMEOUT)?;
    // 1 means none of the paths is ignored.
    Ok(match output.code {
        Some(0) | Some(1) => Ok(output
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()),
        _ => Err(output.reason()),
    })
}

/// The path git refused in a message such as `fatal: Pathspec 'a/b/c' is in submodule 'a/b'`
/// (the submodule, so everything in it goes) or `fatal: pathspec 'x/y' is beyond a symbolic
/// link`.
fn refused_path(reason: &str) -> Option<String> {
    let quoted: Vec<&str> = reason.split('\'').skip(1).step_by(2).collect();
    if reason.contains("is in submodule") {
        return quoted.get(1).map(|s| (*s).to_owned());
    }
    if reason.to_lowercase().contains("pathspec") {
        return quoted.first().map(|s| (*s).to_owned());
    }
    None
}

/// Whether `path` is `refused` or lies inside it, ignoring case and a trailing `/`.
fn path_within(path: &str, refused: &str) -> bool {
    let path = path.trim_end_matches('/').to_lowercase();
    let refused = refused.trim_end_matches('/').to_lowercase();
    path == refused || path.starts_with(&format!("{refused}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_headers_and_every_record_kind() {
        let raw = b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0\
# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb src/main.rs\0\
1 A. N... 000000 100644 100644 000 ccc new file.txt\0\
1 MM N... 100644 100644 100644 aaa bbb both.rs\0\
2 R. N... 100644 100644 100644 aaa aaa R100 renamed.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c clash.txt\0\
? notes/\0? scratch.txt\0";
        let status = parse_status(raw);
        assert_eq!(
            status.branch,
            Branch {
                name: Some("main".into()),
                head: Some("0123456789ab".into()),
                upstream: Some("origin/main".into()),
                ahead: 2,
                behind: 1
            }
        );
        let entries: Vec<(&str, Change, bool, bool)> =
            status.entries.iter().map(|e| (e.path.as_str(), e.change, e.staged, e.unstaged)).collect();
        assert_eq!(
            entries,
            [
                (r"src\main.rs", Change::Modified, false, true),
                ("new file.txt", Change::Added, true, false),
                ("both.rs", Change::Modified, true, true),
                ("renamed.rs", Change::Added, true, false),
                ("clash.txt", Change::Conflicted, false, false),
                (r"notes\", Change::Untracked, false, false),
                ("scratch.txt", Change::Untracked, false, false),
            ]
        );
        assert_eq!(status.counts(), Counts { staged: 3, modified: 2, untracked: 2, conflicted: 1 });
    }

    #[test]
    fn a_fresh_or_detached_head_has_no_name_or_commit() {
        let status = parse_status(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(status.branch.head, None);
        assert_eq!(status.branch.name.as_deref(), Some("main"));
        let detached = parse_status(b"# branch.oid abc\0# branch.head (detached)\0");
        assert_eq!(detached.branch.name, None);
    }

    #[test]
    fn last_commits_parse() {
        let commit = parse_last_commit("Fix the thing\0Ada Lovelace\x001767225600\n").unwrap();
        assert_eq!(commit, Commit { subject: "Fix the thing".into(), author: "Ada Lovelace".into(), time: 1_767_225_600_000 });
        assert_eq!(parse_last_commit(""), None);
    }

    #[test]
    fn remotes_become_web_links_for_known_hosts_only() {
        let cases = [
            ("git@github.com:owner/repo.git", Some("https://github.com/owner/repo")),
            ("https://github.com/owner/repo.git", Some("https://github.com/owner/repo")),
            ("https://user:secret-token@github.com/owner/repo", Some("https://github.com/owner/repo")),
            ("ssh://git@github.com:22/owner/repo.git", Some("https://github.com/owner/repo")),
            ("git@gitlab.com:group/sub/project.git", Some("https://gitlab.com/group/sub/project")),
            ("https://gitlab.example.org/team/app.git/", Some("https://gitlab.example.org/team/app")),
            ("https://ada@bitbucket.org/team/repo.git", Some("https://bitbucket.org/team/repo")),
            ("git@ssh.dev.azure.com:v3/org/project/repo", None),
            ("https://example.com/owner/repo.git", None),
            (r"C:\repos\bare.git", None),
            ("file:///C:/repos/bare.git", None),
            ("https://github.com/owner", None),
            ("https://github.com/owner/repo\"><script>", None),
        ];
        for (remote, expected) in cases {
            assert_eq!(web_url(remote).as_deref(), expected, "{remote}");
        }
    }

    #[test]
    fn ignored_records_are_collected() {
        let raw = b"1 .M N... 1 1 1 a b src/x.rs\0! Target/\0! apps/e2e/out/\0! debug.log\0? new.txt\0";
        let ignored = parse_ignored(raw);
        let mut list: Vec<&str> = ignored.iter().map(String::as_str).collect();
        list.sort();
        assert_eq!(list, ["apps/e2e/out/", "debug.log", "target/"]);
    }

    #[test]
    fn refused_paths_are_read_from_git_messages() {
        let submodule = "fatal: Pathspec 'libs/inner/Cargo.toml' is in submodule 'libs/inner'";
        assert_eq!(refused_path(submodule).as_deref(), Some("libs/inner"));
        assert_eq!(refused_path("fatal: pathspec 'a/b' is beyond a symbolic link").as_deref(), Some("a/b"));
        assert_eq!(refused_path("fatal: not a git repository"), None);
        assert!(path_within("libs/inner/", "libs/inner"));
        assert!(path_within("Libs/Inner/x.rs", "libs/inner"));
        assert!(!path_within("libs/innermost", "libs/inner"));
    }

    #[test]
    fn dates_are_written_for_git() {
        assert_eq!(git_date(0), "1970-01-01 00:00:00 +0000");
        assert_eq!(git_date(1_767_225_600), "2026-01-01 00:00:00 +0000");
        assert_eq!(git_date(951_782_400 + 3661), "2000-02-29 01:01:01 +0000");
    }

    #[test]
    fn credentials_never_reach_the_screen() {
        assert_eq!(display_remote("https://user:token@github.com/o/r.git"), "https://github.com/o/r.git");
        assert_eq!(display_remote("ssh://git@host.com/o/r.git"), "ssh://git@host.com/o/r.git");
        assert_eq!(display_remote("git@github.com:o/r.git"), "git@github.com:o/r.git");
    }
}
