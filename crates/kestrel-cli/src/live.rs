//! `--live`: asking the running Kestrel over its query pipe (`kestrel_ipc`) instead of loading
//! the saved index, which is current within a second and costs no load. When Kestrel isn't
//! running, the commands that can fall back to the snapshot do.

use std::time::Duration;

use anyhow::{Result, bail};
use kestrel_ipc::protocol::{
    DiskArgs, DiskUsage, FolderListing, FolderSize, Hello, LimitArgs, ListArgs, PathArgs, ProjectArgs, ProjectDetails,
    ProjectList, ProjectsArgs, RecentFiles, Request, SearchArgs, SearchResult, SortBy,
};
use kestrel_ipc::{CallError, call};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::human;

/// How long the CLI waits for Kestrel.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Where to ask, and how to print.
pub struct Live {
    pub pipe: String,
    pub json: bool,
}

impl Live {
    /// Asks Kestrel; `None` if it isn't running (or its query service is off).
    fn ask(&self, request: &Request) -> Result<Option<Value>> {
        match call(&self.pipe, request, TIMEOUT) {
            Ok(value) => Ok(Some(value)),
            Err(CallError::NotRunning) => Ok(None),
            Err(error) => bail!("{error}"),
        }
    }

    fn ask_as<T: DeserializeOwned>(&self, request: &Request) -> Result<Option<(T, Value)>> {
        let Some(value) = self.ask(request)? else { return Ok(None) };
        Ok(Some((serde_json::from_value(value.clone())?, value)))
    }

    /// Prints the answer as JSON when asked to, and says whether it did.
    fn printed_json(&self, value: &Value) -> bool {
        if self.json {
            println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
        }
        self.json
    }

    /// The answer, or an error saying the command needs Kestrel running.
    fn require<T: DeserializeOwned>(&self, request: &Request) -> Result<(T, Value)> {
        match self.ask_as(request)? {
            Some(answer) => Ok(answer),
            None => bail!("Kestrel isn't running (or its query service is turned off in its Developer tools page); this command needs it"),
        }
    }

    /// `search`: true if Kestrel answered.
    pub fn search(&self, query: String, sort: SortBy, descending: bool, limit: usize) -> Result<bool> {
        let request = Request::Search(SearchArgs { query, limit: Some(limit), sort: Some(sort), descending: Some(descending), ..SearchArgs::default() });
        let Some((found, value)) = self.ask_as::<SearchResult>(&request)? else { return Ok(false) };
        if !self.printed_json(&value) {
            if let Some(ran_as) = &found.ran_as {
                println!("(read as: {ran_as})");
            }
            for entry in &found.results {
                println!("{:>10}  {}", human(entry.size), entry.path);
                if let Some(matched) = &entry.matched {
                    println!("{:>10}  {}: {}", "", matched.line, matched.text);
                }
            }
            println!("{} results in {:.2} ms (from the running Kestrel)", found.total, found.took_ms);
        }
        Ok(true)
    }

    /// `ls`: true if Kestrel answered.
    pub fn ls(&self, path: String) -> Result<bool> {
        let request = Request::ListFolder(ListArgs { path, limit: Some(2000), sort: Some(SortBy::Size), descending: Some(true), hidden: true });
        let Some((listing, value)) = self.ask_as::<FolderListing>(&request)? else { return Ok(false) };
        if !self.printed_json(&value) {
            let mut items = listing.items.clone();
            // Largest first, folders among the files, as the snapshot's listing shows them.
            items.sort_by_key(|entry| std::cmp::Reverse(entry.size));
            for entry in &items {
                let marker = if entry.is_folder() { "\\" } else { "" };
                println!("{:>10}  {}{marker}", human(entry.size), entry.name);
            }
            let more = if listing.truncated { format!(" ({} shown)", listing.returned) } else { String::new() };
            println!("{} items{more}, {} total (from the running Kestrel)", listing.total, human(listing.size));
        }
        Ok(true)
    }

    /// `size`: true if Kestrel answered.
    pub fn size(&self, path: String) -> Result<bool> {
        let Some((size, value)) = self.ask_as::<FolderSize>(&Request::FolderSize(PathArgs { path }))? else { return Ok(false) };
        if !self.printed_json(&value) {
            print_size(&size.path, size.size, size.size_on_disk, size.files, size.folders);
            if !size.complete {
                println!("(counting stopped early: the numbers of files and folders are incomplete)");
            }
        }
        Ok(true)
    }

    /// `usage`: true if Kestrel answered.
    pub fn usage(&self, path: String) -> Result<bool> {
        let Some((usage, value)) = self.ask_as::<DiskUsage>(&Request::DiskUsage(DiskArgs { path, top: Some(10) }))? else { return Ok(false) };
        if self.printed_json(&value) {
            return Ok(true);
        }
        println!("{}", usage.path);
        println!("  {} on disk, {} of files, {} files, {} folders", human(usage.size_on_disk), human(usage.size), usage.files, usage.folders);
        if let Some(drive) = &usage.drive {
            println!("  drive {}: {} free of {} ({})", drive.root, human(drive.free), human(drive.total), drive.file_system);
        }
        println!("\nby type");
        for row in &usage.types {
            println!("  {:<28} {:>10}  {:>9} files", row.label, human(row.size_on_disk), row.files);
        }
        println!("\nby age");
        for row in &usage.ages {
            println!("  {:<28} {:>10}  {:>9} files", row.label, human(row.size_on_disk), row.files);
        }
        println!("\nlargest folders");
        for entry in &usage.largest_folders {
            println!("  {:>10}  {}", human(entry.size_on_disk), entry.path);
        }
        println!("\nlargest files");
        for entry in &usage.largest_files {
            println!("  {:>10}  {}", human(entry.size_on_disk), entry.path);
        }
        Ok(true)
    }

    pub fn status(&self) -> Result<()> {
        let (hello, value) = self.require::<Hello>(&Request::Hello)?;
        if !self.printed_json(&value) {
            println!("{} {} (process {}), index {} with {} entries, on {}", hello.app, hello.version, hello.pid, hello.index, hello.entries, self.pipe);
        }
        Ok(())
    }

    pub fn projects(&self, filter: Option<String>, limit: usize) -> Result<()> {
        let (list, value) = self.require::<ProjectList>(&Request::Projects(ProjectsArgs { limit: Some(limit), filter }))?;
        if self.printed_json(&value) {
            return Ok(());
        }
        for project in &list.projects {
            let branch = project.git.as_ref().and_then(|git| git.branch.clone()).unwrap_or_default();
            let changes = project.git.as_ref().map_or(0, |git| git.staged + git.modified + git.untracked + git.conflicted);
            let changed = if changes > 0 { format!(", {changes} changed") } else { String::new() };
            println!("{:<24} {:<10} {:>10}  {:<24} {}{changed}", project.name, project.status, human(project.size_on_disk), project.stack.join(" "), branch);
            println!("{:<24} {}", "", project.root);
        }
        let more = if list.truncated { format!(" ({} shown)", list.returned) } else { String::new() };
        println!("{} projects{more}{}", list.total, if list.indexing { " so far; the index is still being built" } else { "" });
        Ok(())
    }

    pub fn project(&self, project: String) -> Result<()> {
        let (details, value) = self.require::<ProjectDetails>(&Request::ProjectDetails(ProjectArgs { project }))?;
        if self.printed_json(&value) {
            return Ok(());
        }
        let summary = &details.summary;
        println!("{}  {}", summary.name, summary.root);
        println!("  {}  {}  {} on disk, {} build output", summary.status, summary.stack.join(" "), human(summary.size_on_disk), human(summary.build_output));
        if let Some(git) = &summary.git {
            println!(
                "  {} ({} ahead, {} behind), {} staged, {} modified, {} untracked",
                git.branch.clone().unwrap_or_default(),
                git.ahead,
                git.behind,
                git.staged,
                git.modified,
                git.untracked
            );
            if let Some(commit) = &git.last_commit {
                println!("  last commit: {commit}");
            }
        }
        if !details.scripts.is_empty() {
            println!("\nscripts");
            for script in &details.scripts {
                println!("  {:<8} {:<20} {}", script.source, script.name, script.command);
            }
        }
        if !details.recent_files.is_empty() {
            println!("\nrecently changed");
            for entry in &details.recent_files {
                println!("  {}  {}", entry.modified.clone().unwrap_or_default(), entry.path);
            }
        }
        Ok(())
    }

    pub fn recent(&self, limit: usize) -> Result<()> {
        let (recent, value) = self.require::<RecentFiles>(&Request::RecentFiles(LimitArgs { limit: Some(limit) }))?;
        if !self.printed_json(&value) {
            for file in &recent.files {
                println!("{}  {}", file.opened, file.path);
            }
        }
        Ok(())
    }
}

pub fn print_size(path: &str, size: u64, on_disk: u64, files: u64, folders: u64) {
    println!("{path}");
    println!("  {} ({size} bytes), {} on disk", human(size), human(on_disk));
    println!("  {files} files, {folders} folders");
}
