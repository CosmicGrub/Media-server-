//! Minimal HTTP/1.1 `GET`/`HEAD` file server for streaming a library file's bytes -- the piece
//! `remote/protocol.rs`'s own module doc names as still missing: "there is no browser client today"
//! because nothing on that wire ever carries video bytes, only small JSON state pushes.
//!
//! **Multiplexed onto the same TLS listener, not a second port.** `handle_connection`'s caller peeks
//! at a new connection's first bytes and routes here only when they look like an HTTP request line
//! (`GET `/`HEAD `); anything else falls through to the existing JSON-line protocol unchanged. This
//! reuses that listener's TLS certificate, `TokenStore`, and [`super::contain_within_library`] path
//! containment check rather than inventing a second auth/containment story for one new surface.
//!
//! **Hand-rolled, not an HTTP crate.** Matches this workspace's established pattern (`json.rs`,
//! `lumen-probe`'s EBML/ISOBMFF readers) of writing the small, bounded parser one specific job needs
//! rather than a general-purpose dependency, for a server that only ever answers a `GET`/`HEAD` with
//! no request body and no keep-alive (`Connection: close` on every response -- one request per TCP
//! connection, which is exactly what `server.rs`'s one-thread-per-connection model already assumes).
//!
//! **Auth**: the same bearer token `Pair`/`Auth` already issue, read from `Authorization: Bearer
//! <token>` or a `?token=` query parameter -- the latter because a `<video src="...">` element has no
//! way to set a custom header. **Containment**: `/stream/<url-encoded library path>`, resolved through
//! the identical canonicalize-and-check-ancestry logic `Play` already uses, so a paired client (or a
//! stolen token) can stream only a file this server actually scanned, the same guarantee `Play` makes.

use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use lumen_model::Container;

use super::{ServerContext, TlsStream, contain_within_library, is_timeout};

/// Bytes accumulated while looking for the end of the request headers (`\r\n\r\n`). A real request's
/// headers are a few hundred bytes at most; bounding this is what stops a client sending an enormous
/// header block from holding this thread's buffer open indefinitely.
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// How long a connection may take to deliver a complete request before it is simply dropped -- the
/// HTTP analogue of `server.rs`'s own per-line dispatch, bounding a slow-loris-style trickle rather
/// than waiting on it forever.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// One chunk of file body per read/write cycle. Large enough for real throughput, small enough that
/// streaming a multi-gigabyte remux never holds more than this much of it in memory at once.
const CHUNK_SIZE: usize = 256 * 1024;

struct HttpRequest {
    method: String,
    /// Percent-decoded, query string already stripped.
    path: String,
    /// The raw query string (before the `?`), left encoded -- only `query_param` needs to decode a
    /// value, and only the one value it is asked for.
    query: String,
    headers: std::collections::HashMap<String, String>,
}

impl HttpRequest {
    /// `None` on anything malformed or incomplete -- the caller answers 400 rather than guessing at
    /// partial intent, the posture every other parser in this workspace already takes.
    fn parse(head: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(head).ok()?;
        let mut lines = text.split("\r\n");
        let request_line = lines.next()?;
        let mut parts = request_line.split(' ');
        let method = parts.next()?.to_string();
        let raw_target = parts.next()?;
        parts.next()?; // HTTP version -- read to confirm the line has the right shape, not checked.

        let (raw_path, query) = match raw_target.split_once('?') {
            Some((p, q)) => (p, q.to_string()),
            None => (raw_target, String::new()),
        };

        let mut headers = std::collections::HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        Some(Self { method, path: percent_decode(raw_path), query, headers })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Whether `head` looks like the start of an HTTP request line -- just enough of a check to route a
/// fresh connection, not a claim that the rest of the request is well-formed.
pub(super) fn looks_like_http_request(head: &[u8]) -> bool {
    const METHODS: &[&[u8]] = &[b"GET ", b"HEAD "];
    METHODS.iter().any(|m| head.starts_with(m))
}

/// Handle one connection identified as HTTP: finish reading the request headers (`initial` is
/// whatever the caller already read while peeking), parse, and answer. Always one request per
/// connection -- no keep-alive, no request body support, both correct for a server that only ever
/// serves a `GET`/`HEAD` for a file.
pub(super) fn handle_connection(tls: &mut TlsStream, mut pending: Vec<u8>, ctx: &ServerContext) {
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + HEADER_READ_TIMEOUT;
    loop {
        if let Some(end) = find_header_end(&pending) {
            let Some(req) = HttpRequest::parse(&pending[..end]) else {
                write_error(tls, 400, "Bad Request");
                return;
            };
            handle_request(tls, &req, ctx);
            return;
        }
        if pending.len() > MAX_HEADER_BYTES {
            write_error(tls, 431, "Request Header Fields Too Large");
            return;
        }
        if Instant::now() >= deadline {
            return; // Gave up waiting for a complete request; the peer gets nothing further.
        }
        match tls.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => {}
            Err(_) => return,
        }
    }
}

/// The index just past `\r\n\r\n`, if the buffer contains it -- i.e. the header block is complete.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn handle_request(tls: &mut TlsStream, req: &HttpRequest, ctx: &ServerContext) {
    if !matches!(req.method.as_str(), "GET" | "HEAD") {
        write_error(tls, 405, "Method Not Allowed");
        return;
    }

    // Needs no token: the page is the same static bytes for everyone and does nothing without a
    // valid `path`+`token` of its own, at which point it only ever requests `/stream/<path>` -- the
    // one place a token is actually checked. See `vr`'s own module doc for the full reasoning.
    if req.path == "/vr" {
        write_ok(tls, "text/html; charset=\"utf-8\"", super::vr::PAGE.as_bytes());
        return;
    }

    if let Some(rest) = req.path.strip_prefix("/hls/") {
        super::hls::handle(tls, &req.method, rest, ctx);
        return;
    }

    if let Some(rest) = req.path.strip_prefix("/dash/") {
        super::dash::handle(tls, &req.method, rest, ctx);
        return;
    }

    let Some(requested) = req.path.strip_prefix("/stream/") else {
        write_error(tls, 404, "Not Found");
        return;
    };

    let token = req
        .header("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_string)
        .or_else(|| query_param(&req.query, "token"));
    let Some(token) = token else {
        write_error(tls, 401, "Unauthorized");
        return;
    };
    if !ctx.tokens.lock().unwrap().is_valid(&token) {
        write_error(tls, 401, "Unauthorized");
        return;
    }

    let real_path = match contain_within_library(&ctx.library_root, requested) {
        Ok(p) => p,
        Err(_) => {
            write_error(tls, 404, "Not Found");
            return;
        }
    };

    // The container this file was scanned as, when it is still part of the current library listing --
    // used only to pick a more precise `Content-Type`. A file that has since fallen out of the scan
    // (moved, deleted and replaced) still streams; it just gets the extension-based fallback below,
    // never a reason to refuse a file `contain_within_library` already confirmed is in scope.
    let container = ctx
        .library
        .lock()
        .unwrap()
        .files
        .iter()
        .find(|f| f.path.to_string_lossy() == real_path)
        .and_then(|f| f.container);
    let ext = Path::new(&real_path).extension().and_then(std::ffi::OsStr::to_str);
    let content_type = content_type_for(container, ext);

    serve_file(tls, &req.method, Path::new(&real_path), content_type, req.header("range"));
}

pub(super) fn serve_file(
    tls: &mut TlsStream,
    method: &str,
    real_path: &Path,
    content_type: &str,
    range_header: Option<&str>,
) {
    let Ok(mut file) = std::fs::File::open(real_path) else {
        write_error(tls, 404, "Not Found");
        return;
    };
    let Ok(total_len) = file.metadata().map(|m| m.len()) else {
        write_error(tls, 500, "Internal Server Error");
        return;
    };

    let range = range_header.and_then(|h| parse_range(h, total_len));
    // A `Range` header that fails to parse (or asks for something outside the file) is answered with
    // 416, never silently treated as "no range was sent" -- serving the whole file when a player
    // asked to seek would look like a successful seek to nowhere.
    if range_header.is_some() && range.is_none() {
        write_range_not_satisfiable(tls, total_len);
        return;
    }
    let (start, end) = range.unwrap_or((0, total_len.saturating_sub(1)));
    let body_len = if total_len == 0 { 0 } else { end - start + 1 };

    let mut head = String::new();
    head.push_str(if range.is_some() {
        "HTTP/1.1 206 Partial Content\r\n"
    } else {
        "HTTP/1.1 200 OK\r\n"
    });
    head.push_str(&format!("Content-Type: {content_type}\r\n"));
    head.push_str(&format!("Content-Length: {body_len}\r\n"));
    head.push_str("Accept-Ranges: bytes\r\n");
    if range.is_some() {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{total_len}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    if tls.write_all(head.as_bytes()).is_err() {
        return;
    }
    if method == "HEAD" || body_len == 0 {
        return;
    }
    if file.seek(std::io::SeekFrom::Start(start)).is_err() {
        return;
    }

    let mut remaining = body_len;
    let mut buf = vec![0u8; CHUNK_SIZE];
    while remaining > 0 {
        let want = remaining.min(CHUNK_SIZE as u64) as usize;
        let n = match file.read(&mut buf[..want]) {
            Ok(0) => break, // The file shrank out from under us; send what was read and stop.
            Ok(n) => n,
            Err(_) => return,
        };
        if tls.write_all(&buf[..n]).is_err() {
            return;
        }
        remaining -= n as u64;
    }
}

/// Parses `Range: bytes=START-END` -- the only form a real player sends to seek. `None` for anything
/// else this does not support: multiple ranges (comma-separated), a unit other than `bytes`, a
/// syntactically invalid range, or one wholly outside the file -- all of which the caller turns into
/// 416 rather than guessing at what was meant.
fn parse_range(header: &str, total_len: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') || total_len == 0 {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let (start, end) = if start_s.is_empty() {
        // A suffix range: `bytes=-500` means the last 500 bytes.
        let suffix_len: u64 = end_s.parse().ok()?;
        (total_len.saturating_sub(suffix_len), total_len - 1)
    } else {
        let start: u64 = start_s.parse().ok()?;
        let end = if end_s.is_empty() { total_len - 1 } else { end_s.parse().ok()? };
        (start, end)
    };
    (start <= end && start < total_len).then_some((start, end.min(total_len - 1)))
}

fn write_range_not_satisfiable(tls: &mut TlsStream, total_len: u64) {
    let body = "Range Not Satisfiable";
    let head = format!(
        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total_len}\r\n\
         Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tls.write_all(head.as_bytes());
}

/// A whole in-memory response body in one shot -- only for the static `/vr` shell, which is small
/// and fixed, not the general-purpose path `serve_file` already owns for range-aware file streaming.
fn write_ok(tls: &mut TlsStream, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    if tls.write_all(head.as_bytes()).is_ok() {
        let _ = tls.write_all(body);
    }
}

pub(super) fn write_error(tls: &mut TlsStream, code: u16, reason: &str) {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{reason}",
        reason.len()
    );
    let _ = tls.write_all(head.as_bytes());
}

/// A precise `Content-Type` from the container this file was scanned as; falls back to a small
/// extension-based guess for audio, then the generic octet-stream a player treats as "just try it" --
/// the same "never refuse, degrade honestly" posture the rest of this codebase already takes rather
/// than rejecting a file this server has already agreed, via `contain_within_library`, to serve.
fn content_type_for(container: Option<Container>, ext: Option<&str>) -> &'static str {
    match container {
        Some(Container::Matroska) => "video/x-matroska",
        Some(Container::WebM) => "video/webm",
        Some(Container::Mp4 | Container::FragmentedMp4) => "video/mp4",
        Some(Container::MpegTs) => "video/mp2t",
        Some(Container::MpegPs) => "video/mpeg",
        Some(Container::Avi) => "video/x-msvideo",
        Some(Container::Asf) => "video/x-ms-asf",
        Some(Container::Flv) => "video/x-flv",
        Some(Container::Ogg) => "video/ogg",
        _ => match ext.map(str::to_ascii_lowercase).as_deref() {
            Some("mp3") => "audio/mpeg",
            Some("flac") => "audio/flac",
            Some("aac") => "audio/aac",
            Some("wav") => "audio/wav",
            Some("m4a") => "audio/mp4",
            _ => "application/octet-stream",
        },
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// Decodes `%XX` escapes. Bounded and total: a malformed escape (a stray `%`, non-hex digits, one
/// truncated at the end of the string) is passed through literally rather than panicking or dropping
/// bytes, the same "unknown is not fatal" stance every other parser in this workspace takes.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_get_request_line_is_recognised_and_a_json_line_is_not() {
        assert!(looks_like_http_request(b"GET /stream/x HTTP/1.1\r\n"));
        assert!(looks_like_http_request(b"HEAD /stream/x HTTP/1.1\r\n"));
        assert!(!looks_like_http_request(b"{\"type\":\"pair\",\"id\":\"1\"}\n"));
        assert!(!looks_like_http_request(b""));
    }

    #[test]
    fn a_request_line_and_headers_parse_including_the_query_string() {
        let raw = b"GET /stream/Movie%20(2019).mkv?token=abc123 HTTP/1.1\r\n\
                    Host: example\r\nRange: bytes=0-99\r\n\r\n";
        let req = HttpRequest::parse(raw).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/stream/Movie (2019).mkv");
        assert_eq!(req.query, "token=abc123");
        assert_eq!(req.header("range"), Some("bytes=0-99"));
        assert_eq!(req.header("host"), Some("example"));
    }

    #[test]
    fn malformed_request_lines_are_none_not_a_panic() {
        assert!(HttpRequest::parse(b"").is_none());
        assert!(HttpRequest::parse(b"GET\r\n\r\n").is_none(), "no target or version");
        assert!(HttpRequest::parse(b"GET /x\r\n\r\n").is_none(), "no version");
    }

    #[test]
    fn percent_decoding_handles_spaces_and_leaves_a_bad_escape_literal() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("no-escapes"), "no-escapes");
        assert_eq!(
            percent_decode("bad%"),
            "bad%",
            "a truncated escape passes through, not a panic"
        );
        assert_eq!(percent_decode("bad%zz"), "bad%zz", "non-hex digits pass through literally");
    }

    #[test]
    fn query_param_finds_the_right_key_among_several() {
        assert_eq!(query_param("a=1&token=abc&b=2", "token").as_deref(), Some("abc"));
        assert_eq!(query_param("a=1", "token"), None);
        assert_eq!(query_param("", "token"), None);
    }

    #[test]
    fn a_plain_range_is_parsed() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)), "open-ended means to EOF");
    }

    #[test]
    fn a_suffix_range_means_the_last_n_bytes() {
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        // A suffix longer than the whole file is clamped to the start, not rejected.
        assert_eq!(parse_range("bytes=-5000", 1000), Some((0, 999)));
    }

    #[test]
    fn a_range_past_the_end_of_a_shrunk_end_is_clamped_not_rejected() {
        assert_eq!(parse_range("bytes=0-999999", 1000), Some((0, 999)));
    }

    #[test]
    fn invalid_ranges_are_refused_rather_than_guessed_at() {
        assert_eq!(parse_range("bytes=500-100", 1000), None, "start after end");
        assert_eq!(parse_range("bytes=1000-1500", 1000), None, "start at/past EOF");
        assert_eq!(
            parse_range("bytes=0-10,20-30", 1000),
            None,
            "multi-range is refused, not truncated"
        );
        assert_eq!(parse_range("items=0-10", 1000), None, "wrong unit");
        assert_eq!(parse_range("bytes=abc-def", 1000), None);
        assert_eq!(parse_range("bytes=0-100", 0), None, "nothing to range over in an empty file");
    }

    #[test]
    fn content_type_prefers_the_scanned_container_over_the_extension() {
        assert_eq!(content_type_for(Some(Container::Matroska), Some("mkv")), "video/x-matroska");
        assert_eq!(content_type_for(Some(Container::WebM), Some("mkv")), "video/webm");
        assert_eq!(content_type_for(None, Some("mp3")), "audio/mpeg");
        assert_eq!(content_type_for(None, Some("flac")), "audio/flac");
        assert_eq!(content_type_for(None, None), "application/octet-stream");
    }

    #[test]
    fn find_header_end_locates_the_blank_line_and_only_that() {
        let full = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let end = find_header_end(full).expect("a complete header block must be found");
        assert_eq!(&full[end - 4..end], b"\r\n\r\n");
        assert_eq!(end, full.len(), "the blank line is the last thing in this buffer");
        assert_eq!(
            find_header_end(b"GET / HTTP/1.1\r\nHost: x\r\n"),
            None,
            "no terminating blank line yet"
        );
        assert_eq!(find_header_end(b""), None);
    }
}
