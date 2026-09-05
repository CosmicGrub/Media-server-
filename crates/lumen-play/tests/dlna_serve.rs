//! End-to-end verification of `lumen serve --dlna` against a real, nested directory tree.
//!
//! Every other test of `dlna.rs`'s own `LibraryTree`/`Browse`/`Search` dispatch logic (in
//! `src/dlna.rs`'s own `#[cfg(test)] mod tests`) drives that code directly, in-process, over a
//! loopback socket this test file does not share. This is the one that proves the pieces actually
//! work assembled: a real `lumen serve --dlna` process, spawned against a real nested fixture on disk,
//! issuing real SOAP `Browse` and `Search` requests over a real HTTP connection to the real listener
//! `dlna::run` binds -- the same wire protocol a real smart TV's Browse/Search UI would speak, not a
//! reimplementation of it.
//!
//! Real mpv is still required to spawn the process at all: `main.rs`'s `serve` command starts the DLNA
//! listener on its own thread and then unconditionally calls `remote::server::run`, which spawns an
//! idle mpv on startup regardless of whether `--dlna` was passed or any DLNA route is ever hit. This is
//! skipped, not failed, when mpv is not on `PATH` -- the same convention every other mpv-dependent test
//! in this crate already uses.
//!
//! DLNA itself never touches mpv or ffmpeg at all -- `serve_stream` just opens the file and copies
//! bytes, so the fixture files below are plain dummy bytes, not real encoded media.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// One self-deleting temp directory holding two *sibling* subdirectories: `library/`, the root the
/// spawned server is pointed at (and therefore the root both of its filesystem watchers recurse
/// over), and `config/`, which the server is told is its OS config directory.
///
/// Siblings, never `config/` inside the library: `remote::server::run` creates its HLS and DASH cache
/// directories under the config directory on the main thread only *after* mpv's IPC socket has
/// connected, while `dlna::run` arms its watcher before that on its own thread. With the config
/// directory under the watched root, those two `create_dir_all`s are real change events the DLNA
/// watcher sees -- and whether they coalesce into the first drop's refresh, get discarded by its quiet
/// period, or trigger a *second* refresh depends entirely on how long mpv took to start. That made
/// the auto-refresh test's `SystemUpdateID` assertions a function of mpv startup latency (reproduced:
/// an mpv delayed by 4s produced `SystemUpdateID` 3 for one dropped file and failed the "must not
/// keep climbing" check). Keeping the server's own writes outside the tree it watches is what makes
/// "one file dropped is one refresh" a statement about the watcher rather than about the runner.
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let anchor = 0u8;
        let d = std::env::temp_dir().join(format!(
            "lumen-dlna-it-{tag}-{}-{:x}",
            std::process::id(),
            std::ptr::from_ref(&anchor) as usize
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("library")).unwrap();
        std::fs::create_dir_all(d.join("config")).unwrap();
        Self(d)
    }

    /// The library root the server serves and watches.
    fn library(&self) -> PathBuf {
        self.0.join("library")
    }

    /// The directory the server is told is its OS config directory -- beside the library, never
    /// under it (see the type's own doc).
    fn config(&self) -> PathBuf {
        self.0.join("config")
    }

    /// Writes a fixture file at `rel` *under the library root*.
    fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.library().join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, bytes).unwrap();
        p
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Kills the wrapped `lumen serve` child (and its own child mpv) on drop, including when a panicking
/// assertion unwinds through the scope holding it -- see `remote_serve.rs`'s own `KillOnDrop` for the
/// full reasoning; this is the same fix, needed for the same reason, in a second test binary.
struct KillOnDrop(std::process::Child);
impl std::ops::Deref for KillOnDrop {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for KillOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn mpv_on_path() -> bool {
    std::process::Command::new("mpv")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn connect_with_retry(port: u16, timeout: Duration) -> TcpStream {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => return s,
            Err(e) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
                let _ = e;
            }
            Err(e) => panic!("could not connect to the DLNA listener on port {port}: {e}"),
        }
    }
}

/// Read one full HTTP/1.1 response -- status code, headers, and a body read to exactly
/// `Content-Length` (0 if absent). A small, purpose-built reader over a plain `TcpStream`, mirroring
/// `remote_serve.rs`'s own `read_http_response` but without TLS -- DLNA's HTTP surface is plain by
/// protocol design (see `dlna.rs`'s own module doc).
fn read_http_response(tcp: &mut TcpStream) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = tcp.read(&mut chunk).expect("reading the HTTP response must not fail");
        assert!(n > 0, "connection closed before a complete header block arrived");
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = std::str::from_utf8(&buf[..header_end]).expect("headers must be valid UTF-8");
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("a response must have a status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("a status line must carry a code")
        .parse()
        .expect("the status code must be numeric");

    let mut headers = Vec::new();
    let mut content_length = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        if k.eq_ignore_ascii_case("content-length") {
            content_length = v.parse::<usize>().ok();
        }
        headers.push((k, v));
    }

    let mut body = buf[header_end..].to_vec();
    let want = content_length.unwrap_or(0);
    while body.len() < want {
        let n = tcp.read(&mut chunk).expect("reading the HTTP body must not fail");
        assert!(n > 0, "connection closed before the full body arrived");
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(want);
    (status, headers, body)
}

fn browse_soap(object_id: &str, flag: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
         <s:Body><u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
         <ObjectID>{object_id}</ObjectID><BrowseFlag>{flag}</BrowseFlag>\
         <Filter>*</Filter><StartingIndex>0</StartingIndex>\
         <RequestedCount>0</RequestedCount><SortCriteria></SortCriteria>\
         </u:Browse></s:Body></s:Envelope>"
    )
}

/// Send a real `Browse` SOAP request over a fresh, real TCP connection to the DLNA port, and return
/// the full `BrowseResponse` SOAP body as text. Panics loudly (with the full response) if the reply is
/// not a well-formed `BrowseResponse` -- a SOAP fault included, since every call site that expects
/// success is asserting the happy path. [`browse`] narrows this to the DIDL-Lite; the auto-refresh
/// test reads the `<UpdateID>` off the whole envelope too.
fn browse_response(port: u16, object_id: &str, flag: &str) -> String {
    let mut tcp = connect_with_retry(port, Duration::from_secs(5));
    let body = browse_soap(object_id, flag);
    let request = format!(
        "POST /dlna/cd/control HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    let (status, _headers, resp_body) = read_http_response(&mut tcp);
    let text = String::from_utf8(resp_body).expect("a Browse response must be valid UTF-8");
    assert_eq!(status, 200, "a successful Browse is always wrapped in a 200: {text}");
    assert!(!text.contains("<errorCode>"), "expected a BrowseResponse, got a SOAP fault: {text}");
    text
}

/// [`browse_response`], narrowed to the DIDL-Lite text out of `<Result>...</Result>`, unescaped.
fn browse(port: u16, object_id: &str, flag: &str) -> String {
    let text = browse_response(port, object_id, flag);
    let start =
        text.find("<Result>").expect("a Browse response must carry <Result>") + "<Result>".len();
    let end = text.find("</Result>").expect("a Browse response must carry </Result>");
    unescape(&text[start..end])
}

/// The decimal text of one leaf element (`<UpdateID>7</UpdateID>`, `<Id>7</Id>`) in a SOAP
/// response -- the same bounded "find the one known tag" scan every other helper here uses, not a
/// parser.
fn leaf_u32(response: &str, tag: &str) -> u32 {
    let open = format!("<{tag}>");
    let start =
        response.find(&open).unwrap_or_else(|| panic!("no <{tag}> in: {response}")) + open.len();
    let end = start + response[start..].find('<').expect("a closing tag");
    response[start..end].parse().unwrap_or_else(|_| panic!("<{tag}> is not a u32: {response}"))
}

/// A real `GetSystemUpdateID` SOAP request over a fresh, real TCP connection -- the action a
/// spec-following control point polls to learn whether anything it cached is stale -- returning the
/// `Id` it reports. Panics loudly on anything but a well-formed `GetSystemUpdateIDResponse`.
fn get_system_update_id(port: u16) -> u32 {
    let mut tcp = connect_with_retry(port, Duration::from_secs(5));
    let body = "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
                <s:Body><u:GetSystemUpdateID xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\"/>\
                </s:Body></s:Envelope>";
    let request = format!(
        "POST /dlna/cd/control HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:ContentDirectory:1#GetSystemUpdateID\"\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    let (status, _headers, resp_body) = read_http_response(&mut tcp);
    let text =
        String::from_utf8(resp_body).expect("a GetSystemUpdateID response must be valid UTF-8");
    assert_eq!(status, 200, "GetSystemUpdateID must succeed: {text}");
    assert!(
        text.contains("<u:GetSystemUpdateIDResponse"),
        "not a GetSystemUpdateIDResponse: {text}"
    );
    leaf_u32(&text, "Id")
}

/// Same as [`browse`], but for a request this test expects to be *refused* -- returns the raw response
/// text (headers and all) rather than panicking on a non-200/fault, so the caller can assert on the
/// fault itself.
fn browse_expect_fault(port: u16, object_id: &str, flag: &str) -> String {
    let mut tcp = connect_with_retry(port, Duration::from_secs(5));
    let body = browse_soap(object_id, flag);
    let request = format!(
        "POST /dlna/cd/control HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    let (_status, _headers, resp_body) = read_http_response(&mut tcp);
    String::from_utf8(resp_body).expect("a SOAP fault must be valid UTF-8")
}

fn search_soap(container_id: &str, criteria: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
         <s:Body><u:Search xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
         <ContainerID>{container_id}</ContainerID><SearchCriteria>{criteria}</SearchCriteria>\
         <Filter>*</Filter><StartingIndex>0</StartingIndex>\
         <RequestedCount>0</RequestedCount><SortCriteria></SortCriteria>\
         </u:Search></s:Body></s:Envelope>"
    )
}

/// As [`browse`], but for a real `Search` SOAP request: sends it over a fresh, real TCP connection and
/// returns the DIDL-Lite text out of `<Result>...</Result>`, unescaped. Panics loudly (with the full
/// response) if the reply is not a well-formed `SearchResponse`.
fn search(port: u16, container_id: &str, criteria: &str) -> String {
    let mut tcp = connect_with_retry(port, Duration::from_secs(5));
    let body = search_soap(container_id, criteria);
    let request = format!(
        "POST /dlna/cd/control HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:ContentDirectory:1#Search\"\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    let (status, _headers, resp_body) = read_http_response(&mut tcp);
    let text = String::from_utf8(resp_body).expect("a Search response must be valid UTF-8");
    assert_eq!(status, 200, "a successful Search is always wrapped in a 200: {text}");
    assert!(!text.contains("<errorCode>"), "expected a SearchResponse, got a SOAP fault: {text}");

    let start =
        text.find("<Result>").expect("a Search response must carry <Result>") + "<Result>".len();
    let end = text.find("</Result>").expect("a Search response must carry </Result>");
    unescape(&text[start..end])
}

/// Same as [`search`], but for a request this test expects to be *refused* -- returns the raw response
/// text (headers and all) rather than panicking on a non-200/fault, so the caller can assert on the
/// fault itself.
fn search_expect_fault(port: u16, container_id: &str, criteria: &str) -> String {
    let mut tcp = connect_with_retry(port, Duration::from_secs(5));
    let body = search_soap(container_id, criteria);
    let request = format!(
        "POST /dlna/cd/control HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         SOAPACTION: \"urn:schemas-upnp-org:service:ContentDirectory:1#Search\"\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(request.as_bytes()).unwrap();
    let (_status, _headers, resp_body) = read_http_response(&mut tcp);
    String::from_utf8(resp_body).expect("a SOAP fault must be valid UTF-8")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Find the `id` of a `<container id="..."> ... <dc:title>TITLE</dc:title> ... </container>` (or
/// `<item ...>` with `tag = "item"`) block naming `title` in a DIDL-Lite document, without depending
/// on attribute or element order beyond what `build_didl_lite` actually emits (id, then title, inside
/// one element). Panics with the full document on a miss -- a clearer failure than an `Option` the
/// caller has to `.unwrap()` and lose all context for.
fn find_object_id(didl: &str, tag: &str, title: &str) -> String {
    let open = format!("<{tag} id=\"");
    let close = format!("</{tag}>");
    let mut search_from = 0;
    while let Some(rel) = didl[search_from..].find(&open) {
        let id_start = search_from + rel + open.len();
        let id_end = id_start + didl[id_start..].find('"').unwrap();
        let block_end = id_end + didl[id_end..].find(&close).unwrap();
        let block = &didl[id_end..block_end];
        if block.contains(&format!("<dc:title>{title}</dc:title>")) {
            return didl[id_start..id_end].to_string();
        }
        search_from = block_end + close.len();
    }
    panic!("no <{tag}> titled {title:?} found in DIDL-Lite: {didl}");
}

/// Spawns a real `lumen serve --dlna` pointed at `dir.library()` with `dir.config()` as its config
/// directory, waits for its DLNA listener's own startup line on stdout (never a fixed sleep -- flaky
/// either way, too slow in the common case and too fast under load, if guessed at), keeps draining
/// both pipes live so the server can never block on a full pipe buffer (`remote_serve.rs`'s own basic
/// test explains why in detail), and returns once the listener actually accepts a connection. The
/// exact startup sequence both tests in this file need verbatim.
fn spawn_dlna_server(dir: &TempDir, port: u16, dlna_port: u16) -> KillOnDrop {
    let library = dir.library();
    let config_dir = dir.config();
    let bin = env!("CARGO_BIN_EXE_lumen");
    let mut server = KillOnDrop(
        std::process::Command::new(bin)
            .args([
                "serve",
                library.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--dlna",
                "--dlna-port",
                &dlna_port.to_string(),
                "--dlna-bind",
                "127.0.0.1",
                "--dlna-name",
                "lumen-test",
                "--",
                "--vo=null",
                "--ao=null",
                "--force-window=no",
            ])
            .env("XDG_CONFIG_HOME", &config_dir)
            .env("APPDATA", &config_dir)
            .env("HOME", &config_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("lumen must be runnable"),
    );

    if let Some(stderr) = server.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[lumen serve stderr] {line}");
            }
        });
    }

    let stdout = server.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut dlna_ready = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        let Some(Ok(line)) = lines.next() else { break };
        println!("[lumen serve stdout] {line}");
        if line.starts_with("DLNA: advertising") {
            dlna_ready = true;
            break;
        }
    }
    assert!(dlna_ready, "the server must announce its DLNA listener on startup");

    std::thread::spawn(move || {
        for line in lines.map_while(Result::ok) {
            println!("[lumen serve stdout] {line}");
        }
    });

    // The listener itself may need a moment past the log line to actually accept connections.
    let _ = connect_with_retry(dlna_port, Duration::from_secs(10));
    server
}

#[test]
fn a_real_server_serves_a_real_nested_folder_hierarchy_over_real_soap_and_streams_real_bytes() {
    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("hierarchy");
    // A real nested tree: two top-level folders (one a show with a season, one a single file at the
    // root), plus a directory holding nothing playable -- the exact shape `LibraryTree`'s own unit
    // tests already cover in isolation, here proven end to end over the real wire protocol.
    //
    // Every file starts with the real EBML magic (`lumen_probe::magic` matches this at
    // `Confidence::Certain`; see `lumen-probe/src/magic.rs`'s own tests for the same 4-byte minimal
    // case) so `content_type_for` resolves each one as real Matroska rather than the
    // `application/octet-stream` fallback an unrecognised extension-only file would get -- DLNA
    // browsing itself never reads a file's bytes, but the streamed `Content-Type` this test also
    // checks does depend on it, and a meaningless payload would make that assertion meaningless too.
    let ebml_magic: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
    let mkv_bytes = |tag: &[u8]| [ebml_magic, tag].concat();
    let interstellar =
        dir.file("Movies/Interstellar (2014).mkv", &mkv_bytes(b"movie bytes, not a real video"));
    dir.file("Shows/Chernobyl/Season 01/Chernobyl.S01E01.mkv", &mkv_bytes(b"episode one"));
    dir.file("Shows/Chernobyl/Season 01/Chernobyl.S01E02.mkv", &mkv_bytes(b"episode two"));
    let root_file = dir.file("RootFile.mkv", &mkv_bytes(b"a file directly under the library root"));
    std::fs::create_dir_all(dir.library().join("Movies/Empty")).unwrap();

    let port = 21000 + (std::process::id() % 3000) as u16;
    let dlna_port = 24000 + (std::process::id() % 3000) as u16;
    let mut server = spawn_dlna_server(&dir, port, dlna_port);

    // 1. Root DirectChildren: two real folders (Movies, Shows) and one real file (RootFile.mkv) --
    // never the empty "Movies/Empty" directory, and never a flat list of every file in the tree.
    let root_didl = browse(dlna_port, "0", "BrowseDirectChildren");
    assert!(root_didl.contains("object.container.storageFolder"), "{root_didl}");
    let movies_id = find_object_id(&root_didl, "container", "Movies");
    let shows_id = find_object_id(&root_didl, "container", "Shows");
    assert!(
        root_didl.contains(&format!("<dc:title>{}</dc:title>", root_file_label())),
        "the root file itself must be listed directly under the root: {root_didl}"
    );
    assert!(
        !root_didl.contains("Empty"),
        "an empty directory must never appear in a real Browse response: {root_didl}"
    );
    assert!(
        !root_didl.contains("Chernobyl") && !root_didl.contains("Season 01"),
        "a nested show/season must not leak into the root's own listing: {root_didl}"
    );

    // 2. Browsing into "Movies" shows only Interstellar -- not RootFile, not anything from Shows.
    let movies_didl = browse(dlna_port, &movies_id, "BrowseDirectChildren");
    assert!(movies_didl.contains("Interstellar"), "{movies_didl}");
    assert!(!movies_didl.contains("Chernobyl"), "{movies_didl}");
    assert!(
        !movies_didl.contains("RootFile") && !movies_didl.contains(root_file_label()),
        "{movies_didl}"
    );
    assert!(!movies_didl.contains("<container"), "Movies holds no subfolders: {movies_didl}");

    // 3. Descend Shows -> Chernobyl -> Season 01, each level showing only its own real children.
    let shows_didl = browse(dlna_port, &shows_id, "BrowseDirectChildren");
    assert!(!shows_didl.contains("<item"), "Shows holds no files directly: {shows_didl}");
    let chernobyl_id = find_object_id(&shows_didl, "container", "Chernobyl");

    let chernobyl_didl = browse(dlna_port, &chernobyl_id, "BrowseDirectChildren");
    let season_id = find_object_id(&chernobyl_didl, "container", "Season 01");

    let season_didl = browse(dlna_port, &season_id, "BrowseDirectChildren");
    assert!(season_didl.contains("S01E01"), "{season_didl}");
    assert!(season_didl.contains("S01E02"), "{season_didl}");
    assert!(!season_didl.contains("<container"), "Season 01 holds no subfolders: {season_didl}");
    assert_eq!(
        season_didl.matches("<item").count(),
        2,
        "exactly the two real episodes, nothing else: {season_didl}"
    );

    // 4. Metadata on the root still reads exactly as it did before this stage, and on a real
    // directory/file id resolves to that object's own metadata -- not the root's.
    let root_meta = browse(dlna_port, "0", "BrowseMetadata");
    assert!(root_meta.contains("<dc:title>lumen</dc:title>"), "{root_meta}");
    let movies_meta = browse(dlna_port, &movies_id, "BrowseMetadata");
    assert!(movies_meta.contains("<dc:title>Movies</dc:title>"), "{movies_meta}");

    // 5. An unknown id, and the bare-integer legacy shape Stage 1 used to hand out, are both real
    // SOAP 701 faults -- not a 404, not a crash, not a silent empty result.
    for bad in ["d999", "f999", "0-legacy", "3"] {
        let fault = browse_expect_fault(dlna_port, bad, "BrowseMetadata");
        assert!(fault.contains("<errorCode>701</errorCode>"), "{bad}: {fault}");
    }

    // 6. A leaf file's own <res> URL is a real, fetchable HTTP resource that streams the real bytes on
    // disk -- the whole point of Browse existing at all. Resolve the URL from Movies's own listing
    // rather than guessing at the id scheme, exactly as a real renderer would.
    let res_url = extract_res_url(&movies_didl, "Interstellar");
    let (host_port, path) = split_url(&res_url);
    assert_eq!(
        host_port, dlna_port,
        "the <res> URL must point back at this same DLNA port: {res_url}"
    );
    let mut stream_tcp = connect_with_retry(dlna_port, Duration::from_secs(5));
    stream_tcp
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{dlna_port}\r\n\r\n").as_bytes())
        .unwrap();
    let (status, headers, body) = read_http_response(&mut stream_tcp);
    assert_eq!(status, 200, "streaming a real file's <res> URL must succeed");
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("matroska")),
        "expected a Matroska content-type from the sniffed/extension-derived MIME type: {headers:?}"
    );
    assert_eq!(
        body,
        std::fs::read(&interstellar).unwrap(),
        "the streamed bytes must match the real file on disk exactly"
    );

    // And the file living directly under the root streams correctly too, proving the "attaches
    // straight to the root" path end to end, not just Movies's nested one.
    let root_res_url = extract_res_url(&root_didl, root_file_label());
    let (_host_port, root_path) = split_url(&root_res_url);
    let mut root_stream_tcp = connect_with_retry(dlna_port, Duration::from_secs(5));
    root_stream_tcp
        .write_all(
            format!("GET {root_path} HTTP/1.1\r\nHost: 127.0.0.1:{dlna_port}\r\n\r\n").as_bytes(),
        )
        .unwrap();
    let (status, _headers, body) = read_http_response(&mut root_stream_tcp);
    assert_eq!(status, 200);
    assert_eq!(body, std::fs::read(&root_file).unwrap());

    // 7. Search(MatchAll) from the root finds every real file, including the deeply nested episodes
    // two directories down, and never the empty directory -- and, since Search answers "find these
    // items", never emits a <container> element for Movies/Shows/Season 01 either.
    let all_files_didl = search(dlna_port, "0", "*");
    for expected in ["Interstellar", "S01E01", "S01E02", root_file_label()] {
        assert!(
            all_files_didl.contains(expected),
            "{expected} missing from a real Search(*) from the root: {all_files_didl}"
        );
    }
    assert!(
        !all_files_didl.contains("Empty"),
        "an empty directory must never appear in a real Search response: {all_files_didl}"
    );
    assert!(
        !all_files_didl.contains("<container"),
        "Search results must be items only, never containers: {all_files_didl}"
    );

    // 8. Search(dc:title contains "chernobyl") finds only the two real episodes, matched
    // case-insensitively, never Interstellar and never the unrelated root file.
    let chernobyl_search_didl = search(dlna_port, "0", "dc:title contains \"chernobyl\"");
    assert!(chernobyl_search_didl.contains("S01E01"), "{chernobyl_search_didl}");
    assert!(chernobyl_search_didl.contains("S01E02"), "{chernobyl_search_didl}");
    assert!(!chernobyl_search_didl.contains("Interstellar"), "{chernobyl_search_didl}");
    assert!(!chernobyl_search_didl.contains(root_file_label()), "{chernobyl_search_didl}");

    // 9. Searching from "Shows" instead of the root still finds both episodes two levels further down
    // (Shows -> Chernobyl -> Season 01) -- proving the recursion genuinely walks from the *named*
    // container, not always from the root -- and, scoped there, never returns Interstellar or the
    // root file, which a container-blind ("accidentally global") search would.
    let shows_search_didl = search(dlna_port, &shows_id, "*");
    assert!(shows_search_didl.contains("S01E01"), "{shows_search_didl}");
    assert!(shows_search_didl.contains("S01E02"), "{shows_search_didl}");
    assert!(!shows_search_didl.contains("Interstellar"), "{shows_search_didl}");
    assert!(!shows_search_didl.contains(root_file_label()), "{shows_search_didl}");

    // 10. An unknown container_id is a real 701 SOAP fault over the wire, exactly like Browse.
    let search_fault = search_expect_fault(dlna_port, "d999", "*");
    assert!(search_fault.contains("<errorCode>701</errorCode>"), "{search_fault}");

    let _ = server.kill();
    let _ = server.wait();
}

/// Polls a real `Browse(object_id, DirectChildren)` until its DIDL-Lite names `title`, or `timeout`
/// elapses -- returning the full `BrowseResponse` that first showed it (`None` on timeout), so the
/// caller can read the `UpdateID` that very response carried. A deadline loop, not a sleep-and-hope:
/// the watcher's timing (debounce, walk, quiet period) is the server's to decide and this test's only
/// to observe, the same convention `remote_serve.rs`'s automatic-rescan tests already follow.
fn wait_for_browse_to_list(
    port: u16,
    object_id: &str,
    title: &str,
    timeout: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let response = browse_response(port, object_id, "BrowseDirectChildren");
        if unescape(&response).contains(&format!("<dc:title>{title}</dc:title>")) {
            return Some(response);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// Every `SystemUpdateID` value `GetSystemUpdateID` reports over `window` that is strictly greater
/// than `baseline` (the value the caller has already confirmed is current), in increasing order --
/// the same "distinct versions beyond baseline" idea `remote_serve.rs`'s
/// `distinct_library_versions_over` uses to tell "one coalesced refresh, then quiet" apart from "the
/// watcher keeps retriggering itself". Empty means the counter never moved past what was already
/// confirmed.
fn system_update_ids_beyond(port: u16, baseline: u32, window: Duration) -> Vec<u32> {
    let deadline = std::time::Instant::now() + window;
    let mut max_seen = baseline;
    let mut beyond = Vec::new();
    while std::time::Instant::now() < deadline {
        let id = get_system_update_id(port);
        if id > max_seen {
            beyond.push(id);
            max_seen = id;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    beyond
}

/// Proves the DLNA half of `docs/15` §A's phase-2 item end to end, with no client-side trigger of any
/// kind: a real `lumen serve --dlna`, a real `GetSystemUpdateID` over SOAP at its starting value, a
/// file dropped into the real library directory on disk, and then -- driven by nothing but the
/// server's own filesystem watcher -- a real `Browse` that lists the new file, carrying an `UpdateID`
/// that moved, agreeing with what `GetSystemUpdateID` now reports. Then the negative half: once the
/// change has settled, the counter must stay put (no self-triggered refresh loop -- the exact bug the
/// watcher's `Access` filter and post-rescan quiet period exist to close, and one that two watchers on
/// the same tree, each re-reading every file, would be a fresh opportunity to reintroduce). Finally a
/// second drop after that quiet period proves the watcher is still alive, not one-shot.
///
/// Deadlines are generous on purpose: the watcher waits `WATCHER_DEBOUNCE` (1.5s) past the last
/// event before re-walking, and ignores everything for a further `POST_RESCAN_QUIET_PERIOD` (1.5s)
/// after -- so the first appearance can legitimately take ~2s, and a second drop landing inside the
/// quiet window is only picked up on the *next* trigger. 15s bounds each wait without ever being the
/// thing that decides the outcome.
#[test]
fn a_file_dropped_into_the_library_appears_in_browse_and_moves_system_update_id_unprompted() {
    if !mpv_on_path() {
        eprintln!("skipping: mpv is not on PATH in this environment");
        return;
    }

    let dir = TempDir::new("auto-refresh");
    // Dummy bytes are enough -- `scan::scan` classifies by extension and name (see the other test in
    // this file, and `remote_serve.rs`'s own watcher tests). Each file's bytes are distinct so that
    // step 2's "the URL still streams the *same* file" check cannot pass by accident.
    let existing_bytes: &[u8] = b"EXISTING: not a real container, just needs to exist";
    dir.file("Movies/Existing.mkv", existing_bytes);

    // A port range disjoint from the hierarchy test above (21000..23999 / 24000..26999), since both
    // run concurrently in this one test process.
    let port = 38000 + (std::process::id() % 3000) as u16;
    let dlna_port = 41000 + (std::process::id() % 3000) as u16;
    let mut server = spawn_dlna_server(&dir, port, dlna_port);

    // 1. Baseline: the starting SystemUpdateID (1, the constant every UpdateID always was), reported
    // both by GetSystemUpdateID and by a Browse's own UpdateID -- one counter, not two.
    let baseline = get_system_update_id(dlna_port);
    assert_eq!(baseline, 1, "a fresh server starts at the value clients always saw");
    let root_before = browse_response(dlna_port, "0", "BrowseDirectChildren");
    assert_eq!(leaf_u32(&root_before, "UpdateID"), baseline, "{root_before}");
    let movies_id = find_object_id(&unescape(&root_before), "container", "Movies");
    let movies_before = browse(dlna_port, &movies_id, "BrowseDirectChildren");
    assert!(movies_before.contains("<dc:title>Existing</dc:title>"), "{movies_before}");
    assert!(!movies_before.contains("AutoDetected"), "{movies_before}");
    // The <res> URL a renderer would now be playing Existing from -- kept across the refresh below.
    let (_, existing_path) = split_url(&extract_res_url(&movies_before, "Existing"));

    // 2. Drop a new file into the real library directory. No request of any kind tells the server
    // about it -- if Browse ever lists it, the watcher is what made that happen. The name sorts
    // *before* Existing's, so a position-based id scheme would have shifted Existing's id here.
    dir.file("Movies/AutoDetected.mkv", b"AUTODETECTED: not a real container, just needs to exist");

    let appeared =
        wait_for_browse_to_list(dlna_port, &movies_id, "AutoDetected", Duration::from_secs(15))
            .expect("the new file must appear in a real Browse with no client-side trigger");
    let update_id_when_appeared = leaf_u32(&appeared, "UpdateID");

    // The URL handed out before the refresh, re-requested the way a renderer seeks -- a fresh
    // connection with a Range header -- must still stream Existing's own bytes, not the newcomer's
    // that now sorts ahead of it. This is the live, over-the-wire form of `dlna.rs`'s own
    // `a_stream_url_handed_out_before_a_refresh_still_serves_the_same_file_after_it`.
    let mut seek = connect_with_retry(dlna_port, Duration::from_secs(5));
    seek.write_all(
        format!(
            "GET {existing_path} HTTP/1.1\r\nHost: 127.0.0.1:{dlna_port}\r\nRange: bytes=9-\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let (status, _headers, body) = read_http_response(&mut seek);
    assert_eq!(status, 206, "a Range re-request of a still-present file is a real 206");
    assert_eq!(
        body,
        &existing_bytes[9..],
        "the stream URL a renderer was already playing must name the same file after a refresh"
    );
    assert_eq!(
        split_url(&extract_res_url(&unescape(&appeared), "Existing")).1,
        existing_path,
        "and the refreshed listing still advertises Existing at that same URL"
    );
    assert!(
        update_id_when_appeared > baseline,
        "the Browse that first showed the new file must already carry a moved UpdateID -- the \
         snapshot and the counter are swapped together, never one before the other: {appeared}"
    );
    assert_eq!(
        get_system_update_id(dlna_port),
        update_id_when_appeared,
        "GetSystemUpdateID must report the same value the Browse just carried"
    );
    assert_eq!(
        update_id_when_appeared,
        baseline + 1,
        "one file dropped in one burst is exactly one refresh, not one per raw filesystem event"
    );

    // 3. Nothing further changes on disk, so the counter must not keep climbing: the refresh's own
    // re-read of every file (and the paired channel's own watcher doing the same on the same tree
    // whenever *it* fires -- it is armed only once mpv is up, so it may or may not have caught this
    // first drop) must never look like a fresh external change. 6s comfortably spans the 1.5s quiet
    // period plus another full debounce window in which a self-triggered event would have fired.
    // The server's own config-directory writes cannot be what moves the counter here either -- the
    // fixture keeps them outside the watched root (see `TempDir`).
    let after_settling =
        system_update_ids_beyond(dlna_port, update_id_when_appeared, Duration::from_secs(6));
    assert!(
        after_settling.is_empty(),
        "SystemUpdateID must not keep climbing once nothing further has changed on disk: saw \
         {after_settling:?} beyond {update_id_when_appeared}"
    );

    // 4. The watcher is still alive after its quiet period: a second drop is picked up too, and
    // advances the counter by exactly one more.
    dir.file("Movies/SecondDrop.mkv", b"not a real container, just needs to exist");
    let second =
        wait_for_browse_to_list(dlna_port, &movies_id, "SecondDrop", Duration::from_secs(15))
            .expect("a second file dropped after the quiet period must be picked up as well");
    assert_eq!(leaf_u32(&second, "UpdateID"), update_id_when_appeared + 1, "{second}");
    assert_eq!(get_system_update_id(dlna_port), update_id_when_appeared + 1);
    let movies_final = unescape(&second);
    for expected in ["Existing", "AutoDetected", "SecondDrop"] {
        assert!(
            movies_final.contains(&format!("<dc:title>{expected}</dc:title>")),
            "{expected} missing from the final Movies listing: {movies_final}"
        );
    }

    let _ = server.kill();
    let _ = server.wait();
}

/// `RootFile.mkv` has no year, episode, or other parseable metadata in its name, so
/// `lumen_match::parse` (via `ScannedFile::label`) leaves the title as the bare filename minus
/// extension -- `lumen_match`'s own parsing rules are exercised and asserted on elsewhere; this test
/// only needs to name the one label it actually produces for this one deliberately plain filename.
fn root_file_label() -> &'static str {
    "RootFile"
}

/// Pull the `<res ...>URL</res>` text out of the DIDL-Lite block for the item titled `title` -- the
/// URL a real renderer would fetch to actually play the file.
fn extract_res_url(didl: &str, title: &str) -> String {
    let title_marker = format!("<dc:title>{title}");
    let title_pos = didl.find(&title_marker).unwrap_or_else(|| {
        panic!("no item titled {title:?} found in DIDL-Lite: {didl}");
    });
    let res_open = didl[title_pos..].find("<res ").map(|i| title_pos + i).expect("a res element");
    let gt = didl[res_open..].find('>').map(|i| res_open + i + 1).unwrap();
    let close = didl[gt..].find("</res>").map(|i| gt + i).unwrap();
    didl[gt..close].to_string()
}

/// Split `http://host:port/path` into the port and the `/path...` portion, so a test can reconnect to
/// exactly the URL a real DIDL-Lite `<res>` element actually named rather than assuming it matches
/// whatever port this test already happens to be using.
fn split_url(url: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("a res URL must be a plain http:// URL");
    let (authority, path) = rest.split_once('/').expect("a res URL must carry a path");
    let port: u16 =
        authority.split(':').nth(1).expect("a res URL must name a port").parse().unwrap();
    (port, format!("/{path}"))
}
