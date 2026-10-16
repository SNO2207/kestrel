//! `kestrel-mcp` driven over stdio as an MCP client drives it (initialize, tools/list,
//! tools/call), against a stand-in for Kestrel: a query pipe of the test's own, served by
//! `kestrel_ipc` with canned answers, which records what it was asked.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kestrel_ipc::protocol::{Failure, FailureKind, Request};
use kestrel_ipc::server::Handler;
use kestrel_ipc::{Context, Limits, Server};
use serde_json::{Value, json};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn pipe_name(label: &str) -> String {
    format!(r"\\.\pipe\kestrel-mcp-test-{}-{}-{label}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

fn entry(path: &str, kind: &str, size: u64) -> Value {
    let name = path.rsplit('\\').next().unwrap();
    json!({"path": path, "name": name, "kind": kind, "size": size, "size_on_disk": size, "modified": "2026-10-15T11:30:45Z"})
}

/// Answers like Kestrel would, from a made-up drive, and records every request.
fn stand_in(asked: Arc<Mutex<Vec<Request>>>) -> Arc<Handler> {
    Arc::new(move |request: Request, _: &Context| {
        asked.lock().unwrap().push(request.clone());
        match request {
            Request::Hello => Ok(json!({"app": "Kestrel", "version": "test", "protocol": 1, "pid": 1, "index": "ready", "entries": 3})),
            Request::Search(args) if args.query == "fail" => Err(Failure::bad_request("Unknown clause \"fail\"")),
            Request::Search(args) => Ok(json!({
                "query": args.query, "total": 1, "returned": 1, "truncated": false, "took_ms": 0.5,
                "results": [entry(r"C:\Code\kestrel\notes.md", "file", 42)]
            })),
            Request::ListFolder(args) => Ok(json!({
                "path": args.path, "size": 1042, "size_on_disk": 4096, "indexed": true, "total": 2, "returned": 2, "truncated": false,
                "items": [entry(r"C:\Code\src", "folder", 1000), entry(r"C:\Code\notes.md", "file", 42)]
            })),
            Request::FolderSize(args) if args.path.contains("nowhere") => Err(Failure::not_found(format!("{} is not in Kestrel's index", args.path))),
            Request::FolderSize(args) => Ok(json!({"path": args.path, "size": 1042, "size_on_disk": 4096, "files": 2, "folders": 1, "cloud_only_files": 0, "complete": true})),
            Request::Projects(_) => Ok(json!({"total": 1, "returned": 1, "truncated": false, "indexing": false, "projects": [
                {"name": "kestrel", "root": r"C:\Code\kestrel", "stack": ["Rust"], "status": "active", "is_repo": true, "size_on_disk": 10, "build_output": 4}
            ]})),
            Request::ProjectDetails(args) => Ok(json!({"name": args.project, "root": r"C:\Code\kestrel", "stack": ["Rust"], "status": "active", "is_repo": true,
                "size_on_disk": 10, "build_output": 4, "languages": [["Rust", 1.0]], "scripts": [{"source": "cargo", "name": "test", "command": "cargo test"}],
                "recent_files": [], "build_output_folders": []})),
            Request::RecentFiles(_) => Ok(json!({"returned": 1, "files": [{"path": r"C:\Code\notes.md", "name": "notes.md", "folder": r"C:\Code", "opened": "2026-10-15T11:30:45Z", "source": "windows"}]})),
            Request::DiskUsage(args) => Ok(json!({"path": args.path, "size_on_disk": 4096, "size": 1042, "files": 2, "folders": 1, "cloud_only_files": 0,
                "indexing": false, "types": [], "ages": [], "largest_files": [], "largest_folders": []})),
        }
    })
}

/// A running `kestrel-mcp`, spoken to line by line.
struct Mcp {
    child: Child,
    input: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(pipe: &str) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kestrel-mcp"))
            .args(["--pipe", pipe])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (send, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Mcp { child, input, lines, next_id: 1 }
    }

    fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_vec(message).unwrap();
        line.push(b'\n');
        self.input.write_all(&line).unwrap();
        self.input.flush().unwrap();
    }

    /// Sends a request and returns its response, skipping any notifications in between.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self.lines.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| panic!("no answer to {method}"));
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|error| panic!("not JSON-RPC on stdout ({error}): {line}"));
            assert_eq!(message["jsonrpc"], "2.0", "{message}");
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
        }
    }

    fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "kestrel-test", "version": "1.0"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        response
    }

    /// Calls a tool and returns its result: whether it is an error, and its text.
    fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
        let response = self.request("tools/call", json!({"name": tool, "arguments": arguments}));
        let result = response.get("result").unwrap_or_else(|| panic!("{tool} failed: {response}"));
        let text = result["content"][0]["text"].as_str().unwrap_or_default().to_owned();
        (result["isError"].as_bool().unwrap_or(false), text)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const TOOLS: [&str; 7] = ["search_files", "list_folder", "folder_size", "list_projects", "project_details", "recent_files", "disk_usage"];

#[test]
fn initialize_announces_kestrel_and_its_tools() {
    let mut mcp = Mcp::start(&pipe_name("initialize"));
    let response = mcp.initialize();
    let result = &response["result"];
    assert_eq!(result["serverInfo"]["name"], "kestrel", "{response}");
    assert!(result["capabilities"]["tools"].is_object(), "{response}");
    assert!(result["protocolVersion"].is_string());
    assert!(result["instructions"].as_str().unwrap().contains("only reads"));

    let listed = mcp.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    names.sort();
    let mut expected = TOOLS.to_vec();
    expected.sort();
    assert_eq!(names, expected);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(tool["description"].as_str().is_some_and(|d| d.len() > 60), "{name} explains itself");
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name} says it only reads");
        assert_eq!(tool["annotations"]["destructiveHint"], false, "{name}");
    }
    let search = tools.iter().find(|tool| tool["name"] == "search_files").unwrap();
    let description = search["description"].as_str().unwrap();
    for syntax in ["ext:", "type:image", "size:>1gb", "dm:", "in:", "regex:", "tag:", "*.rs", "content:", "taken:", "kind:photo", "duration:", "text:", "like:", "ran_as"] {
        assert!(description.contains(syntax), "the query syntax documents {syntax}");
    }
    assert_eq!(search["inputSchema"]["required"], json!(["query"]));
    for property in ["query", "folder", "limit", "sort", "descending", "include_hidden"] {
        assert!(search["inputSchema"]["properties"][property].is_object(), "search_files takes {property}");
    }
    let list = tools.iter().find(|tool| tool["name"] == "list_folder").unwrap();
    assert_eq!(list["inputSchema"]["required"], json!(["path"]));
}

#[test]
fn each_tool_asks_kestrel_and_returns_its_answer() {
    let pipe = pipe_name("tools");
    let asked: Arc<Mutex<Vec<Request>>> = Arc::default();
    let _server = Server::start(&pipe, Limits::default(), stand_in(asked.clone()), None).unwrap();
    let mut mcp = Mcp::start(&pipe);
    mcp.initialize();

    let (failed, text) = mcp.call("search_files", json!({"query": "ext:md notes", "folder": r"C:\Code", "limit": 5, "sort": "size", "include_hidden": true}));
    assert!(!failed, "{text}");
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(answer["results"][0]["path"], r"C:\Code\kestrel\notes.md");
    assert_eq!(answer["total"], 1);

    let (failed, text) = mcp.call("list_folder", json!({"path": r"C:\Code", "sort": "size", "descending": false}));
    assert!(!failed, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["items"][0]["kind"], "folder");

    let (failed, text) = mcp.call("folder_size", json!({"path": r"C:\Code"}));
    assert!(!failed);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["files"], 2);

    let (failed, text) = mcp.call("list_projects", json!({"filter": "rust"}));
    assert!(!failed);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["projects"][0]["name"], "kestrel");

    let (failed, text) = mcp.call("project_details", json!({"project": "kestrel"}));
    assert!(!failed);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["scripts"][0]["command"], "cargo test");

    let (failed, text) = mcp.call("recent_files", json!({"limit": 3}));
    assert!(!failed);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["files"][0]["name"], "notes.md");

    let (failed, text) = mcp.call("disk_usage", json!({"path": r"C:\", "top": 3}));
    assert!(!failed);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["size_on_disk"], 4096);

    // Kestrel's own refusals reach the client as tool errors, worded for a person.
    let (failed, text) = mcp.call("folder_size", json!({"path": r"C:\nowhere"}));
    assert!(failed);
    assert!(text.contains("is not in Kestrel's index"), "{text}");
    let (failed, text) = mcp.call("search_files", json!({"query": "fail"}));
    assert!(failed);
    assert!(text.contains("Unknown clause"), "{text}");

    // What reached Kestrel: the arguments, carried over as asked.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 9);
    match &asked[0] {
        Request::Search(args) => {
            assert_eq!((args.query.as_str(), args.folder.as_deref(), args.limit), ("ext:md notes", Some(r"C:\Code"), Some(5)));
            assert_eq!(args.sort, Some(kestrel_ipc::protocol::SortBy::Size));
            assert!(args.hidden);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&asked[1], Request::ListFolder(args) if args.path == r"C:\Code" && args.descending == Some(false)));
    assert!(matches!(&asked[3], Request::Projects(args) if args.filter.as_deref() == Some("rust")));
    assert!(matches!(&asked[4], Request::ProjectDetails(args) if args.project == "kestrel"));
    assert!(matches!(&asked[5], Request::RecentFiles(args) if args.limit == Some(3)));
    assert!(matches!(&asked[6], Request::DiskUsage(args) if args.top == Some(3)));
}

#[test]
fn without_kestrel_the_tools_say_so() {
    // Nothing serves this pipe.
    let mut mcp = Mcp::start(&pipe_name("absent"));
    mcp.initialize();
    let (failed, text) = mcp.call("search_files", json!({"query": "anything"}));
    assert!(failed);
    assert!(text.contains("Kestrel isn't running"), "{text}");
    // The server itself keeps going.
    let (failed, _) = mcp.call("recent_files", json!({}));
    assert!(failed);
}

#[test]
fn wrong_arguments_are_refused_before_kestrel_is_asked() {
    let pipe = pipe_name("arguments");
    let asked: Arc<Mutex<Vec<Request>>> = Arc::default();
    let _server = Server::start(&pipe, Limits::default(), stand_in(asked.clone()), None).unwrap();
    let mut mcp = Mcp::start(&pipe);
    mcp.initialize();
    // No query.
    let response = mcp.request("tools/call", json!({"name": "search_files", "arguments": {"folder": r"C:\"}}));
    let refused = response.get("error").is_some() || response["result"]["isError"] == true;
    assert!(refused, "{response}");
    // A sort Kestrel doesn't know.
    let response = mcp.request("tools/call", json!({"name": "list_folder", "arguments": {"path": r"C:\", "sort": "colour"}}));
    assert!(response.get("error").is_some() || response["result"]["isError"] == true, "{response}");
    // No such tool, and nothing that changes files.
    let response = mcp.request("tools/call", json!({"name": "delete_file", "arguments": {"path": r"C:\x"}}));
    assert!(response.get("error").is_some() || response["result"]["isError"] == true, "{response}");
    assert!(asked.lock().unwrap().is_empty(), "nothing reached Kestrel");
}

#[test]
fn a_slow_kestrel_gives_a_timeout_not_a_hang() {
    let pipe = pipe_name("slow");
    let slow: Arc<Handler> = Arc::new(|_: Request, _: &Context| {
        std::thread::sleep(Duration::from_millis(800));
        Ok(json!({}))
    });
    let limits = Limits { handler_timeout: Duration::from_millis(200), ..Limits::default() };
    let _server = Server::start(&pipe, limits, slow, None).unwrap();
    let mut mcp = Mcp::start(&pipe);
    mcp.initialize();
    let (failed, text) = mcp.call("folder_size", json!({"path": r"C:\"}));
    assert!(failed);
    assert!(text.contains("longer than"), "{text}");
    let _ = FailureKind::Timeout;
}
