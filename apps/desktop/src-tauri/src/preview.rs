//! The preview pane's backend.
//!
//! - The `preview` protocol (`http://preview.localhost/?p=<path>`) streams a file's bytes to
//!   the pane's `<img>`, `<video>`, `<audio>` and PDF viewer. It honours `Range`, which video
//!   seeking and the PDF viewer rely on, and never holds more of a file in memory than one
//!   [`MAX_CHUNK`], or [`MAX_WHOLE`] for a request without a range. An online-only file, whose
//!   reading downloads it, is served only to a request marked `download=1`, which the pane
//!   adds once the user has asked for that item; images inside a document never are.
//! - [`preview_text`] reads the start of a file as text, [`preview_details`] describes a file
//!   or folder, and [`open_link`] opens a link from a rendered Markdown file in the browser.
//!
//! Security. Tauri counts every custom protocol, `preview.localhost` included, as part of the
//! app, and on Windows the web view injects Tauri's IPC bridge, invoke key and all, into every
//! frame. A document served by this protocol that could run script could therefore call
//! commands. Four layers keep that from happening:
//! 1. Images, video, audio and PDF documents are shown in a webview of their own
//!    (`previewview.rs`), which has no capability at all, so neither the app's commands nor
//!    Tauri's plugin commands (window, event, app, webview) answer anything in it.
//! 2. Only regular files are served, each with a media type fixed by its extension. Anything
//!    that is not an image, audio, video or PDF is served as `text/plain`, so HTML and other
//!    active content are never rendered as documents. SVG is served as an image and only ever
//!    shown through `<img>`, where scripts never run.
//! 3. Every response except a PDF carries `Content-Security-Policy: sandbox`, so even if one
//!    were opened as a document it would get an opaque origin and run no script, and Tauri
//!    rejects IPC from an opaque origin outright. So does [`viewer`], the page the preview's
//!    webview shows media in.
//! 4. [`guard`] refuses app commands to every webview but the app's own, and to every frame in
//!    it whose origin is not the app's own.
//!
//! A PDF is the one document from this protocol that is not sandboxed: Chromium will not show
//! a PDF in a sandboxed frame and ignores CSP on the viewer's document. Its content cannot run
//! script in that document (a PDF's own scripts run inside the viewer), and the webview it is
//! shown in can call nothing.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::null;
use std::sync::Arc;

use kestrel_index::walk;
use serde::Serialize;
use tauri::http::header::{
    ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_SECURITY_POLICY,
    CONTENT_TYPE, ETAG, IF_RANGE, ORIGIN, RANGE, X_CONTENT_TYPE_OPTIONS,
};
use tauri::http::{Method, Request, Response, StatusCode, Uri};
use tauri::ipc::Invoke;
use tauri::{Runtime, State, UriSchemeContext, UriSchemeResponder, Url};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_TYPE_DISK, GetFileAttributesExW, GetFileExInfoStandard, GetFileType,
    WIN32_FILE_ATTRIBUTE_DATA,
};
use windows_sys::Win32::UI::Shell::{
    ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYDOCNAME, AssocQueryStringW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::engine::Engine;

/// The most of a file read for a text preview.
pub const TEXT_LIMIT: usize = 1 << 20;
/// The most bytes sent for one range request. A media element or the PDF viewer that wants
/// more simply asks again from where the last part ended.
const MAX_CHUNK: u64 = 4 << 20;
/// Files up to this size are sent whole to a request without a range, such as an `<img>` or
/// the PDF viewer, which reads a document in one request. A larger file is refused to such a
/// request; media elements, which always ask for ranges, can still play it.
pub const MAX_WHOLE: u64 = 64 << 20;
/// How far into an image its header is searched for the image's size. JPEG files can carry a
/// large thumbnail before the frame header.
const HEADER_PROBE: u64 = 256 << 10;
/// How much of a file is checked for NUL bytes, which text files do not contain.
const BINARY_PROBE: usize = 8 << 10;
/// FILETIME ticks at the Unix epoch.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// Applied to everything served except PDF: a document opened from it gets an opaque origin,
/// runs no script and loads nothing from elsewhere.
const SANDBOX_POLICY: &str =
    "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'";

// ---------- the protocol ----------

/// The `preview` protocol handler: answers each request on a blocking thread, since it reads
/// the disk.
pub fn protocol<R: Runtime>(
    context: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    // The window whose preview asks, so an item from an archive it no longer wants gives way.
    let window = crate::parity::appwindows::owner_of(context.webview_label());
    tauri::async_runtime::spawn_blocking(move || {
        let response = serve_for(&request, &window);
        #[cfg(debug_assertions)]
        eprintln!(
            "preview: {} {} range={:?} -> {} {:?}",
            request.method(),
            requested_path(request.uri()).unwrap_or_default(),
            request.headers().get(RANGE),
            response.status().as_u16(),
            response.headers().get(CONTENT_RANGE),
        );
        responder.respond(response);
    });
}

/// A `Range` header, resolved against the length of the file.
#[derive(Debug, PartialEq, Eq)]
pub enum ByteRange {
    /// No usable range: send the whole file.
    Whole,
    /// Bytes `start..=end`.
    Part(u64, u64),
    /// Every range lies beyond the end of the file.
    Unsatisfiable,
}

/// Parses a `Range` header for a file of `len` bytes. A header that is malformed or names
/// another unit is ignored, as RFC 9110 asks. Several ranges are coalesced into the one span
/// covering them, which the RFC allows and which saves a multipart body.
pub fn parse_range(header: &str, len: u64) -> ByteRange {
    let header = header.trim();
    let Some(spec) = header.get(..6).filter(|unit| unit.eq_ignore_ascii_case("bytes=")).map(|_| &header[6..])
    else {
        return ByteRange::Whole;
    };
    let mut span: Option<(u64, u64)> = None;
    let mut any = false;
    for part in spec.split(',').map(str::trim).filter(|part| !part.is_empty()) {
        let Some((first, last)) = part.split_once('-') else { return ByteRange::Whole };
        let (first, last) = (first.trim(), last.trim());
        let range = if first.is_empty() {
            // `-n`: the last n bytes.
            let Ok(count) = last.parse::<u64>() else { return ByteRange::Whole };
            (count > 0 && len > 0).then(|| (len.saturating_sub(count), len - 1))
        } else {
            let Ok(start) = first.parse::<u64>() else { return ByteRange::Whole };
            let end = match last {
                "" => None,
                last => match last.parse::<u64>() {
                    Ok(end) if end >= start => Some(end),
                    _ => return ByteRange::Whole,
                },
            };
            (start < len).then(|| (start, end.map_or(len - 1, |end| end.min(len - 1))))
        };
        any = true;
        if let Some((start, end)) = range {
            span = Some(span.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
        }
    }
    match span {
        Some((start, end)) => ByteRange::Part(start, end),
        None if any => ByteRange::Unsatisfiable,
        None => ByteRange::Whole,
    }
}

/// The media type a file is served with, and the policy that goes with it.
fn media_type(path: &Path) -> (&'static str, Option<&'static str>) {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "apng" => "image/apng",
        "jpg" | "jpeg" | "jfif" | "pjpeg" | "pjp" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" | "cur" => "image/x-icon",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "weba" => "audio/webm",
        // No policy: the viewer's document is synthesised by the browser, which applies no CSP
        // from this response to it (neither `sandbox` nor `connect-src` takes effect).
        "pdf" => return ("application/pdf", None),
        // HTML, XML, scripts and everything unknown: shown as text, never run.
        _ => "text/plain",
    };
    (mime, Some(SANDBOX_POLICY))
}

/// The path named by a request's `p` parameter.
fn requested_path(uri: &Uri) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| match pair.split_once('=') {
        Some(("p", value)) => Some(percent_decode(value)),
        _ => None,
    })
}

/// Whether a request carries `download=1`: the user agreed to fetch this item from the cloud
/// if it is online only. The pane adds it to the previewed item's own URL after "Download and
/// preview"; nothing else does, so an image a Markdown document refers to never downloads.
fn download_agreed(uri: &Uri) -> bool {
    uri.query().unwrap_or_default().split('&').any(|pair| pair == "download=1")
}

/// Whether reading a file's data would fetch it from elsewhere, judged from its directory
/// entry alone, which never does. The path is first made whole the way opening it would be,
/// so that another spelling of it (`photo.png.`) is judged as the file it opens.
fn is_online_only(path: &str) -> bool {
    let whole = std::path::absolute(path).ok().and_then(|whole| whole.to_str().map(str::to_owned));
    attribute_data(whole.as_deref().unwrap_or(path)).is_ok_and(|data| data.dwFileAttributes & RECALL != 0)
}

/// Whether reading an opened file's data would fetch it from elsewhere. Asking the handle for
/// its attributes reads none of the data.
fn is_opened_online_only(file: &File) -> bool {
    file.metadata().is_ok_and(|meta| meta.file_attributes() & RECALL != 0)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a path names a file on a drive or share, as opposed to a device or a path in the
/// object manager's namespace (`\\.\`, `\\?\`).
fn is_plain_path(path: &str) -> bool {
    !path.contains('\0')
        && !path.starts_with(r"\\.\")
        && !path.starts_with(r"\\?\")
        && !path.starts_with("//")
        && Path::new(path).is_absolute()
}

/// Opens a regular file for serving, mapping failures to the status to answer with.
fn open_regular(path: &str) -> Result<(File, u64, String), StatusCode> {
    if !is_plain_path(path) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Opening a folder fails on Windows (it needs backup semantics), which is one reason a
    // folder is never served; the file-type check below is the other.
    let file = File::open(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
        _ => StatusCode::FORBIDDEN,
    })?;
    let meta = file.metadata().map_err(|_| StatusCode::FORBIDDEN)?;
    // SAFETY: the handle belongs to `file`, which is alive for the call.
    let on_disk = unsafe { GetFileType(file.as_raw_handle()) } == FILE_TYPE_DISK;
    if !meta.is_file() || !on_disk {
        return Err(StatusCode::FORBIDDEN);
    }
    // Identifies this version of the file, so a resumed download notices a change.
    let etag = format!("\"{:x}-{:x}\"", meta.last_write_time(), meta.len());
    Ok((file, meta.len(), etag))
}

fn status_only(status: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    if status == StatusCode::METHOD_NOT_ALLOWED {
        response.headers_mut().insert(ALLOW, "GET, HEAD".parse().expect("a static header value is valid"));
    }
    response
}

/// Reads `count` bytes from `start`, or fewer if the file ends first.
fn read_span(file: &mut File, start: u64, count: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(start))?;
    let mut body = Vec::with_capacity(count as usize);
    file.take(count).read_to_end(&mut body)?;
    Ok(body)
}

/// The policy of the viewer page: no script, and nothing loaded but the one file it shows.
const VIEWER_POLICY: &str = "sandbox; default-src 'none'; img-src http://preview.localhost; \
     media-src http://preview.localhost; style-src 'unsafe-inline'";

/// Percent-encodes everything but unreserved characters, so the result is safe in a query,
/// and in HTML, as it stands.
fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// A colour given as `#rrggbb`, the only form the viewer takes from its address.
fn hex_colour(text: &str) -> Option<&str> {
    let digits = text.strip_prefix('#')?;
    (digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit())).then_some(text)
}

/// The page the preview's own webview (`previewview.rs`) shows an image, a video or a sound
/// in: the file, centred, on the app's colours, which the address carries as `bg`, `raised`,
/// `line` and `chrome`. The page has no script and its policy forbids any; it names the file
/// again on this protocol, so everything `serve` checks still applies to the file itself.
pub fn viewer(uri: &Uri) -> Response<Vec<u8>> {
    let query = uri.query().unwrap_or_default();
    let param = |name: &str| {
        query.split('&').find_map(|pair| pair.split_once('=').filter(|(key, _)| *key == name).map(|(_, value)| value))
    };
    let Some(path) = requested_path(uri) else { return status_only(StatusCode::BAD_REQUEST) };
    let version: i64 = param("v").and_then(|v| v.parse().ok()).unwrap_or(0);
    let download = if download_agreed(uri) { "&amp;download=1" } else { "" };
    let src = format!("/?p={}&amp;v={version}{download}", encode_component(&path));
    let colour = |name: &str, fallback: &'static str| {
        param(name).map(percent_decode).filter(|value| hex_colour(value).is_some()).unwrap_or_else(|| fallback.to_owned())
    };
    // The dark theme's values from styles.css, for an address without colours.
    let (bg, raised, line, chrome) =
        (colour("bg", "#1b2129"), colour("raised", "#242c37"), colour("line", "#2c3541"), colour("chrome", "#151a21"));
    let scheme = if param("scheme") == Some("light") { "light" } else { "dark" };
    let (element, background) = match param("kind") {
        Some("image") => (format!(r#"<img src="{src}" alt="">"#), &bg),
        Some("video") => (format!(r#"<video src="{src}" controls preload="metadata"></video>"#), &chrome),
        Some("audio") => (format!(r#"<audio src="{src}" controls preload="metadata"></audio>"#), &bg),
        _ => return status_only(StatusCode::BAD_REQUEST),
    };
    let page = format!(
        r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="color-scheme" content="{scheme}">
<title>Preview</title>
<style>
html, body {{ width: 100%; height: 100%; margin: 0; overflow: hidden; background: {background}; }}
body {{ display: flex; align-items: center; justify-content: center; }}
img {{ max-width: calc(100% - 2px); max-height: calc(100% - 2px); background: repeating-conic-gradient({raised} 0 25%, {bg} 0 50%) 0 0 / 16px 16px; box-shadow: 0 0 0 1px {line}; }}
video {{ width: 100%; height: 100%; object-fit: contain; background: {chrome}; }}
audio {{ width: 100%; }}
</style>
{element}
"#
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/html; charset=utf-8")
        .header(CONTENT_SECURITY_POLICY, VIEWER_POLICY)
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(CACHE_CONTROL, "no-store")
        .body(page.into_bytes())
        .unwrap_or_else(|_| status_only(StatusCode::INTERNAL_SERVER_ERROR))
}

/// Answers one request to the `preview` protocol.
#[cfg(test)]
pub fn serve(request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    serve_for(request, crate::parity::appwindows::MAIN)
}

/// Answers one request to the `preview` protocol from the preview of the app window `window`.
pub fn serve_for(request: &Request<Vec<u8>>, window: &str) -> Response<Vec<u8>> {
    let head = match *request.method() {
        Method::GET => false,
        Method::HEAD => true,
        _ => return status_only(StatusCode::METHOD_NOT_ALLOWED),
    };
    if request.uri().path() == "/view" {
        return viewer(request.uri());
    }
    let Some(path) = requested_path(request.uri()) else { return status_only(StatusCode::BAD_REQUEST) };
    // A file inside an archive is served from its copy in the cache (archive::browse).
    let path = match crate::archive::preview_file(&path, window) {
        Some(Ok(copy)) => copy,
        Some(Err(_)) => return status_only(StatusCode::NOT_FOUND),
        None => path,
    };
    // Checked before the file is opened, since opening some online-only files downloads them.
    let agreed = download_agreed(request.uri());
    if is_plain_path(&path) && !agreed && is_online_only(&path) {
        return status_only(StatusCode::FORBIDDEN);
    }
    let (mut file, len, etag) = match open_regular(&path) {
        Ok(opened) => opened,
        Err(status) => return status_only(status),
    };
    // And again on the file as opened, before anything is read from it: a link to an
    // online-only file passes the check above with the link's own attributes.
    if !agreed && is_opened_online_only(&file) {
        return status_only(StatusCode::FORBIDDEN);
    }

    let header = |name| request.headers().get(name).and_then(|value| value.to_str().ok());
    // A range only applies to the version of the file the client saw before.
    let still_current = header(IF_RANGE).is_none_or(|tag| tag == etag);
    let range = match header(RANGE) {
        Some(range) if still_current => parse_range(range, len),
        _ => ByteRange::Whole,
    };
    let (start, count, partial) = match range {
        ByteRange::Whole if head || len <= MAX_WHOLE => (0, len, false),
        ByteRange::Whole => {
            let mut response = status_only(StatusCode::PAYLOAD_TOO_LARGE);
            response.headers_mut().insert(ACCEPT_RANGES, "bytes".parse().expect("a static header value is valid"));
            return response;
        }
        ByteRange::Part(start, end) => (start, (end - start + 1).min(MAX_CHUNK), true),
        ByteRange::Unsatisfiable => {
            let mut response = status_only(StatusCode::RANGE_NOT_SATISFIABLE);
            let unsatisfied = format!("bytes */{len}").parse().expect("a formatted number is a valid header");
            response.headers_mut().insert(CONTENT_RANGE, unsatisfied);
            return response;
        }
    };

    let body = if head {
        Vec::new()
    } else {
        match read_span(&mut file, start, count) {
            Ok(body) => body,
            Err(_) => return status_only(StatusCode::INTERNAL_SERVER_ERROR),
        }
    };
    // The file may have shrunk since it was measured.
    let sent = if head { count } else { body.len() as u64 };
    if partial && sent == 0 {
        return status_only(StatusCode::RANGE_NOT_SATISFIABLE);
    }

    let (mime, policy) = media_type(Path::new(&path));
    let mut response = Response::builder()
        .status(if partial { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(CONTENT_TYPE, mime)
        .header(CONTENT_LENGTH, sent)
        .header(ACCEPT_RANGES, "bytes")
        .header(ETAG, &etag)
        .header(CACHE_CONTROL, "no-store")
        .header(X_CONTENT_TYPE_OPTIONS, "nosniff");
    if partial {
        response = response.header(CONTENT_RANGE, format!("bytes {start}-{}/{len}", start + sent - 1));
    }
    if let Some(policy) = policy {
        response = response.header(CONTENT_SECURITY_POLICY, policy);
    }
    response.body(body).unwrap_or_else(|_| status_only(StatusCode::INTERNAL_SERVER_ERROR))
}

// ---------- commands ----------

/// The label of the main window's own webview, where the app's page runs.
pub const APP_WEBVIEW: &str = "main";
/// The webviews that show the app's own pages, and the only ones whose pages may call the
/// app's commands: the main window's and the quick launcher's (`launcher.rs`), and those of
/// the windows Ctrl+N opens (`main-2`...; `parity::appwindows`).
pub const APP_WEBVIEWS: [&str; 2] = [APP_WEBVIEW, crate::launcher::QUICK];

/// Whether the webview called `label` shows one of the app's own pages.
pub fn is_app_webview(label: &str) -> bool {
    APP_WEBVIEWS.contains(&label) || crate::parity::appwindows::is_app_window(label)
}

/// Refuses app commands to every webview but the app's own (the preview's webview above all,
/// see `previewview.rs`), and to any frame in them whose origin is not the app's own.
///
/// Tauri lets every local page call the app's commands, and counts `preview.localhost` as
/// local, so the label is what tells the preview's webview apart. Requests over Tauri's IPC
/// protocol always carry the browser-set `Origin` of the frame that made them; the fallback
/// channel is open to the top-level document only and carries none.
pub fn guard<R: Runtime>(
    handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke: Invoke<R>| {
        let label = invoke.message.webview_ref().label().to_owned();
        if !is_app_webview(&label) {
            invoke.resolver.reject(format!("Commands are not available to the {label} view"));
            return true;
        }
        let origin = invoke.message.headers().get(ORIGIN).and_then(|value| value.to_str().ok()).map(str::to_owned);
        if let Some(origin) = origin {
            let app = invoke.message.webview_ref().url().ok();
            if !app.is_some_and(|app| same_origin(&origin, &app)) {
                invoke.resolver.reject(format!("Commands are not available to {origin}"));
                return true;
            }
        }
        handler(invoke)
    }
}

/// Whether an `Origin` header names the origin of `app`. Opaque origins (`null`) never do.
pub fn same_origin(origin: &str, app: &Url) -> bool {
    Url::parse(origin).is_ok_and(|origin| origin.origin().is_tuple() && origin.origin() == app.origin())
}

/// The start of a file as text.
#[derive(Debug, Serialize)]
pub struct TextPreview {
    pub text: String,
    /// Only the first [`TEXT_LIMIT`] bytes were read.
    pub truncated: bool,
    /// `utf-8`, `utf-16le` or `utf-16be`.
    pub encoding: &'static str,
    /// Some bytes were not valid in the encoding and were replaced.
    pub lossy: bool,
    /// The file looks binary; `text` is then empty.
    pub binary: bool,
    pub size: u64,
}

/// Whether the start of a file looks like something other than text: it has no byte-order
/// mark, and a NUL byte, which text without one never contains.
fn looks_binary(bytes: &[u8]) -> bool {
    let marked = [&[0xEF, 0xBB, 0xBF][..], &[0xFF, 0xFE], &[0xFE, 0xFF]].iter().any(|bom| bytes.starts_with(bom));
    !marked && bytes[..bytes.len().min(BINARY_PROBE)].contains(&0)
}

/// Decodes the start of a file: UTF-8 or UTF-16 when it has a byte-order mark, otherwise
/// UTF-8 with invalid sequences replaced. `truncated` says the bytes stop at the read limit,
/// so a character cut in two at the end is dropped rather than replaced.
pub fn decode_text(bytes: &[u8], truncated: bool, size: u64) -> TextPreview {
    let preview = |text: String, encoding, lossy| TextPreview { text, truncated, encoding, lossy, binary: false, size };
    if looks_binary(bytes) {
        return TextPreview { binary: true, ..preview(String::new(), "utf-8", false) };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        let (text, lossy) = decode_utf8(rest, truncated);
        return preview(text, "utf-8", lossy);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let (text, lossy) = decode_utf16(rest, truncated, u16::from_le_bytes);
        return preview(text, "utf-16le", lossy);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let (text, lossy) = decode_utf16(rest, truncated, u16::from_be_bytes);
        return preview(text, "utf-16be", lossy);
    }
    let (text, lossy) = decode_utf8(bytes, truncated);
    preview(text, "utf-8", lossy)
}

fn decode_utf8(bytes: &[u8], truncated: bool) -> (String, bool) {
    let bytes = if truncated { without_partial_char(bytes) } else { bytes };
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), true),
    }
}

/// Drops an incomplete UTF-8 sequence from the end of `bytes`.
fn without_partial_char(bytes: &[u8]) -> &[u8] {
    let tail = bytes.len().saturating_sub(4);
    for at in (tail..bytes.len()).rev() {
        let byte = bytes[at];
        // Continuation bytes look like 10xxxxxx; anything else starts a character.
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let needed = match byte {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => 1,
        };
        return if bytes.len() - at < needed { &bytes[..at] } else { bytes };
    }
    bytes
}

fn decode_utf16(bytes: &[u8], truncated: bool, unit: fn([u8; 2]) -> u16) -> (String, bool) {
    let mut units: Vec<u16> = bytes.chunks_exact(2).map(|pair| unit([pair[0], pair[1]])).collect();
    // A read cut short can end between the halves of a surrogate pair.
    if truncated && units.last().is_some_and(|last| (0xD800..0xDC00).contains(last)) {
        units.pop();
    }
    let mut lossy = !truncated && bytes.len() % 2 == 1;
    let text = char::decode_utf16(units)
        .map(|unit| {
            unit.unwrap_or_else(|_| {
                lossy = true;
                char::REPLACEMENT_CHARACTER
            })
        })
        .collect();
    (text, lossy)
}

/// Reads at most the first [`TEXT_LIMIT`] bytes of a file as text.
pub fn read_text(path: &str) -> io::Result<TextPreview> {
    let (file, len, _) = open_regular(path).map_err(|status| match status {
        StatusCode::NOT_FOUND => io::Error::from(io::ErrorKind::NotFound),
        // Opening fails like this both for lack of permission and for a folder, which the
        // pane never asks to read as text.
        StatusCode::FORBIDDEN => io::Error::from(io::ErrorKind::PermissionDenied),
        _ => io::Error::other("Only files on a drive or share can be previewed."),
    })?;
    let mut file = file;
    let mut bytes = Vec::with_capacity(len.min(TEXT_LIMIT as u64 + 1) as usize);
    // A binary file is recognised from its start, so the rest is not read.
    (&mut file).take(BINARY_PROBE as u64).read_to_end(&mut bytes)?;
    if looks_binary(&bytes) {
        return Ok(decode_text(&bytes, false, len));
    }
    (&mut file).take(TEXT_LIMIT as u64 + 1 - bytes.len() as u64).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);
    Ok(decode_text(&bytes, truncated, len))
}

/// Why an item can't be previewed, in words for the pane, which already names the item.
fn explain(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "It no longer exists.".to_owned(),
        io::ErrorKind::PermissionDenied => "You don't have permission to open it.".to_owned(),
        _ => error.to_string(),
    }
}

#[tauri::command(async)]
pub fn preview_text(window: tauri::Window, path: String) -> Result<TextPreview, String> {
    let path = match crate::archive::preview_file(&path, window.label()) {
        Some(copy) => copy?,
        None => path,
    };
    read_text(&path).map_err(|error| explain(&error))
}

/// What the details block shows about a file or folder.
#[derive(Debug, Serialize)]
pub struct Details {
    pub name: String,
    pub path: String,
    pub dir: bool,
    /// Bytes; for a folder, the total from the index, if the index has it.
    pub size: Option<u64>,
    /// Milliseconds since the Unix epoch, or 0 when unknown.
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub attributes: Vec<&'static str>,
    /// The file's contents are in the cloud; reading them would download it.
    pub online_only: bool,
    /// The shell's name for the type, such as "Text Document".
    pub type_name: String,
    /// Pixel size, read from an image's header.
    pub dimensions: Option<(u32, u32)>,
    pub folder: Option<FolderSummary>,
}

#[derive(Debug, Serialize)]
pub struct FolderSummary {
    pub files: u32,
    pub folders: u32,
    /// The folder could be listed; when not, the counts are zero.
    pub readable: bool,
}

const ATTRIBUTE_NAMES: [(u32, &str); 11] = [
    (0x1, "Read-only"),
    (0x2, "Hidden"),
    (0x4, "System"),
    (0x20, "Archive"),
    (0x100, "Temporary"),
    (0x200, "Sparse"),
    (0x400, "Link or cloud file"),
    (0x800, "Compressed"),
    (0x1000, "Offline"),
    (0x4000, "Encrypted"),
    (0x40_0000, "Online-only"),
];
const DIRECTORY: u32 = 0x10;
/// Reading the data of a file with either attribute fetches it from elsewhere first.
const RECALL: u32 = 0x1000 | 0x4_0000 | 0x40_0000;

/// The extended-length form of a path, for Win32 calls that would otherwise stop at 260
/// characters.
fn extended(path: &str) -> Vec<u16> {
    let long = match path.strip_prefix(r"\\") {
        Some(share) => format!(r"\\?\UNC\{share}"),
        None => format!(r"\\?\{path}"),
    };
    long.encode_utf16().chain([0]).collect()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn unix_ms(filetime: windows_sys::Win32::Foundation::FILETIME) -> i64 {
    let ticks = (filetime.dwHighDateTime as i64) << 32 | filetime.dwLowDateTime as i64;
    if ticks == 0 { 0 } else { (ticks - FILETIME_UNIX_EPOCH) / 10_000 }
}

/// Attributes, times and size, read without opening the file, so a cloud file is not fetched.
fn attribute_data(path: &str) -> io::Result<WIN32_FILE_ATTRIBUTE_DATA> {
    let wide = extended(path);
    // SAFETY: all-zero is a valid value for this plain-data struct.
    let mut data: WIN32_FILE_ATTRIBUTE_DATA = unsafe { std::mem::zeroed() };
    // SAFETY: `wide` is NUL-terminated and `data` is the struct this information level fills.
    let ok = unsafe { GetFileAttributesExW(wide.as_ptr(), GetFileExInfoStandard, (&raw mut data).cast()) };
    if ok != 0 { Ok(data) } else { Err(io::Error::last_os_error()) }
}

/// The shell's name for files with this extension, such as "PNG File".
pub(crate) fn type_name(ext: &str) -> Option<String> {
    let assoc = wide(&format!(".{ext}"));
    let mut buffer = [0u16; 256];
    let mut len = buffer.len() as u32;
    // SAFETY: `assoc` is NUL-terminated, and `buffer` holds `len` characters, which bounds
    // what the call writes.
    let result = unsafe {
        AssocQueryStringW(ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYDOCNAME, assoc.as_ptr(), null(), buffer.as_mut_ptr(), &mut len)
    };
    let end = buffer.iter().position(|&c| c == 0)?;
    (result >= 0 && end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

fn extension(path: &str) -> String {
    Path::new(path).extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase()
}

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "apng", "jpg", "jpeg", "jfif", "pjpeg", "pjp", "gif", "webp", "bmp", "dib", "ico", "cur", "psd",
    "psb", "tif", "tiff",
];

/// Describes a file or folder for the details block.
pub fn describe(engine: &Engine, path: &str) -> io::Result<Details> {
    let data = attribute_data(path)?;
    let attrs = data.dwFileAttributes;
    let dir = attrs & DIRECTORY != 0;
    let online_only = attrs & RECALL != 0;
    let ext = extension(path);
    let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path).to_owned();

    let (size, type_name, dimensions, folder) = if dir {
        let summary = match walk::list(path) {
            Ok(items) => {
                let folders = items.iter().filter(|item| item.meta.attrs & DIRECTORY != 0).count() as u32;
                FolderSummary { files: items.len() as u32 - folders, folders, readable: true }
            }
            Err(_) => FolderSummary { files: 0, folders: 0, readable: false },
        };
        let index = engine.index().read();
        let total = index.lookup(path).filter(|&id| index.is_dir(id)).map(|id| index.size(id));
        (total, "File folder".to_owned(), None, Some(summary))
    } else {
        let size = (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64;
        let type_name = type_name(&ext).unwrap_or_else(|| match ext.as_str() {
            "" => "File".to_owned(),
            ext => format!("{} File", ext.to_ascii_uppercase()),
        });
        let dimensions = (!online_only && IMAGE_EXTENSIONS.contains(&ext.as_str()))
            .then(|| read_header(path).ok().and_then(|header| image_dimensions(&header)))
            .flatten();
        (Some(size), type_name, dimensions, None)
    };

    Ok(Details {
        name,
        path: path.to_owned(),
        dir,
        size,
        created: unix_ms(data.ftCreationTime),
        modified: unix_ms(data.ftLastWriteTime),
        accessed: unix_ms(data.ftLastAccessTime),
        attributes: ATTRIBUTE_NAMES.iter().filter(|(bit, _)| attrs & bit != 0).map(|&(_, name)| name).collect(),
        online_only,
        type_name,
        dimensions,
        folder,
    })
}

fn read_header(path: &str) -> io::Result<Vec<u8>> {
    let (file, _, _) = open_regular(path).map_err(|_| io::Error::other("not a regular file"))?;
    let mut header = Vec::new();
    file.take(HEADER_PROBE).read_to_end(&mut header)?;
    Ok(header)
}

#[tauri::command(async)]
pub fn preview_details(window: tauri::Window, engine: State<'_, Arc<Engine>>, path: String) -> Result<Details, String> {
    // Items inside archives, and archives themselves, with what they hold (archive::browse).
    if let Some(details) = crate::archive::preview_details(&engine, &path, window.label()) {
        return details;
    }
    describe(&engine, &path).map_err(|error| explain(&error))
}

/// A link from previewed content that may be handed to the shell: only web and mail links,
/// normalised so that no space or quote reaches it. Anything else could start a program.
pub fn checked_link(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let web = matches!(url.scheme(), "http" | "https") && url.host_str().is_some_and(|host| !host.is_empty());
    (web || url.scheme() == "mailto").then(|| url.to_string())
}

/// Opens a web or mail link in the default browser or mail app.
#[tauri::command(async)]
pub fn open_link(url: String) -> Result<(), String> {
    let link = checked_link(&url).ok_or("Only web and mail links can be opened")?;
    let target = wide(&link);
    let verb = wide("open");
    // SAFETY: both strings are NUL-terminated and outlive the call.
    let result = unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL)
    };
    // ShellExecute reports success as a value greater than 32.
    if result as usize > 32 { Ok(()) } else { Err(format!("Windows couldn't open {link}")) }
}

// ---------- image sizes ----------

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
}

/// An image's width and height, read from the start of the file, for PNG, GIF, BMP, JPEG,
/// WebP, ICO, PSD and TIFF.
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let size = match bytes {
        [0x89, b'P', b'N', b'G', ..] if bytes.get(12..16) == Some(&b"IHDR"[..]) => (be32(bytes, 16)?, be32(bytes, 20)?),
        [b'G', b'I', b'F', b'8', ..] => (le16(bytes, 6)?, le16(bytes, 8)?),
        [b'B', b'M', ..] => bmp_size(bytes)?,
        [0xFF, 0xD8, ..] => jpeg_size(bytes)?,
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => webp_size(bytes)?,
        [0, 0, 1 | 2, 0, ..] => icon_size(bytes)?,
        [b'8', b'B', b'P', b'S', ..] => (be32(bytes, 18)?, be32(bytes, 14)?),
        [b'I', b'I', 42, 0, ..] => tiff_size(bytes, le16, le32)?,
        [b'M', b'M', 0, 42, ..] => tiff_size(bytes, be16, be32)?,
        _ => return None,
    };
    (size.0 > 0 && size.1 > 0).then_some(size)
}

fn bmp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    // The old OS/2 header stores 16-bit sizes; later ones store signed 32-bit sizes, with a
    // negative height for a top-down bitmap.
    if le32(bytes, 14)? == 12 {
        Some((le16(bytes, 18)?, le16(bytes, 20)?))
    } else {
        Some(((le32(bytes, 18)? as i32).unsigned_abs(), (le32(bytes, 22)? as i32).unsigned_abs()))
    }
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        // Markers may be preceded by any number of fill bytes.
        while *bytes.get(at)? == 0xFF && *bytes.get(at + 1)? == 0xFF {
            at += 1;
        }
        if *bytes.get(at)? != 0xFF {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            // Start of frame, except DHT (C4), JPG (C8) and DAC (CC), which share the range.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                return Some((be16(bytes, at + 7)?, be16(bytes, at + 5)?));
            }
            // Markers without a length.
            0x01 | 0xD0..=0xD8 => at += 2,
            // Start of scan: the frame header should have come first.
            0xDA => return None,
            _ => at += 2 + be16(bytes, at + 2)? as usize,
        }
    }
}

fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    match bytes.get(12..16)? {
        b"VP8 " if bytes.get(23..26)? == &[0x9D, 0x01, 0x2A][..] => {
            Some((le16(bytes, 26)? & 0x3FFF, le16(bytes, 28)? & 0x3FFF))
        }
        b"VP8L" if *bytes.get(20)? == 0x2F => {
            let bits = le32(bytes, 21)?;
            Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(bytes, 24)? + 1, le24(bytes, 27)? + 1)),
        _ => None,
    }
}

/// The largest image in an icon or cursor file. A stored size of 0 means 256.
fn icon_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let count = le16(bytes, 4)? as usize;
    (0..count)
        .filter_map(|i| {
            let entry = bytes.get(6 + i * 16..6 + i * 16 + 2)?;
            let side = |b: u8| if b == 0 { 256 } else { b as u32 };
            Some((side(entry[0]), side(entry[1])))
        })
        .max_by_key(|&(w, h)| w * h)
}

fn tiff_size(
    bytes: &[u8],
    u16_at: fn(&[u8], usize) -> Option<u32>,
    u32_at: fn(&[u8], usize) -> Option<u32>,
) -> Option<(u32, u32)> {
    let ifd = u32_at(bytes, 4)? as usize;
    let entries = u16_at(bytes, ifd)? as usize;
    let (mut width, mut height) = (None, None);
    for i in 0..entries {
        let entry = ifd + 2 + i * 12;
        let value = match u16_at(bytes, entry + 2)? {
            3 => u16_at(bytes, entry + 8)?,
            4 => u32_at(bytes, entry + 8)?,
            _ => continue,
        };
        match u16_at(bytes, entry)? {
            256 => width = Some(value),
            257 => height = Some(value),
            _ => {}
        }
    }
    Some((width?, height?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A scratch folder under the temp directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kestrel-preview-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, contents: &[u8]) -> String {
            let path = self.0.join(name);
            fs::write(&path, contents).unwrap();
            path.to_str().unwrap().to_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn url_for(path: &str) -> String {
        let encoded: String = path
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        format!("http://preview.localhost/?p={encoded}&v=1")
    }

    fn get(path: &str, headers: &[(&str, &str)]) -> Response<Vec<u8>> {
        let mut request = Request::builder().uri(url_for(path));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        serve(&request.body(Vec::new()).unwrap())
    }

    fn header<'a>(response: &'a Response<Vec<u8>>, name: &str) -> Option<&'a str> {
        response.headers().get(name).map(|value| value.to_str().unwrap())
    }

    #[test]
    fn parses_ranges() {
        use ByteRange::*;
        assert_eq!(parse_range("bytes=0-99", 1000), Part(0, 99));
        assert_eq!(parse_range("bytes=500-", 1000), Part(500, 999));
        assert_eq!(parse_range("bytes=-100", 1000), Part(900, 999));
        assert_eq!(parse_range("bytes=-5000", 1000), Part(0, 999), "a suffix longer than the file is the file");
        assert_eq!(parse_range("bytes=900-5000", 1000), Part(900, 999), "the end is clamped");
        assert_eq!(parse_range("BYTES = 0-0", 1000), Whole, "spaces around `=` are not allowed");
        assert_eq!(parse_range("Bytes=0-0", 1000), Part(0, 0), "the unit is case-insensitive");
        assert_eq!(parse_range(" bytes=10-19 , 30-39", 1000), Part(10, 39), "several ranges coalesce");
        assert_eq!(parse_range("bytes=0-1, 5000-6000", 1000), Part(0, 1), "unsatisfiable parts drop out");
        assert_eq!(parse_range("bytes=1000-", 1000), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 1000), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable, "nothing can be sent from an empty file");
        assert_eq!(parse_range("bytes=5-1", 1000), Whole, "a backwards range is invalid and ignored");
        assert_eq!(parse_range("bytes=x-1", 1000), Whole);
        assert_eq!(parse_range("bytes=1", 1000), Whole);
        assert_eq!(parse_range("items=0-1", 1000), Whole, "unknown units are ignored");
        assert_eq!(parse_range("bytes=", 1000), Whole);
        assert_eq!(parse_range("", 1000), Whole);
    }

    #[test]
    fn serves_whole_files_and_ranges() {
        let scratch = Scratch::new("serve");
        let bytes: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        let path = scratch.file("clip.mp4", &bytes);

        let whole = get(&path, &[]);
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(whole.body(), &bytes);
        assert_eq!(header(&whole, "content-type"), Some("video/mp4"));
        assert_eq!(header(&whole, "content-length"), Some("10000"));
        assert_eq!(header(&whole, "accept-ranges"), Some("bytes"));
        assert_eq!(header(&whole, "x-content-type-options"), Some("nosniff"));
        assert!(header(&whole, "content-security-policy").unwrap().starts_with("sandbox;"));

        let part = get(&path, &[("Range", "bytes=100-199")]);
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(part.body(), &bytes[100..200]);
        assert_eq!(header(&part, "content-range"), Some("bytes 100-199/10000"));
        assert_eq!(header(&part, "content-length"), Some("100"));

        let tail = get(&path, &[("Range", "bytes=-10")]);
        assert_eq!(tail.body(), &bytes[9990..]);
        assert_eq!(header(&tail, "content-range"), Some("bytes 9990-9999/10000"));

        let open = get(&path, &[("Range", "bytes=9000-")]);
        assert_eq!(open.body(), &bytes[9000..]);

        let beyond = get(&path, &[("Range", "bytes=10000-")]);
        assert_eq!(beyond.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header(&beyond, "content-range"), Some("bytes */10000"));
        assert!(beyond.body().is_empty());

        // A range for an older version of the file is answered with the whole current one.
        let etag = header(&whole, "etag").unwrap().to_owned();
        assert_eq!(get(&path, &[("Range", "bytes=0-9"), ("If-Range", etag.as_str())]).status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(get(&path, &[("Range", "bytes=0-9"), ("If-Range", "\"old\"")]).status(), StatusCode::OK);

        let mut head = Request::builder().method("HEAD").uri(url_for(&path)).body(Vec::new()).unwrap();
        let response = serve(&head);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "content-length"), Some("10000"));
        assert!(response.body().is_empty());
        *head.method_mut() = Method::POST;
        assert_eq!(serve(&head).status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn large_files_are_sent_in_bounded_parts() {
        let scratch = Scratch::new("large");
        let path = scratch.0.join("big.webm");
        // Extending a file writes nothing, so a file the size of a large video is quick to make.
        let file = File::create(&path).unwrap();
        file.set_len(MAX_WHOLE + 1).unwrap();
        drop(file);
        let path = path.to_str().unwrap();

        let whole = get(path, &[]);
        assert_eq!(whole.status(), StatusCode::PAYLOAD_TOO_LARGE, "too large to send whole");
        assert_eq!(header(&whole, "accept-ranges"), Some("bytes"));
        assert!(whole.body().is_empty());

        let head = Request::builder().method("HEAD").uri(url_for(path)).body(Vec::new()).unwrap();
        let head = serve(&head);
        assert_eq!(head.status(), StatusCode::OK, "a HEAD request reads nothing, so it is answered");
        assert_eq!(header(&head, "content-length"), Some((MAX_WHOLE + 1).to_string().as_str()));

        let first = get(path, &[("Range", "bytes=0-")]);
        assert_eq!(first.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(first.body().len() as u64, MAX_CHUNK);
        assert_eq!(header(&first, "content-range"), Some(format!("bytes 0-{}/{}", MAX_CHUNK - 1, MAX_WHOLE + 1).as_str()));

        let open = get(path, &[("Range", "bytes=1000-")]);
        assert_eq!(open.body().len() as u64, MAX_CHUNK, "an open range is answered with one part");
        assert_eq!(header(&open, "content-range"), Some(format!("bytes 1000-{}/{}", 999 + MAX_CHUNK, MAX_WHOLE + 1).as_str()));
    }

    #[test]
    fn serves_only_regular_files_with_safe_types() {
        let scratch = Scratch::new("safety");
        let page = scratch.file("page.html", b"<script>alert(1)</script>");
        let html = get(&page, &[]);
        assert_eq!(html.status(), StatusCode::OK);
        assert_eq!(header(&html, "content-type"), Some("text/plain"), "HTML is shown as text");
        assert!(header(&html, "content-security-policy").unwrap().starts_with("sandbox;"));

        let svg = scratch.file("logo.svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>");
        assert_eq!(header(&get(&svg, &[]), "content-type"), Some("image/svg+xml"));
        assert!(header(&get(&svg, &[]), "content-security-policy").unwrap().starts_with("sandbox;"));

        let pdf = scratch.file("doc.PDF", b"%PDF-1.4");
        assert_eq!(header(&get(&pdf, &[]), "content-type"), Some("application/pdf"));

        let folder = scratch.0.to_str().unwrap();
        assert_eq!(get(folder, &[]).status(), StatusCode::FORBIDDEN, "folders are never served");
        assert_eq!(get(&format!("{folder}\\missing.png"), &[]).status(), StatusCode::NOT_FOUND);
        assert_eq!(get(r"\\.\PhysicalDrive0", &[]).status(), StatusCode::BAD_REQUEST);
        assert_eq!(get(r"\\?\C:\Windows\win.ini", &[]).status(), StatusCode::BAD_REQUEST);
        assert_eq!(get("relative.txt", &[]).status(), StatusCode::BAD_REQUEST);
        let no_path = Request::builder().uri("http://preview.localhost/").body(Vec::new()).unwrap();
        assert_eq!(serve(&no_path).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn online_only_files_are_served_only_once_the_user_agreed_to_download_them() {
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, SetFileAttributesW};
        let scratch = Scratch::new("online-only");
        let photo = scratch.file("photo.png", b"\x89PNG\r\n\x1a\n");
        // Offline is the attribute a cloud placeholder whose data is elsewhere carries; setting
        // it on a local file changes nothing but the flag.
        let wide = wide(&photo);
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        assert_ne!(unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_OFFLINE) }, 0);

        // An image a Markdown document refers to is asked for without that agreement, however
        // its path is spelled.
        assert_eq!(get(&photo, &[]).status(), StatusCode::FORBIDDEN);
        assert_eq!(get(&format!("{photo}."), &[]).status(), StatusCode::FORBIDDEN, "a trailing dot opens the same file");
        assert_eq!(get(&photo.replace('\\', "/"), &[]).status(), StatusCode::FORBIDDEN);
        // A link carries attributes of its own; creating one needs a privilege or developer mode.
        let link = scratch.0.join("link.png");
        if std::os::windows::fs::symlink_file(&photo, &link).is_ok() {
            assert_eq!(get(link.to_str().unwrap(), &[]).status(), StatusCode::FORBIDDEN, "a link to an online-only file");
        }
        // Which is why the file is checked again once open, where a link has led to its target.
        assert!(is_opened_online_only(&File::open(&photo).unwrap()));
        let head = Request::builder().method("HEAD").uri(url_for(&photo)).body(Vec::new()).unwrap();
        assert_eq!(serve(&head).status(), StatusCode::FORBIDDEN);
        // The pane's own request for the item, after "Download and preview".
        let agreed = Request::builder().uri(format!("{}&download=1", url_for(&photo))).body(Vec::new()).unwrap();
        assert_eq!(serve(&agreed).status(), StatusCode::OK);
        // A file on this computer needs no agreement.
        let local = scratch.file("local.png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(get(&local, &[]).status(), StatusCode::OK);
        assert!(!is_opened_online_only(&File::open(&local).unwrap()));
    }

    #[test]
    fn the_viewer_page_names_the_file_and_runs_nothing() {
        let page = |query: &str| {
            let request = Request::builder().uri(format!("http://preview.localhost/view?{query}")).body(Vec::new()).unwrap();
            serve(&request)
        };
        let path = url_for(r#"C:\a "b" <c> & d.png"#);
        let query = path.split_once('?').unwrap().1;
        let image = page(&format!("kind=image&{query}&bg=%23fbfcfd&raised=%23edf0f4&line=red&scheme=light"));
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(header(&image, "content-type"), Some("text/html; charset=utf-8"));
        let policy = header(&image, "content-security-policy").unwrap();
        assert!(policy.starts_with("sandbox;") && policy.contains("default-src 'none'") && !policy.contains("script"), "{policy}");
        let html = String::from_utf8(image.body().clone()).unwrap();
        // The path reaches the page only percent-encoded, so it can't close the attribute.
        assert!(html.contains(r#"<img src="/?p=C%3A%5Ca%20%22b%22%20%3Cc%3E%20%26%20d.png&amp;v=1" alt="">"#), "{html}");
        assert!(!html.contains("<script") && !html.contains("download"));
        assert!(html.contains("#fbfcfd") && html.contains("#edf0f4"));
        assert!(!html.contains("red"), "a colour that isn't #rrggbb is replaced");
        assert!(html.contains(r#"content="light""#));

        let video = String::from_utf8(page("kind=video&p=C%3A%5Cclip.webm&v=7&download=1").body().clone()).unwrap();
        assert!(video.contains(r#"<video src="/?p=C%3A%5Cclip.webm&amp;v=7&amp;download=1" controls preload="metadata">"#), "{video}");
        let audio = String::from_utf8(page("kind=audio&p=C%3A%5Ctone.wav").body().clone()).unwrap();
        assert!(audio.contains(r#"<audio src="/?p=C%3A%5Ctone.wav&amp;v=0" controls"#), "{audio}");

        assert_eq!(page("kind=page&p=C%3A%5Cx.html").status(), StatusCode::BAD_REQUEST, "only images, video and sound");
        assert_eq!(page("kind=image").status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn decodes_text() {
        let utf8 = decode_text("héllo".as_bytes(), false, 6);
        assert_eq!((utf8.text.as_str(), utf8.encoding, utf8.lossy, utf8.binary), ("héllo", "utf-8", false, false));

        let bom = decode_text(b"\xEF\xBB\xBFhi", false, 5);
        assert_eq!((bom.text.as_str(), bom.encoding), ("hi", "utf-8"));

        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain("hé😀".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let decoded = decode_text(&le, false, le.len() as u64);
        assert_eq!((decoded.text.as_str(), decoded.encoding, decoded.lossy), ("hé😀", "utf-16le", false));

        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain("ok".encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(decode_text(&be, false, 6).text, "ok");
        assert_eq!(decode_text(&be, false, 6).encoding, "utf-16be");

        // Cut at the read limit in the middle of a surrogate pair, or of an odd byte.
        let cut = &le[..le.len() - 2];
        let decoded = decode_text(cut, true, 100);
        assert_eq!((decoded.text.as_str(), decoded.lossy, decoded.truncated), ("hé", false, true));
        assert_eq!(decode_text(&le[..le.len() - 1], true, 100).text, "hé");

        // A UTF-8 character cut in two at the limit is dropped, not replaced.
        let cut = &"ab€".as_bytes()[..4];
        let decoded = decode_text(cut, true, 100);
        assert_eq!((decoded.text.as_str(), decoded.lossy), ("ab", false));
        // In a complete file the same bytes are invalid and get a replacement character.
        let decoded = decode_text(cut, false, 4);
        assert_eq!((decoded.text.as_str(), decoded.lossy), ("ab\u{FFFD}", true));

        let latin1 = decode_text(b"caf\xE9 au lait", false, 12);
        assert_eq!((latin1.text.as_str(), latin1.lossy), ("caf\u{FFFD} au lait", true));

        let binary = decode_text(b"MZ\x90\x00\x03\x00", false, 6);
        assert!(binary.binary && binary.text.is_empty());
    }

    #[test]
    fn reads_at_most_the_text_limit() {
        let scratch = Scratch::new("text");
        let small = scratch.file("small.rs", b"fn main() {}\n");
        let read = read_text(&small).unwrap();
        assert_eq!((read.text.as_str(), read.truncated, read.size), ("fn main() {}\n", false, 13));

        let mut big = "€".repeat(TEXT_LIMIT / 3 + 10).into_bytes();
        big.extend_from_slice(b"end");
        let big_path = scratch.file("big.txt", &big);
        let read = read_text(&big_path).unwrap();
        assert!(read.truncated && !read.lossy);
        assert_eq!(read.size, big.len() as u64);
        assert!(read.text.len() <= TEXT_LIMIT);
        assert!(read.text.chars().all(|c| c == '€'), "the cut falls between characters");

        let exact = scratch.file("exact.txt", &vec![b'a'; TEXT_LIMIT]);
        assert!(!read_text(&exact).unwrap().truncated, "a file of exactly the limit is complete");
        assert!(read_text(scratch.0.to_str().unwrap()).is_err(), "a folder is not text");

        let mut program = b"MZ\x90\x00".to_vec();
        program.resize(3 * TEXT_LIMIT, b'x');
        let binary = read_text(&scratch.file("tool.exe", &program)).unwrap();
        assert!(binary.binary && binary.text.is_empty() && !binary.truncated);
        assert_eq!(binary.size, program.len() as u64);

        // A UTF-16 file is full of NUL bytes, and its mark says it is text all the same.
        let wide: Vec<u8> = [0xFF, 0xFE].into_iter().chain("wide text".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(read_text(&scratch.file("wide.txt", &wide)).unwrap().text, "wide text");
    }

    #[test]
    fn reads_image_sizes_from_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((640, 480)));

        let gif = b"GIF89a\x20\x03\x58\x02";
        assert_eq!(image_dimensions(gif), Some((800, 600)));

        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&300i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-200i32).to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((300, 200)), "a top-down bitmap has a negative height");

        // SOI, an APP0 segment, then a baseline frame header: 1024 wide, 768 high.
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x03, 0x00, 0x04, 0x00,
        ];
        assert_eq!(image_dimensions(&jpeg), Some((1024, 768)));
        let mut no_frame = jpeg[..8].to_vec();
        no_frame.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        assert_eq!(image_dimensions(&no_frame), None);

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.resize(30, 0);
        webp[24..27].copy_from_slice(&[0x1F, 0x03, 0x00]); // 800 - 1
        webp[27..30].copy_from_slice(&[0x57, 0x02, 0x00]); // 600 - 1
        assert_eq!(image_dimensions(&webp), Some((800, 600)));

        let ico = [0, 0, 1, 0, 2, 0, 16, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(image_dimensions(&ico), Some((256, 256)), "a stored 0 means 256");

        let mut tiff = b"II\x2a\x00\x08\x00\x00\x00\x02\x00".to_vec();
        tiff.extend_from_slice(&[0x00, 0x01, 0x03, 0x00, 0x01, 0, 0, 0, 0x40, 0x01, 0, 0]); // width 320 (SHORT)
        tiff.extend_from_slice(&[0x01, 0x01, 0x04, 0x00, 0x01, 0, 0, 0, 0xF0, 0, 0, 0]); // height 240 (LONG)
        assert_eq!(image_dimensions(&tiff), Some((320, 240)));

        assert_eq!(image_dimensions(b"not an image"), None);
        assert_eq!(image_dimensions(&png[..18]), None, "a truncated header gives no size");
    }

    #[test]
    fn only_web_and_mail_links_reach_the_shell() {
        assert_eq!(checked_link("https://example.com/a b").as_deref(), Some("https://example.com/a%20b"));
        assert_eq!(checked_link(" http://example.com ").as_deref(), Some("http://example.com/"));
        assert_eq!(checked_link("mailto:someone@example.com").as_deref(), Some("mailto:someone@example.com"));
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            r"C:\Windows\System32\calc.exe",
            "calc.exe",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "https://",
            "",
        ] {
            assert_eq!(checked_link(refused), None, "{refused}");
        }
    }

    #[test]
    fn only_the_apps_own_origin_may_call_commands() {
        let dev: Url = "http://localhost:1440/".parse().unwrap();
        assert!(same_origin("http://localhost:1440", &dev));
        assert!(!same_origin("http://preview.localhost", &dev));
        assert!(!same_origin("http://localhost:1441", &dev));
        assert!(!same_origin("null", &dev));
        assert!(!same_origin("", &dev));
        let built: Url = "http://tauri.localhost/index.html".parse().unwrap();
        assert!(same_origin("http://tauri.localhost", &built));
        assert!(!same_origin("http://preview.localhost", &built));
        assert!(!same_origin("http://icon.localhost", &built));
    }

    #[test]
    fn describes_files_and_folders() {
        let scratch = Scratch::new("details");
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&[0, 0, 0, 3, 0, 0, 0, 2]);
        let image = scratch.file("dot.png", &png);
        fs::create_dir(scratch.0.join("sub")).unwrap();
        scratch.file("sub/a.txt", b"12345");

        let engine = Engine::default();
        let details = describe(&engine, &image).unwrap();
        assert_eq!((details.name.as_str(), details.dir, details.size), ("dot.png", false, Some(png.len() as u64)));
        assert_eq!(details.dimensions, Some((3, 2)));
        assert!(details.modified > 1_600_000_000_000, "{}", details.modified);
        assert!(!details.type_name.is_empty());
        assert!(!details.online_only);

        let folder = describe(&engine, scratch.0.to_str().unwrap()).unwrap();
        assert!(folder.dir);
        assert_eq!(folder.type_name, "File folder");
        let summary = folder.folder.unwrap();
        assert_eq!((summary.files, summary.folders, summary.readable), (1, 1, true));
        assert_eq!(folder.size, None, "a folder outside the index has no known total");
        assert!(describe(&engine, &format!("{}\\missing", scratch.0.display())).is_err());

        // A drive root, shown when browsing it with nothing focused; only read, never changed.
        let root = describe(&engine, r"C:\").unwrap();
        assert!(root.dir && root.folder.is_some_and(|summary| summary.readable));
        assert_eq!(root.name, r"C:\");
    }
}
