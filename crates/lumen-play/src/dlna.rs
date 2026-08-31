//! DLNA `MediaServer` support for `lumen serve` -- the `--dlna` opt-in surface: SSDP announcement
//! plus a plain, unauthenticated HTTP server answering `ContentDirectory`'s `Browse` action and
//! streaming the files it lists.
//!
//! **Deliberately separate infrastructure from `remote::server`, not an extension of it.** SSDP and
//! DLNA's `ContentDirectory`/`ConnectionManager` services are unauthenticated by protocol design --
//! any renderer on the LAN must be able to discover and browse a `MediaServer` with no handshake at
//! all, which is structurally incompatible with `remote::server`'s pairing-code-plus-pinned-TLS model
//! (`lumen_discovery`'s own module doc makes the same point about why *it* is a separate crate; this
//! module is the other half of the same reasoning, on the `lumen-play` side). This listener runs on
//! its own port, in plain HTTP, and is never started unless `--dlna` is passed -- an operator who
//! never asks for it gets exactly the security posture they already had.
//!
//! **Stage 1, honestly scoped.** Every playable file in the current scan is listed as a single flat
//! set of children under the root container ("0") -- no folder hierarchy yet, which
//! `lumen_discovery::content_directory`'s own module doc already flagged as the next real limitation
//! once this ships. Object IDs are the file's index into the current [`Scan`], stable only for the
//! lifetime of one `lumen serve` process (the same "one snapshot taken at startup, never refreshed"
//! limitation `docs/15` Engine A already documents for the paired control channel's own library
//! listing -- this shares that gap rather than inventing a second one).

use std::io::{Read, Seek, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lumen_discovery::{
    Announcement, BrowseFlag, DeviceIdentity, DidlObject, DidlResource, ObjectClass, Responder,
    build_browse_response, build_device_description, build_didl_lite,
    build_get_current_connection_ids_response, build_get_protocol_info_response, build_soap_fault,
    connection_manager_scpd, content_directory_scpd, parse_browse_request,
};
use lumen_model::Container;

use crate::scan::Scan;

/// How long an SSDP announcement stays valid before this responder re-multicasts it.
const RENOTIFY_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// One chunk of file body per read/write cycle, matching `remote::server::http`'s own constant --
/// the same tradeoff (real throughput, bounded memory) applies identically here.
const CHUNK_SIZE: usize = 256 * 1024;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The MIME types this server can source, advertised via `ConnectionManager#GetProtocolInfo` --
/// exactly [`content_type_for`]'s own range, so this list can never claim a format the actual
/// streaming handler does not serve.
const SOURCE_MIME_TYPES: &[&str] = &[
    "video/x-matroska",
    "video/webm",
    "video/mp4",
    "video/mp2t",
    "video/mpeg",
    "video/x-msvideo",
    "video/x-ms-asf",
    "video/x-flv",
    "video/ogg",
    "audio/mpeg",
    "audio/flac",
    "audio/aac",
    "audio/wav",
    "audio/mp4",
];

/// Run the DLNA listener. Blocks until the process is killed, the same "meant to run on its own
/// thread" contract `lumen_discovery::Responder::run` and `remote::server::run` both already commit
/// to. `bind`/`port` are the plain-HTTP listener's own; SSDP itself always uses the fixed multicast
/// port regardless of what is passed here.
///
/// Scans `library_root` itself rather than sharing the paired control channel's `Scan` -- this
/// listener is deliberately separate infrastructure (see the module doc), and `lumen serve` may run
/// with or without `--dlna` independently of whether pairing is even in use, so tying the two
/// together would reintroduce exactly the coupling this module exists to avoid. The same "one
/// snapshot at startup" limitation applies here as it does there.
pub fn run(
    library_root: PathBuf,
    bind: &str,
    port: u16,
    friendly_name: String,
    log: impl Fn(&str) + Send + Sync + 'static,
) -> Result<(), String> {
    let scan = crate::scan::scan(
        std::slice::from_ref(&library_root),
        &crate::scan::ScanOptions::default(),
    );
    let library = Arc::new(Mutex::new(scan));

    let advertise_host = if bind == "0.0.0.0" {
        local_ip_for_lan().unwrap_or(Ipv4Addr::LOCALHOST)
    } else {
        bind.parse().map_err(|_| format!("--dlna-bind {bind} is not a valid IPv4 address"))?
    };
    let base_url = format!("http://{advertise_host}:{port}");
    let location = format!("{base_url}/dlna/desc.xml");
    let uuid = random_uuid();

    let announcements = vec![
        Announcement {
            notification_type: "upnp:rootdevice".into(),
            unique_service_name: format!("uuid:{uuid}::upnp:rootdevice"),
        },
        Announcement {
            notification_type: format!("uuid:{uuid}"),
            unique_service_name: format!("uuid:{uuid}"),
        },
        Announcement {
            notification_type: "urn:schemas-upnp-org:device:MediaServer:1".into(),
            unique_service_name: format!("uuid:{uuid}::urn:schemas-upnp-org:device:MediaServer:1"),
        },
        Announcement {
            notification_type: "urn:schemas-upnp-org:service:ContentDirectory:1".into(),
            unique_service_name: format!(
                "uuid:{uuid}::urn:schemas-upnp-org:service:ContentDirectory:1"
            ),
        },
        Announcement {
            notification_type: "urn:schemas-upnp-org:service:ConnectionManager:1".into(),
            unique_service_name: format!(
                "uuid:{uuid}::urn:schemas-upnp-org:service:ConnectionManager:1"
            ),
        },
    ];

    let responder = Responder::bind(location, announcements).map_err(|e| {
        format!(
            "cannot start SSDP (is another DLNA/UPnP responder already running \
                               without SO_REUSEADDR support, or is this environment blocking \
                               multicast?): {e}"
        )
    })?;
    // `log` is `Fn`, not `Clone`, but `Arc<T>` is `Clone` regardless of whether `T` is -- wrapping it
    // once here is all the SSDP thread and this function's own later use of `log` need to each get
    // their own handle to the same closure.
    let log = Arc::new(log);
    {
        let log = Arc::clone(&log);
        std::thread::spawn(move || responder.run(RENOTIFY_INTERVAL, |m| log(m)));
    }

    let listener = TcpListener::bind((bind, port))
        .map_err(|e| format!("cannot listen on {bind}:{port} for DLNA: {e}"))?;
    log(&format!("DLNA: advertising \"{friendly_name}\" at {base_url}/dlna/desc.xml"));

    let ctx = Arc::new(DlnaContext { library, library_root, base_url, friendly_name, uuid });
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle_connection(stream, &ctx));
    }
    Ok(())
}

struct DlnaContext {
    library: Arc<Mutex<Scan>>,
    /// Not read yet -- reserved for the folder-hierarchy `Browse` support this module's own doc
    /// comment already flags as Stage 1's next limitation, where child containers will need to be
    /// resolved back to real subdirectories under this root. Kept on the context now rather than
    /// added as a second breaking-change parameter later.
    #[allow(dead_code)]
    library_root: PathBuf,
    base_url: String,
    friendly_name: String,
    /// Generated once in [`run`] and carried here so every `desc.xml` response agrees with the UUID
    /// already baked into the SSDP announcements and the response's own LOCATION URL.
    uuid: String,
}

fn handle_connection(mut tcp: TcpStream, ctx: &DlnaContext) {
    let _ = tcp.set_read_timeout(Some(HEADER_READ_TIMEOUT));
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + HEADER_READ_TIMEOUT;
    let header_end = loop {
        if let Some(end) = find_header_end(&pending) {
            break end;
        }
        if pending.len() > MAX_HEADER_BYTES || Instant::now() >= deadline {
            return;
        }
        match tcp.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => pending.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    };

    let Some(req) = HttpRequest::parse(&pending[..header_end]) else {
        write_error(&mut tcp, 400, "Bad Request");
        return;
    };

    let content_length: usize =
        req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = pending[header_end..].to_vec();
    while body.len() < content_length {
        match tcp.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
    body.truncate(content_length);

    route(&mut tcp, &req, &body, ctx);
}

fn route(tcp: &mut TcpStream, req: &HttpRequest, body: &[u8], ctx: &DlnaContext) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/dlna/desc.xml") => {
            let identity =
                DeviceIdentity { friendly_name: ctx.friendly_name.clone(), uuid: ctx.uuid.clone() };
            let doc = build_device_description(&identity, &ctx.base_url);
            write_ok(tcp, "text/xml; charset=\"utf-8\"", doc.as_bytes());
        }
        ("GET", "/dlna/cd.xml") => {
            write_ok(tcp, "text/xml; charset=\"utf-8\"", content_directory_scpd().as_bytes());
        }
        ("GET", "/dlna/cm.xml") => {
            write_ok(tcp, "text/xml; charset=\"utf-8\"", connection_manager_scpd().as_bytes());
        }
        ("POST", "/dlna/cd/control") => handle_content_directory_control(tcp, req, body, ctx),
        ("POST", "/dlna/cm/control") => handle_connection_manager_control(tcp, req, body),
        ("GET" | "HEAD", p) if p.starts_with("/dlna/stream/") => {
            serve_stream(tcp, req, &p["/dlna/stream/".len()..], ctx);
        }
        _ => write_error(tcp, 404, "Not Found"),
    }
}

fn handle_content_directory_control(
    tcp: &mut TcpStream,
    req: &HttpRequest,
    body: &[u8],
    ctx: &DlnaContext,
) {
    let soap = String::from_utf8_lossy(body);
    let action = req.header("soapaction").unwrap_or("");
    if !action.contains("#Browse") {
        write_soap_fault(tcp, 401, "Invalid Action");
        return;
    }
    let Some(browse) = parse_browse_request(&soap) else {
        write_soap_fault(tcp, 402, "Invalid Args");
        return;
    };

    let scan = ctx.library.lock().unwrap();
    let files: Vec<_> = scan.playable().collect();

    let (objects, total) = match (browse.object_id.as_str(), browse.flag) {
        ("0", BrowseFlag::DirectChildren) => {
            let start = browse.starting_index as usize;
            let count = if browse.requested_count == 0 {
                files.len()
            } else {
                browse.requested_count as usize
            };
            let objects: Vec<DidlObject> = files
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .map(|(i, f)| DidlObject {
                    id: i.to_string(),
                    parent_id: "0".into(),
                    title: f.label(),
                    class: object_class_for(f.container),
                    resource: Some(DidlResource {
                        url: format!("{}/dlna/stream/{i}", ctx.base_url),
                        mime_type: content_type_for(f.container, f.extension.as_deref())
                            .to_string(),
                        size_bytes: Some(f.size),
                    }),
                })
                .collect();
            (objects, files.len())
        }
        ("0", BrowseFlag::Metadata) => (
            vec![DidlObject {
                id: "0".into(),
                parent_id: "-1".into(),
                title: "lumen".into(),
                class: ObjectClass::StorageFolder,
                resource: None,
            }],
            1,
        ),
        (id, BrowseFlag::Metadata) => match id.parse::<usize>().ok().and_then(|i| files.get(i)) {
            Some(f) => (
                vec![DidlObject {
                    id: id.to_string(),
                    parent_id: "0".into(),
                    title: f.label(),
                    class: object_class_for(f.container),
                    resource: Some(DidlResource {
                        url: format!("{}/dlna/stream/{id}", ctx.base_url),
                        mime_type: content_type_for(f.container, f.extension.as_deref())
                            .to_string(),
                        size_bytes: Some(f.size),
                    }),
                }],
                1,
            ),
            None => {
                write_soap_fault(tcp, 701, "No such object");
                return;
            }
        },
        _ => {
            write_soap_fault(tcp, 701, "No such object");
            return;
        }
    };

    let didl = build_didl_lite(&objects);
    let response = build_browse_response(&didl, objects.len() as u32, total as u32, 1);
    write_ok(tcp, "text/xml; charset=\"utf-8\"", response.as_bytes());
}

fn handle_connection_manager_control(tcp: &mut TcpStream, req: &HttpRequest, _body: &[u8]) {
    let action = req.header("soapaction").unwrap_or("");
    if action.contains("#GetProtocolInfo") {
        write_ok(
            tcp,
            "text/xml; charset=\"utf-8\"",
            build_get_protocol_info_response(SOURCE_MIME_TYPES).as_bytes(),
        );
    } else if action.contains("#GetCurrentConnectionIDs") {
        write_ok(
            tcp,
            "text/xml; charset=\"utf-8\"",
            build_get_current_connection_ids_response().as_bytes(),
        );
    } else {
        write_soap_fault(tcp, 401, "Invalid Action");
    }
}

fn serve_stream(tcp: &mut TcpStream, req: &HttpRequest, id: &str, ctx: &DlnaContext) {
    let scan = ctx.library.lock().unwrap();
    let Some(f) = id.parse::<usize>().ok().and_then(|i| scan.playable().nth(i)) else {
        write_error(tcp, 404, "Not Found");
        return;
    };
    let path = f.path.clone();
    let content_type = content_type_for(f.container, f.extension.as_deref()).to_string();
    drop(scan);

    let Ok(mut file) = std::fs::File::open(&path) else {
        write_error(tcp, 404, "Not Found");
        return;
    };
    let Ok(total_len) = file.metadata().map(|m| m.len()) else {
        write_error(tcp, 500, "Internal Server Error");
        return;
    };

    let range = req.header("range").and_then(|h| parse_range(h, total_len));
    if req.header("range").is_some() && range.is_none() {
        write_range_not_satisfiable(tcp, total_len);
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
    head.push_str("contentFeatures.dnt.org: \r\n"); // Present, empty: no transcoding/DTCP profile claimed.
    if range.is_some() {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{total_len}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    if tcp.write_all(head.as_bytes()).is_err() || req.method == "HEAD" || body_len == 0 {
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
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return,
        };
        if tcp.write_all(&buf[..n]).is_err() {
            return;
        }
        remaining -= n as u64;
    }
}

fn object_class_for(container: Option<Container>) -> ObjectClass {
    match container {
        Some(
            Container::Matroska
            | Container::WebM
            | Container::Mp4
            | Container::FragmentedMp4
            | Container::MpegTs
            | Container::MpegPs
            | Container::Avi
            | Container::Asf
            | Container::Flv
            | Container::Ogg,
        ) => ObjectClass::VideoItem,
        _ => ObjectClass::VideoItem, // Honest default: this crate does not yet distinguish audio-only
                                     // scans by container alone; refining this is future work, not a
                                     // claim every listed item is definitely video today.
    }
}

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

fn parse_range(header: &str, total_len: u64) -> Option<(u64, u64)> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') || total_len == 0 {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let (start, end) = if start_s.is_empty() {
        let suffix_len: u64 = end_s.parse().ok()?;
        (total_len.saturating_sub(suffix_len), total_len - 1)
    } else {
        let start: u64 = start_s.parse().ok()?;
        let end = if end_s.is_empty() { total_len - 1 } else { end_s.parse().ok()? };
        (start, end)
    };
    (start <= end && start < total_len).then_some((start, end.min(total_len - 1)))
}

struct HttpRequest {
    method: String,
    path: String,
    headers: std::collections::HashMap<String, String>,
}

impl HttpRequest {
    fn parse(head: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(head).ok()?;
        let mut lines = text.split("\r\n");
        let request_line = lines.next()?;
        let mut parts = request_line.split(' ');
        let method = parts.next()?.to_string();
        let raw_target = parts.next()?;
        parts.next()?;
        let path = raw_target.split('?').next().unwrap_or(raw_target);
        let path = percent_decode(path);

        let mut headers = std::collections::HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        Some(Self { method, path, headers })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

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

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn write_ok(tcp: &mut TcpStream, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    if tcp.write_all(head.as_bytes()).is_ok() {
        let _ = tcp.write_all(body);
    }
}

fn write_error(tcp: &mut TcpStream, code: u16, reason: &str) {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{reason}",
        reason.len()
    );
    let _ = tcp.write_all(head.as_bytes());
}

fn write_range_not_satisfiable(tcp: &mut TcpStream, total_len: u64) {
    let body = "Range Not Satisfiable";
    let head = format!(
        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total_len}\r\n\
         Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tcp.write_all(head.as_bytes());
}

fn write_soap_fault(tcp: &mut TcpStream, code: u32, description: &str) {
    let body = build_soap_fault(code, description);
    let head = format!(
        "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if tcp.write_all(head.as_bytes()).is_ok() {
        let _ = tcp.write_all(body.as_bytes());
    }
}

/// Resolves this machine's own LAN-facing IPv4 address by asking the OS which local interface it
/// would use to reach the outside world -- a well-known, portable trick (a UDP "connect" only does a
/// routing-table lookup, no packet is actually sent, so this works offline too) that needs no new
/// dependency or platform-specific interface enumeration to answer "what address should I advertise"
/// when the operator bound the listener to `0.0.0.0`.
fn local_ip_for_lan() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) => Some(v4),
        std::net::IpAddr::V6(_) => None,
    }
}

/// A random UUID (version 4 layout: the right bits set so it *looks* like a real v4 UUID to anything
/// that checks, though nothing here depends on that distinction) -- `unsafe_code` is denied
/// workspace-wide, so this reads the OS random source through `getrandom`, the same crate
/// `remote::server`'s own pairing-code/token generation already uses.
fn random_uuid() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("the OS random source must be available");
    buf[6] = (buf[6] & 0x0F) | 0x40;
    buf[8] = (buf[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-\
         {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        buf[0],
        buf[1],
        buf[2],
        buf[3],
        buf[4],
        buf[5],
        buf[6],
        buf[7],
        buf[8],
        buf[9],
        buf[10],
        buf[11],
        buf[12],
        buf[13],
        buf[14],
        buf[15],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_line_and_headers_parse_including_a_percent_encoded_path() {
        let raw = b"GET /dlna/stream/Movie%20(2019).mkv HTTP/1.1\r\n\
                    Host: example\r\nRange: bytes=0-99\r\nSOAPACTION: \"#Browse\"\r\n\r\n";
        let req = HttpRequest::parse(raw).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/dlna/stream/Movie (2019).mkv");
        assert_eq!(req.header("range"), Some("bytes=0-99"));
        assert_eq!(req.header("soapaction"), Some("\"#Browse\""), "header names are lowercased");
    }

    #[test]
    fn malformed_request_lines_are_none_not_a_panic() {
        assert!(HttpRequest::parse(b"").is_none());
        assert!(HttpRequest::parse(b"GET\r\n\r\n").is_none(), "no target or version");
    }

    #[test]
    fn a_query_string_is_stripped_from_the_routed_path() {
        let raw = b"GET /dlna/stream/0?extra=1 HTTP/1.1\r\n\r\n";
        let req = HttpRequest::parse(raw).unwrap();
        assert_eq!(req.path, "/dlna/stream/0");
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
    fn find_header_end_locates_the_blank_line_and_only_that() {
        let full = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let end = find_header_end(full).expect("a complete header block must be found");
        assert_eq!(&full[end - 4..end], b"\r\n\r\n");
        assert_eq!(end, full.len(), "the blank line is the last thing in this buffer");
        assert_eq!(
            find_header_end(b"GET / HTTP/1.1\r\nHost: x\r\n"),
            None,
            "no blank line yet: headers are still incoming"
        );
    }

    #[test]
    fn a_plain_and_open_ended_range_are_parsed() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)), "open-ended means to EOF");
    }

    #[test]
    fn a_suffix_range_means_the_last_n_bytes_clamped_to_the_start() {
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-5000", 1000), Some((0, 999)));
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
    fn object_class_is_video_for_every_known_video_container_and_the_unknown_default() {
        assert_eq!(object_class_for(Some(Container::Matroska)), ObjectClass::VideoItem);
        assert_eq!(object_class_for(Some(Container::Mp4)), ObjectClass::VideoItem);
        assert_eq!(object_class_for(None), ObjectClass::VideoItem, "honest default, not a guess");
    }

    #[test]
    fn a_generated_uuid_has_the_version_4_and_variant_bits_set() {
        let uuid = random_uuid();
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(parts.len(), 5, "canonical 8-4-4-4-12 grouping: {uuid}");
        assert_eq!(parts[2].chars().next(), Some('4'), "version nibble must read 4: {uuid}");
        let variant = parts[3].chars().next().unwrap();
        assert!(matches!(variant, '8' | '9' | 'a' | 'b'), "variant nibble out of range: {uuid}");
        assert_ne!(random_uuid(), uuid, "two calls must not collide in practice");
    }

    #[test]
    fn local_ip_for_lan_returns_an_ipv4_address_when_it_returns_at_all() {
        // Best-effort like the sandboxed socket tests elsewhere in this workspace: a container with
        // no outbound routing table entry at all is a real environment this can run in, not a bug.
        if let Some(ip) = local_ip_for_lan() {
            assert_ne!(ip, Ipv4Addr::UNSPECIFIED, "a real routable address, not the wildcard");
        }
    }
}
