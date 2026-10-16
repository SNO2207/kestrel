//! The server and client together, over real pipes with names of their own.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::client::{self, CallError, Client};
use crate::pipe::read_line;
use crate::protocol::{Failure, FailureKind, PathArgs, Request, SearchArgs};
use crate::security;
use crate::server::{Context, Handler, Limits, Server, listening_dacl};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn unique(label: &str) -> String {
    format!(r"\\.\pipe\kestrel-ipc-test-{}-{}-{label}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

/// Answers `hello` with a greeting and `search` by echoing the query; anything else fails.
fn echo() -> Arc<Handler> {
    Arc::new(|request: Request, _: &Context| match request {
        Request::Hello => Ok(json!({"app": "test"})),
        Request::Search(args) => Ok(json!({"query": args.query})),
        other => Err(Failure::not_found(format!("no {}", other.op()))),
    })
}

fn search(query: &str) -> Request {
    Request::Search(SearchArgs { query: query.into(), ..SearchArgs::default() })
}

const SECOND: Duration = Duration::from_secs(1);

#[test]
fn questions_are_answered_one_after_another_on_one_connection() {
    let name = unique("round-trip");
    let _server = Server::start(&name, Limits::default(), echo(), None).unwrap();
    let mut client = Client::connect(&name, SECOND).unwrap();
    assert_eq!(client.server_pid(), std::process::id());
    assert_eq!(client.call(&Request::Hello, SECOND).unwrap(), json!({"app": "test"}));
    assert_eq!(client.call(&search("ext:rs main"), SECOND).unwrap(), json!({"query": "ext:rs main"}));
    match client.call(&Request::FolderSize(PathArgs { path: "x".into() }), SECOND) {
        Err(CallError::Failed(failure)) => assert_eq!((failure.kind, failure.message.as_str()), (FailureKind::NotFound, "no folder_size")),
        other => panic!("{other:?}"),
    }
    // A one-off call connects and hangs up by itself.
    assert_eq!(client::call(&name, &search("a"), SECOND).unwrap(), json!({"query": "a"}));
}

#[test]
fn the_pipe_admits_this_account_alone() {
    let name = unique("acl");
    let _server = Server::start(&name, Limits::default(), echo(), None).unwrap();
    let sid = security::current_user_sid().unwrap();
    // What Windows enforces, read back from the pipe: a protected list (P, nothing inherited)
    // with one entry, full access for this account. No Everyone, no other user, not even
    // SYSTEM or Administrators: another account's program is refused when it opens the pipe.
    assert_eq!(listening_dacl(&name).unwrap(), format!("D:P(A;;FA;;;{sid})"));
}

#[test]
fn a_name_already_taken_is_not_shared() {
    let name = unique("taken");
    let _first = Server::start(&name, Limits::default(), echo(), None).unwrap();
    // A second server (or a program posing as Kestrel) can't join the pipe.
    assert!(Server::start(&name, Limits::default(), echo(), None).is_err());
}

#[test]
fn nothing_listening_means_kestrel_is_not_running() {
    let started = Instant::now();
    assert!(matches!(client::call(&unique("nobody"), &Request::Hello, SECOND), Err(CallError::NotRunning)));
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(matches!(client::call(r"\\server\pipe\x", &Request::Hello, SECOND), Err(CallError::Io(_))), "remote pipes are refused");
}

#[test]
fn requests_and_answers_are_bounded_in_size() {
    let name = unique("sizes");
    let limits = Limits { max_request: 1024, max_response: 10_000, ..Limits::default() };
    let big: Arc<Handler> = Arc::new(|request: Request, _: &Context| match request {
        Request::Search(args) => Ok(Value::String("x".repeat(args.limit.unwrap_or(0)))),
        _ => Ok(Value::Null),
    });
    let _server = Server::start(&name, limits, big, None).unwrap();

    let sized = |n: usize| Request::Search(SearchArgs { query: String::new(), limit: Some(n), ..SearchArgs::default() });
    assert!(client::call(&name, &sized(9_000), SECOND).is_ok());
    match client::call(&name, &sized(20_000), SECOND) {
        Err(CallError::Failed(failure)) => {
            assert_eq!(failure.kind, FailureKind::TooLarge);
            assert!(failure.message.contains("ask for fewer"), "{}", failure.message);
        }
        other => panic!("{other:?}"),
    }

    // A request line longer than allowed is refused, and the connection closed.
    let mut client = Client::connect(&name, SECOND).unwrap();
    let mut line = format!(r#"{{"op":"search","query":"{}"}}"#, "q".repeat(2000)).into_bytes();
    line.push(b'\n');
    client.pipe.write_all(&line, SECOND).unwrap();
    let answer = read_line(&client.pipe, &mut client.pending, 1 << 20, SECOND).unwrap().unwrap();
    let answer: crate::protocol::Response = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer.error.unwrap().kind, FailureKind::TooLarge);
    assert!(read_line(&client.pipe, &mut client.pending, 1 << 20, SECOND).unwrap().is_none(), "the server hung up");
}

#[test]
fn malformed_requests_are_refused_and_writes_are_not_requests() {
    let name = unique("malformed");
    let _server = Server::start(&name, Limits::default(), echo(), None).unwrap();
    let mut client = Client::connect(&name, SECOND).unwrap();
    for line in ["this is not json\n", "{\"op\":\"delete\",\"path\":\"C:\\\\x\"}\n", "{\"op\":\"rename\"}\n", "\n{\"op\":\"hello\"}\n"] {
        client.pipe.write_all(line.as_bytes(), SECOND).unwrap();
        let answer = read_line(&client.pipe, &mut client.pending, 1 << 20, SECOND).unwrap().unwrap();
        let answer: crate::protocol::Response = serde_json::from_slice(&answer).unwrap();
        if line.contains("hello") {
            assert!(answer.ok, "an empty line is skipped, the request after it answered");
        } else {
            assert_eq!(answer.error.unwrap().kind, FailureKind::BadRequest, "{line}");
        }
    }
}

#[test]
fn a_slow_answer_times_out_and_the_handler_is_told_to_stop() {
    let name = unique("slow");
    let noticed = Arc::new(AtomicBool::new(false));
    let seen = noticed.clone();
    let slow: Arc<Handler> = Arc::new(move |_: Request, context: &Context| {
        let started = Instant::now();
        while !context.cancelled.load(Ordering::Relaxed) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        seen.store(context.expired(), Ordering::SeqCst);
        Ok(Value::Null)
    });
    let limits = Limits { handler_timeout: Duration::from_millis(300), ..Limits::default() };
    let _server = Server::start(&name, limits, slow, None).unwrap();
    let started = Instant::now();
    match client::call(&name, &Request::Hello, Duration::from_secs(5)) {
        Err(CallError::Failed(failure)) => assert_eq!(failure.kind, FailureKind::Timeout),
        other => panic!("{other:?}"),
    }
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(300) && took < Duration::from_secs(2), "{took:?}");
    let waited = Instant::now();
    while !noticed.load(Ordering::SeqCst) && waited.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(noticed.load(Ordering::SeqCst), "the handler saw that it was given up on");
}

#[test]
fn a_client_gives_up_on_an_app_that_does_not_answer() {
    let name = unique("hung");
    let hung: Arc<Handler> = Arc::new(|_: Request, _: &Context| {
        std::thread::sleep(Duration::from_secs(2));
        Ok(Value::Null)
    });
    let _server = Server::start(&name, Limits::default(), hung, None).unwrap();
    let started = Instant::now();
    assert!(matches!(client::call(&name, &Request::Hello, Duration::from_millis(300)), Err(CallError::TimedOut)));
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}

#[test]
fn a_client_that_says_nothing_is_let_go() {
    let name = unique("silent");
    let limits = Limits { read_timeout: Duration::from_millis(300), ..Limits::default() };
    let _server = Server::start(&name, limits, echo(), None).unwrap();
    let mut client = Client::connect(&name, SECOND).unwrap();
    let started = Instant::now();
    // Waiting to read shows when the server hangs up.
    assert!(read_line(&client.pipe, &mut client.pending, 1024, Duration::from_secs(5)).unwrap().is_none());
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn connections_are_bounded_and_freed() {
    let name = unique("crowd");
    let limits = Limits { max_clients: 2, ..Limits::default() };
    let _server = Server::start(&name, limits, echo(), None).unwrap();
    let mut first = Client::connect(&name, SECOND).unwrap();
    let second = Client::connect(&name, SECOND).unwrap();
    // Both connections are taken: a third waits, then gives up.
    let started = Instant::now();
    assert!(matches!(Client::connect(&name, Duration::from_millis(400)), Err(CallError::Busy)));
    assert!(started.elapsed() >= Duration::from_millis(350));
    assert!(first.call(&Request::Hello, SECOND).is_ok(), "the connections held still work");
    drop(second);
    // Once one hangs up, the next is let in.
    let mut third = Client::connect(&name, Duration::from_secs(3)).unwrap();
    assert!(third.call(&Request::Hello, SECOND).is_ok());
}

#[test]
fn handlers_running_at_once_are_bounded() {
    let name = unique("handlers");
    let limits = Limits { max_handlers: 1, ..Limits::default() };
    let slow: Arc<Handler> = Arc::new(|_: Request, _: &Context| {
        std::thread::sleep(Duration::from_millis(600));
        Ok(json!("done"))
    });
    let _server = Server::start(&name, limits, slow, None).unwrap();
    let busy_name = name.clone();
    let first = std::thread::spawn(move || client::call(&busy_name, &Request::Hello, Duration::from_secs(5)));
    std::thread::sleep(Duration::from_millis(200));
    match client::call(&name, &Request::Hello, Duration::from_secs(5)) {
        Err(CallError::Failed(failure)) => assert_eq!(failure.kind, FailureKind::Busy),
        other => panic!("{other:?}"),
    }
    assert_eq!(first.join().unwrap().unwrap(), json!("done"));
    assert_eq!(client::call(&name, &Request::Hello, Duration::from_secs(5)).unwrap(), json!("done"), "free again");
}

#[test]
fn the_observer_hears_who_asked_what() {
    let name = unique("observed");
    let heard: Arc<Mutex<Vec<(String, String, bool)>>> = Arc::default();
    let log = heard.clone();
    let observer: Arc<crate::server::Observer> = Arc::new(move |request: &Request, context: &Context, outcome: Result<(), &Failure>, _| {
        log.lock().unwrap().push((request.op().to_owned(), context.client_name.clone(), outcome.is_ok()));
    });
    let _server = Server::start(&name, Limits::default(), echo(), Some(observer)).unwrap();
    client::call(&name, &Request::Hello, SECOND).unwrap();
    let _ = client::call(&name, &Request::FolderSize(PathArgs { path: "x".into() }), SECOND);
    let heard = heard.lock().unwrap().clone();
    let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(heard, [("hello".to_owned(), exe.clone(), true), ("folder_size".to_owned(), exe, false)]);
}

#[test]
fn a_stopped_server_is_gone() {
    let name = unique("stopped");
    let mut server = Server::start(&name, Limits::default(), echo(), None).unwrap();
    assert!(client::call(&name, &Request::Hello, SECOND).is_ok());
    server.stop();
    // The listening instance is closed with the accepting thread; give Windows a moment.
    std::thread::sleep(Duration::from_millis(100));
    assert!(matches!(client::call(&name, &Request::Hello, SECOND), Err(CallError::NotRunning)));
    drop(server);
    // The name is free again.
    assert!(Server::start(&name, Limits::default(), echo(), None).is_ok());
}
