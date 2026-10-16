//! The downloader against a web server on this computer that each test starts itself, serving
//! a few hundred kilobytes; nothing here ever contacts another host.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// How the test server misbehaves.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    /// The first answer stops after this many bytes, as a dropped connection does.
    CutFirstAfter(usize),
    /// Every answer is the whole file, whatever was asked.
    IgnoreRange,
    /// Sends 16 KB every 50 ms, so a cancel lands mid-way.
    Slow,
    /// Declares and sends a file of another size.
    WrongSize,
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    served: Arc<AtomicUsize>,
}

fn serve(body: Vec<u8>, mode: Mode) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/onnx/model.onnx", listener.local_addr().unwrap().port());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let served = Arc::new(AtomicUsize::new(0));
    let (seen, count) = (requests.clone(), served.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let n = count.fetch_add(1, Ordering::SeqCst);
            let _ = answer(stream, &body, mode, n, &seen);
        }
    });
    Server { url, requests, served }
}

fn answer(mut stream: TcpStream, body: &[u8], mode: Mode, n: usize, seen: &Mutex<Vec<String>>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut range_from = None;
    let mut first = String::new();
    reader.read_line(&mut first)?;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range_from = value.trim().trim_end_matches('-').parse::<usize>().ok();
        }
    }
    seen.lock().push(format!("{} range={range_from:?}", first.trim()));
    let body: Vec<u8> = if mode == Mode::WrongSize { body[..body.len() - 10].to_vec() } else { body.to_vec() };
    let from = if mode == Mode::IgnoreRange { None } else { range_from };
    let head = match from {
        Some(from) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\nConnection: close\r\n\r\n",
            body.len() - from,
            body.len() - 1,
            body.len()
        ),
        None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()),
    };
    stream.write_all(head.as_bytes())?;
    let rest = &body[from.unwrap_or(0)..];
    match mode {
        Mode::CutFirstAfter(limit) if n == 0 => stream.write_all(&rest[..limit])?,
        Mode::Slow => {
            for piece in rest.chunks(16 * 1024) {
                stream.write_all(piece)?;
                stream.flush()?;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        _ => stream.write_all(rest)?,
    }
    stream.flush()?;
    Ok(())
}

fn body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8 ^ (i >> 9) as u8).collect()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kestrel-embed-download-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn target(server: &Server, dir: &Path, bytes: &[u8]) -> Target {
    Target { url: server.url.clone(), dest: dir.join("onnx").join("model.onnx"), size: bytes.len() as u64, sha256: sha(bytes) }
}

#[test]
fn a_file_arrives_with_progress_and_is_verified() {
    let bytes = body(700_000);
    let server = serve(bytes.clone(), Mode::Normal);
    let dir = scratch("whole");
    let target = target(&server, &dir, &bytes);
    let mut seen = Vec::new();
    fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |done, total| seen.push((done, total))).unwrap();
    assert_eq!(std::fs::read(&target.dest).unwrap(), bytes);
    assert!(!part_path(&target.dest).exists());
    assert!(seen.len() > 2, "progress as it went: {seen:?}");
    assert!(seen.windows(2).all(|pair| pair[0].0 <= pair[1].0), "never backwards");
    assert_eq!(*seen.last().unwrap(), (700_000, 700_000));
    // Asking again finds the file there and verified, without a request.
    let before = server.served.load(Ordering::SeqCst);
    fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| {}).unwrap();
    assert_eq!(server.served.load(Ordering::SeqCst), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// On a slow line, progress (and what a cancel would keep) moves as bytes arrive, not a whole
/// 256 KB buffer at a time.
#[test]
fn progress_moves_as_bytes_arrive() {
    let bytes = body(100_000);
    let server = serve(bytes.clone(), Mode::Slow);
    let dir = scratch("trickle");
    let target = target(&server, &dir, &bytes);
    let mut seen = Vec::new();
    fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |done, _| seen.push(done)).unwrap();
    let partial: Vec<u64> = seen.iter().copied().filter(|&done| done > 0 && done < 100_000).collect();
    assert!(partial.len() >= 4, "{seen:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_interrupted_download_resumes_where_it_stopped() {
    let bytes = body(500_000);
    let server = serve(bytes.clone(), Mode::CutFirstAfter(123_456));
    let dir = scratch("resume");
    let target = target(&server, &dir, &bytes);
    let failure = fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| {}).unwrap_err();
    assert!(matches!(&failure, Failure::Network(why) if why.contains("123456 of 500000")), "{failure:?}");
    assert_eq!(std::fs::metadata(part_path(&target.dest)).unwrap().len(), 123_456, "what arrived is kept");
    let mut first_progress = None;
    fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |done, _| {
        first_progress.get_or_insert(done);
    })
    .unwrap();
    assert_eq!(first_progress, Some(123_456), "it starts from what was there");
    assert_eq!(std::fs::read(&target.dest).unwrap(), bytes, "and the hash covers the whole file");
    let requests = server.requests.lock().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].ends_with("range=None"), "{requests:?}");
    assert!(requests[1].ends_with("range=Some(123456)"), "{requests:?}");
    assert!(requests[1].starts_with("GET /onnx/model.onnx "), "{requests:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_server_that_ignores_the_range_starts_the_file_again() {
    let bytes = body(300_000);
    let server = serve(bytes.clone(), Mode::IgnoreRange);
    let dir = scratch("ignore");
    let target = target(&server, &dir, &bytes);
    std::fs::create_dir_all(target.dest.parent().unwrap()).unwrap();
    std::fs::write(part_path(&target.dest), &bytes[..1000]).unwrap();
    fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(&target.dest).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_wrong_checksum_deletes_the_file_and_says_so() {
    let bytes = body(200_000);
    let server = serve(bytes.clone(), Mode::Normal);
    let dir = scratch("checksum");
    let mut target = target(&server, &dir, &bytes);
    target.sha256 = sha(b"something else");
    let failure = fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| {}).unwrap_err();
    match &failure {
        Failure::Checksum { expected, got } => {
            assert_eq!(expected, &target.sha256);
            assert_eq!(got, &sha(&bytes));
        }
        other => panic!("{other:?}"),
    }
    assert!(failure.to_string().contains("deleted"), "{failure}");
    assert!(!target.dest.exists() && !part_path(&target.dest).exists(), "nothing damaged is kept");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_server_offering_another_size_is_refused_before_downloading_it() {
    let bytes = body(200_000);
    let server = serve(bytes.clone(), Mode::WrongSize);
    let dir = scratch("size");
    let target = target(&server, &dir, &bytes);
    let failure = fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| {}).unwrap_err();
    assert!(matches!(&failure, Failure::Network(why) if why.contains("199990")), "{failure:?}");
    assert_eq!(std::fs::metadata(part_path(&target.dest)).map(|m| m.len()).unwrap_or(0), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancelling_stops_at_once_and_keeps_what_arrived() {
    let bytes = body(1_000_000);
    let server = serve(bytes.clone(), Mode::Slow);
    let dir = scratch("cancel");
    let target = target(&server, &dir, &bytes);
    let cancel = Arc::new(Cancel::default());
    let canceller = cancel.clone();
    let started = Instant::now();
    let mut cancelled_at = None;
    let failure = fetch(&target, &cancel, Policy::LoopbackOnly, &mut |done, _| {
        if done >= 64 * 1024 && cancelled_at.is_none() {
            cancelled_at = Some(Instant::now());
            // From another thread, as the app's Cancel button does.
            let canceller = canceller.clone();
            std::thread::spawn(move || canceller.cancel()).join().unwrap();
        }
    })
    .unwrap_err();
    assert_eq!(failure, Failure::Cancelled);
    assert!(cancelled_at.unwrap().elapsed() < Duration::from_secs(1), "stopped promptly");
    assert!(started.elapsed() < Duration::from_secs(5));
    let kept = std::fs::metadata(part_path(&target.dest)).unwrap().len();
    assert!((64 * 1024..1_000_000).contains(&kept), "{kept}");
    // The same file can be fetched again, resuming.
    cancel.reset();
    fetch(&target, &cancel, Policy::LoopbackOnly, &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(&target.dest).unwrap(), bytes);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Test instances download only from this computer; the refusal comes before any connection.
#[test]
fn loopback_only_refuses_other_hosts_without_connecting() {
    let dir = scratch("refuse");
    let target = Target {
        url: "https://huggingface.co/onnx-community/x/resolve/abc/onnx/vision_model_fp16.onnx".into(),
        dest: dir.join("vision.onnx"),
        size: 10,
        sha256: "00".repeat(32),
    };
    let failure = fetch(&target, &Cancel::default(), Policy::LoopbackOnly, &mut |_, _| panic!("no progress")).unwrap_err();
    assert!(matches!(failure, Failure::Refused(_)), "{failure:?}");
    assert!(!part_path(&target.dest).exists(), "not even a file was made");
    assert!(is_loopback_url("http://localhost:8080/x") && is_loopback_url("http://[::1]:9/x") && is_loopback_url("http://127.0.0.1/"));
    assert!(!is_loopback_url("https://huggingface.co/") && !is_loopback_url("http://127.0.0.1.example.com/"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn addresses_are_split_into_their_parts() {
    assert_eq!(
        parse_url("https://huggingface.co/a/b?x=1"),
        Some(Url { secure: true, host: "huggingface.co".into(), port: 443, path: "/a/b?x=1".into() })
    );
    assert_eq!(parse_url("http://127.0.0.1:8123"), Some(Url { secure: false, host: "127.0.0.1".into(), port: 8123, path: "/".into() }));
    assert_eq!(parse_url("http://[::1]:80/m").map(|u| u.host), Some("[::1]".into()));
    assert_eq!(parse_url("ftp://x/"), None);
    assert_eq!(parse_url("http://user@host/"), None);
}
