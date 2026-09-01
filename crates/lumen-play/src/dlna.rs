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
//! **Stage 2: a real folder hierarchy.** Every playable file is listed under the directory it actually
//! lives in on disk, not flattened into one giant list under the root container. A [`LibraryTree`],
//! built once at startup from the same [`Scan`] and the library root, walks/creates a container node
//! for every ancestor directory a playable file is actually found under -- an empty directory never
//! gets a node, matching `Scan::playable`'s own "only report what's actually playable" posture, not a
//! second filter bolted on afterward.
//!
//! Object IDs are `"0"` for the UPnP-mandated root, `"d<n>"` for a directory, and `"f<n>"` for a file
//! (`<n>` is that file's index into `Scan::playable()`'s own enumeration) -- three disjoint prefixes,
//! so no two distinct objects can ever share an id (see [`LibraryTree`]'s own doc for the bare-integer
//! collision this replaced). IDs are stable only for the lifetime of one `lumen serve` process (the
//! same "one snapshot taken at startup, never refreshed" limitation `docs/15` Engine A already
//! documents for the paired control channel's own library listing -- this shares that gap rather than
//! inventing a second one).

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
    // Canonicalized so every playable file's own path (built by `scan` walking from exactly this root)
    // and this root agree byte-for-byte when `LibraryTree::build` computes each file's directory
    // ancestry below -- a mismatch here (a relative root, a trailing separator, a symlink) would mean
    // `strip_prefix` fails and every file falls back to attaching straight to the root, silently
    // flattening the very hierarchy this stage exists to build. Falls back to the path as given if
    // canonicalization itself fails (e.g. the root vanished between `lumen serve`'s own existence
    // check and here) rather than refusing to serve at all.
    let library_root = library_root.canonicalize().unwrap_or(library_root);

    let scan = crate::scan::scan(
        std::slice::from_ref(&library_root),
        &crate::scan::ScanOptions::default(),
    );
    let tree = LibraryTree::build(&scan, &library_root);
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

    let ctx = Arc::new(DlnaContext { library, tree, base_url, friendly_name, uuid });
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle_connection(stream, &ctx));
    }
    Ok(())
}

struct DlnaContext {
    library: Arc<Mutex<Scan>>,
    /// The library's real folder hierarchy -- built once in [`run`] from the same startup `Scan` as
    /// `library` above. Never mutated after construction, sharing the "one snapshot taken at startup,
    /// never refreshed" limitation `library` itself already carries, so a `Mutex` here would only add
    /// contention this design has no use for.
    tree: LibraryTree,
    base_url: String,
    friendly_name: String,
    /// Generated once in [`run`] and carried here so every `desc.xml` response agrees with the UUID
    /// already baked into the SSDP announcements and the response's own LOCATION URL.
    uuid: String,
}

/// One directory in the served library's own folder hierarchy -- a DLNA `StorageFolder` container.
///
/// `LibraryTree::dirs[0]` is always the root, with the UPnP-mandated fixed id `"0"`; every other
/// node's id is `"d<n>"`, where `<n>` is an incrementing counter assigned in creation order by
/// [`LibraryTree::build`].
#[derive(Debug)]
struct DirNode {
    id: String,
    parent_id: String,
    /// The directory's own basename. Empty for the root -- unused, since `BrowseMetadata("0")`
    /// already hardcodes the root's title as `"lumen"` in `handle_content_directory_control`.
    name: String,
    /// Indices into the owning [`LibraryTree`]'s own `dirs`.
    child_dirs: Vec<usize>,
    /// Indices into `scan.playable()`'s own enumeration -- the same ordering
    /// `handle_content_directory_control` already built its (Stage 1, flat) file list from, so a
    /// file's object id keeps meaning what it already meant.
    child_files: Vec<usize>,
}

/// The served library's real folder hierarchy, built once at startup (see [`run`]) from the same
/// [`Scan`] and library root every playable file was found under.
///
/// **Why this exists, not just a flat list**: a smart TV's Browse UI should show `Movies`, a show's
/// own folder, `Season 01`, and so on -- not one giant flat list of every file in the collection.
/// Building this once, rather than walking the filesystem again per request, matches this module's
/// existing "one snapshot at startup" posture for the library itself.
///
/// **Fixes a real, latent id-collision bug along the way.** Stage 1 gave playable files bare-integer
/// object ids ("0", "1", ...) -- the same string space the root container's own fixed id ("0") lives
/// in. A client asking `Browse("0", BrowseMetadata)` to inspect "the object with id 0" could never
/// actually reach file index 0's metadata; the root-vs-file check in
/// `handle_content_directory_control` ran first and always won, no matter what the client actually
/// meant. Every directory now gets a `"d<n>"` id and every file an `"f<n>"` id -- three disjoint
/// prefixes (`"0"`, `"d..."`, `"f..."`), so no two distinct objects can ever share a string again.
#[derive(Debug)]
struct LibraryTree {
    dirs: Vec<DirNode>,
    /// `DirNode::id` -> index into `dirs`, for every node including the root -- `O(1)` id lookups for
    /// both `Browse` flags, instead of a linear scan per request.
    by_id: HashMap<String, usize>,
    /// The parent directory's own id, for each playable file -- indexed exactly the way
    /// `scan.playable()`'s own enumeration (and therefore each file's own `"f<n>"` id) already is.
    /// Looked up only by a `Browse(Metadata)` naming a bare file id directly; every `DirectChildren`
    /// listing already has its directory's id in hand from the [`DirNode`] being listed, so it never
    /// needs this.
    file_parent: Vec<String>,
}

impl LibraryTree {
    /// Build the hierarchy. A directory node is created only while walking an actual playable file's
    /// own ancestor chain, memoized by that directory's path relative to `library_root` so the same
    /// real directory -- reached as an ancestor of two different playable files -- is only ever
    /// created once. A directory nothing playable lives under (recursively) is therefore never
    /// visited at all, and so never gets a node -- the "empty directories are never listed" guarantee
    /// falls straight out of this construction rather than needing a second, separate check.
    fn build(scan: &Scan, library_root: &Path) -> Self {
        let mut dirs = vec![DirNode {
            id: "0".to_string(),
            parent_id: "-1".to_string(),
            name: String::new(),
            child_dirs: Vec::new(),
            child_files: Vec::new(),
        }];
        let mut by_id: HashMap<String, usize> = HashMap::new();
        by_id.insert("0".to_string(), 0);
        let mut by_rel_dir: HashMap<PathBuf, usize> = HashMap::new();
        let mut next_dir_id: usize = 0;
        let mut file_parent = Vec::new();

        for f in scan.playable() {
            // The ancestor directory names between `library_root` and this file, outermost first. A
            // file directly under `library_root` -- or, defensively, one `strip_prefix` cannot relate
            // to `library_root` at all (should not happen, since `scan` was pointed at exactly this
            // root, but a fallback beats a panic) -- has none, and attaches straight to the root.
            let components: Vec<String> = f
                .path
                .strip_prefix(library_root)
                .ok()
                .and_then(Path::parent)
                .map(|dir| {
                    dir.components()
                        .filter_map(|c| match c {
                            std::path::Component::Normal(s) => {
                                Some(s.to_string_lossy().into_owned())
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut parent_idx = 0usize;
            let mut rel = PathBuf::new();
            for name in &components {
                rel.push(name);
                parent_idx = match by_rel_dir.get(&rel) {
                    Some(&idx) => idx,
                    None => {
                        let id = format!("d{next_dir_id}");
                        next_dir_id += 1;
                        dirs.push(DirNode {
                            id: id.clone(),
                            parent_id: dirs[parent_idx].id.clone(),
                            name: name.clone(),
                            child_dirs: Vec::new(),
                            child_files: Vec::new(),
                        });
                        let new_idx = dirs.len() - 1;
                        dirs[parent_idx].child_dirs.push(new_idx);
                        by_id.insert(id, new_idx);
                        by_rel_dir.insert(rel.clone(), new_idx);
                        new_idx
                    }
                };
            }
            dirs[parent_idx].child_files.push(file_parent.len());
            file_parent.push(dirs[parent_idx].id.clone());
        }

        Self { dirs, by_id, file_parent }
    }

    fn dir_by_id(&self, id: &str) -> Option<&DirNode> {
        self.by_id.get(id).map(|&i| &self.dirs[i])
    }
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
    let tree = &ctx.tree;

    let (objects, total) = match browse.flag {
        BrowseFlag::DirectChildren => {
            let Some(dir) = tree.dir_by_id(&browse.object_id) else {
                write_soap_fault(tcp, 701, "No such object");
                return;
            };
            // Folders first, then files -- a reasonable, common convention; the spec mandates neither
            // order, so this only needs to be consistent with itself, and it is: it is the only order
            // this responder ever produces.
            let mut combined: Vec<DidlObject> =
                Vec::with_capacity(dir.child_dirs.len() + dir.child_files.len());
            combined.extend(dir.child_dirs.iter().map(|&idx| {
                let d = &tree.dirs[idx];
                DidlObject {
                    id: d.id.clone(),
                    parent_id: d.parent_id.clone(),
                    title: d.name.clone(),
                    class: ObjectClass::StorageFolder,
                    resource: None,
                }
            }));
            combined.extend(dir.child_files.iter().filter_map(|&i| {
                files.get(i).map(|f| DidlObject {
                    id: format!("f{i}"),
                    parent_id: dir.id.clone(),
                    title: f.label(),
                    class: object_class_for(f.container),
                    resource: Some(DidlResource {
                        url: format!("{}/dlna/stream/f{i}", ctx.base_url),
                        mime_type: content_type_for(f.container, f.extension.as_deref())
                            .to_string(),
                        size_bytes: Some(f.size),
                    }),
                })
            }));

            // Pagination is over the COMBINED dirs-then-files sequence, not files alone -- a client
            // paging through a large folder must see a stable, single sequence, not two independently
            // restarting ones.
            let total = combined.len();
            let start = browse.starting_index as usize;
            let count =
                if browse.requested_count == 0 { total } else { browse.requested_count as usize };
            (combined.into_iter().skip(start).take(count).collect(), total)
        }
        BrowseFlag::Metadata if browse.object_id == "0" => (
            vec![DidlObject {
                id: "0".into(),
                parent_id: "-1".into(),
                title: "lumen".into(),
                class: ObjectClass::StorageFolder,
                resource: None,
            }],
            1,
        ),
        BrowseFlag::Metadata => {
            if let Some(d) = tree.dir_by_id(&browse.object_id) {
                (
                    vec![DidlObject {
                        id: d.id.clone(),
                        parent_id: d.parent_id.clone(),
                        title: d.name.clone(),
                        class: ObjectClass::StorageFolder,
                        resource: None,
                    }],
                    1,
                )
            } else if let Some(i) =
                browse.object_id.strip_prefix('f').and_then(|s| s.parse::<usize>().ok())
            {
                match (files.get(i), tree.file_parent.get(i)) {
                    (Some(f), Some(parent_id)) => (
                        vec![DidlObject {
                            id: format!("f{i}"),
                            parent_id: parent_id.clone(),
                            title: f.label(),
                            class: object_class_for(f.container),
                            resource: Some(DidlResource {
                                url: format!("{}/dlna/stream/f{i}", ctx.base_url),
                                mime_type: content_type_for(f.container, f.extension.as_deref())
                                    .to_string(),
                                size_bytes: Some(f.size),
                            }),
                        }],
                        1,
                    ),
                    _ => {
                        write_soap_fault(tcp, 701, "No such object");
                        return;
                    }
                }
            } else {
                // Neither a known directory nor a known file -- an unknown "d<n>"/"f<n>" index, or a
                // bare-numeric legacy id from Stage 1's now-retired flat scheme.
                write_soap_fault(tcp, 701, "No such object");
                return;
            }
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
    // `/dlna/stream/<id>` URLs are built with the same "f<n>" ids `build_didl_lite`-driven items carry
    // (see `handle_content_directory_control`) -- strip that prefix before the numeric lookup so this
    // parses exactly the ids this responder itself hands out.
    let Some(f) = id
        .strip_prefix('f')
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|i| scan.playable().nth(i))
    else {
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

    /// A directory that deletes itself. No tempfile crate, and a test that leaks directories into the
    /// user's tree is its own bug -- matches `scan.rs`'s own private helper of the same shape, which
    /// this module cannot reuse across a `pub(crate)`-free module boundary.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let anchor = 0u8;
            let dir = std::env::temp_dir().join(format!(
                "lumen-dlna-{tag}-{}-{:x}",
                std::process::id(),
                std::ptr::from_ref(&anchor) as usize
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
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

    /// Scans a fresh temp directory built from `files` (relative path, content -- content is
    /// irrelevant here since these tests are about the hierarchy `LibraryTree::build` derives from
    /// paths alone, not container sniffing) and builds its `LibraryTree`, mirroring the two-step
    /// `run` itself performs. The root is canonicalized before either step, matching `run`'s own
    /// contract that `LibraryTree::build` is only ever handed a canonical root.
    fn scan_tree(tag: &str, files: &[(&str, &[u8])]) -> (TempDir, PathBuf, Scan, LibraryTree) {
        let d = TempDir::new(tag);
        for (rel, bytes) in files {
            d.file(rel, bytes);
        }
        let root = d.0.canonicalize().unwrap();
        let scan =
            crate::scan::scan(std::slice::from_ref(&root), &crate::scan::ScanOptions::default());
        let tree = LibraryTree::build(&scan, &root);
        (d, root, scan, tree)
    }

    fn test_context(tree: LibraryTree, scan: Scan) -> DlnaContext {
        DlnaContext {
            library: Arc::new(Mutex::new(scan)),
            tree,
            base_url: "http://127.0.0.1:7891".to_string(),
            friendly_name: "test".to_string(),
            uuid: "00000000-0000-4000-8000-000000000000".to_string(),
        }
    }

    /// The inverse of `lumen_discovery::content_directory`'s own (private) `escape_xml` -- duplicated
    /// here, rather than depending on another crate's internals, purely so these tests can assert
    /// against readable DIDL-Lite text instead of its doubly-escaped form inside `<Result>`.
    fn unescape(s: &str) -> String {
        s.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }

    fn browse_soap(object_id: &str, flag: &str, starting_index: u32, count: u32) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
             <s:Body><u:Browse xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <ObjectID>{object_id}</ObjectID><BrowseFlag>{flag}</BrowseFlag>\
             <Filter>*</Filter><StartingIndex>{starting_index}</StartingIndex>\
             <RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>\
             </u:Browse></s:Body></s:Envelope>"
        )
    }

    /// Drives `handle_content_directory_control` itself -- the real dispatch code, not a
    /// reimplementation of its logic -- over a real loopback TCP connection, the same transport a real
    /// control point's request actually arrives on. The inbound `HttpRequest` is built by hand rather
    /// than round-tripped through `HttpRequest::parse`, since header parsing already has its own
    /// dedicated tests elsewhere in this file; what these tests exercise is the Browse dispatch itself.
    fn browse(
        ctx: &DlnaContext,
        object_id: &str,
        flag: &str,
        starting_index: u32,
        count: u32,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let reader = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            let mut resp = Vec::new();
            stream.read_to_end(&mut resp).unwrap();
            String::from_utf8_lossy(&resp).into_owned()
        });

        let (mut server_stream, _) = listener.accept().unwrap();
        let mut headers = HashMap::new();
        headers.insert(
            "soapaction".to_string(),
            "\"urn:schemas-upnp-org:service:ContentDirectory:1#Browse\"".to_string(),
        );
        let req = HttpRequest { method: "POST".into(), path: "/dlna/cd/control".into(), headers };
        let body = browse_soap(object_id, flag, starting_index, count);
        handle_content_directory_control(&mut server_stream, &req, body.as_bytes(), ctx);
        // The response is only complete once the server side has finished writing and this handle is
        // dropped -- `write_ok`/`write_soap_fault` never call `shutdown` themselves, so the reader
        // thread's `read_to_end` would otherwise block forever waiting for a EOF nothing produces.
        drop(server_stream);

        reader.join().unwrap()
    }

    /// Pulls the DIDL-Lite text back out of a `Browse` response's `<Result>...</Result>`, unescaped so
    /// assertions can read it directly (`build_browse_response`'s own doc explains why it is escaped
    /// once more on the way in).
    fn result_didl(response: &str) -> String {
        let start = response.find("<Result>").expect("a Browse response must carry <Result>")
            + "<Result>".len();
        let end = response.find("</Result>").expect("a Browse response must carry </Result>");
        unescape(&response[start..end])
    }

    #[test]
    fn library_tree_builds_nested_directories_and_never_lists_an_empty_one() {
        let d = TempDir::new("nested");
        d.file("Movies/Show/Season 01/S01E01.mkv", b"a");
        d.file("Movies/Show/Season 01/S01E02.mkv", b"b");
        d.file("Movies/OtherShow/Season 01/S01E01.mkv", b"c");
        d.file("Root.mkv", b"d");
        std::fs::create_dir_all(d.0.join("Movies/Empty")).unwrap();

        let root = d.0.canonicalize().unwrap();
        let scan =
            crate::scan::scan(std::slice::from_ref(&root), &crate::scan::ScanOptions::default());
        let tree = LibraryTree::build(&scan, &root);

        let root_node = tree.dir_by_id("0").unwrap();
        assert_eq!(root_node.child_files.len(), 1, "Root.mkv attaches directly to the root");
        assert_eq!(root_node.child_dirs.len(), 1, "only Movies sits directly under the root");

        let movies = &tree.dirs[root_node.child_dirs[0]];
        assert_eq!(movies.name, "Movies");
        assert_eq!(movies.parent_id, "0");
        assert!(movies.child_files.is_empty(), "Movies itself holds no files directly");
        let names: Vec<&str> =
            movies.child_dirs.iter().map(|&i| tree.dirs[i].name.as_str()).collect();
        assert!(names.contains(&"Show"), "{names:?}");
        assert!(names.contains(&"OtherShow"), "{names:?}");
        assert!(
            !names.contains(&"Empty"),
            "a directory with nothing playable anywhere under it must never get a node: {names:?}"
        );
        assert_eq!(movies.child_dirs.len(), 2, "Show and OtherShow, never Empty: {names:?}");

        let show_idx = *movies.child_dirs.iter().find(|&&i| tree.dirs[i].name == "Show").unwrap();
        let show = &tree.dirs[show_idx];
        assert_eq!(
            show.child_dirs.len(),
            1,
            "the two S01E0x files under it must share one Season 01 node, not create two"
        );
        let season = &tree.dirs[show.child_dirs[0]];
        assert_eq!(season.name, "Season 01");
        assert_eq!(season.parent_id, show.id);
        assert_eq!(season.child_files.len(), 2);

        // Season 01 exists twice on disk (once under Show, once under OtherShow) and must be two
        // distinct nodes, not merged -- they are different real directories that only share a name.
        assert_eq!(
            tree.dirs.len(),
            6,
            "root + Movies + Show + OtherShow + two distinct Season 01s"
        );
    }

    #[test]
    fn root_direct_children_lists_directories_before_files_with_correct_ids() {
        let (_d, _root, scan, tree) = scan_tree(
            "root-children",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("Shows/Show/Season 01/S01E02.mkv", b"c"),
                ("RootFile.mkv", b"d"),
            ],
        );
        let ctx = test_context(tree, scan);

        let response = browse(&ctx, "0", "BrowseDirectChildren", 0, 0);
        assert!(response.contains("<NumberReturned>3</NumberReturned>"), "{response}");
        assert!(response.contains("<TotalMatches>3</TotalMatches>"), "{response}");

        let didl = result_didl(&response);
        assert!(didl.contains("<container id=\"d0\" parentID=\"0\""), "{didl}");
        assert!(didl.contains("<container id=\"d1\" parentID=\"0\""), "{didl}");
        assert!(didl.contains("<item id=\"f1\" parentID=\"0\""), "{didl}");

        let last_dir_pos = didl.rfind("<container").unwrap();
        let first_item_pos = didl.find("<item").unwrap();
        assert!(
            last_dir_pos < first_item_pos,
            "every folder must be listed before every file: {didl}"
        );
    }

    #[test]
    fn a_directorys_direct_children_returns_only_its_own_children() {
        let (_d, _root, scan, tree) = scan_tree(
            "dir-children",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("RootFile.mkv", b"d"),
            ],
        );
        // "Shows" is the second directory created (Movies is created first, from the
        // alphabetically-earlier `Movies/Interstellar.mkv`), so it is "d1".
        let ctx = test_context(tree, scan);

        let response = browse(&ctx, "d1", "BrowseDirectChildren", 0, 0);
        assert!(response.contains("<NumberReturned>1</NumberReturned>"), "{response}");
        let didl = result_didl(&response);
        assert!(didl.contains("<container id=\"d2\" parentID=\"d1\""), "{didl}");
        // Nothing belonging to the root or to Movies must leak into Shows's own listing.
        assert!(!didl.contains("id=\"d0\""), "{didl}");
        assert!(!didl.contains("id=\"f0\""), "{didl}");
        assert!(!didl.contains("id=\"f1\""), "{didl}");
    }

    #[test]
    fn metadata_resolves_the_root_a_directory_and_a_file_and_rejects_every_unknown_shape() {
        let (_d, _root, scan, tree) =
            scan_tree("metadata", &[("Movies/Interstellar.mkv", b"a"), ("RootFile.mkv", b"d")]);
        let ctx = test_context(tree, scan);

        let root_didl = result_didl(&browse(&ctx, "0", "BrowseMetadata", 0, 0));
        assert!(root_didl.contains("<container id=\"0\" parentID=\"-1\""), "{root_didl}");
        assert!(root_didl.contains("<dc:title>lumen</dc:title>"), "{root_didl}");

        let dir_didl = result_didl(&browse(&ctx, "d0", "BrowseMetadata", 0, 0));
        assert!(dir_didl.contains("<container id=\"d0\" parentID=\"0\""), "{dir_didl}");
        assert!(dir_didl.contains("<dc:title>Movies</dc:title>"), "{dir_didl}");

        let file_didl = result_didl(&browse(&ctx, "f0", "BrowseMetadata", 0, 0));
        assert!(file_didl.contains("<item id=\"f0\" parentID=\"d0\""), "{file_didl}");

        for unknown in ["d99", "f99", "3", "not-an-id"] {
            let response = browse(&ctx, unknown, "BrowseMetadata", 0, 0);
            assert!(
                response.contains("<errorCode>701</errorCode>"),
                "{unknown} must be a 701 fault: {response}"
            );
        }
    }

    #[test]
    fn the_bare_integer_id_collision_stage_1_had_is_fixed() {
        // Under Stage 1's scheme, this single file (the only playable file, sorted first) would have
        // been object id "0" -- the exact same string as the root container's own fixed id. A client
        // asking `Browse("0", Metadata)` could only ever reach the root, never this file. Confirm both
        // ids now resolve to their own, distinct object.
        let (_d, _root, scan, tree) = scan_tree("collision", &[("OnlyFile.mkv", b"x")]);
        let ctx = test_context(tree, scan);

        let root_didl = result_didl(&browse(&ctx, "0", "BrowseMetadata", 0, 0));
        assert!(root_didl.contains("<dc:title>lumen</dc:title>"), "{root_didl}");
        assert!(root_didl.contains("object.container.storageFolder"), "{root_didl}");

        let file_didl = result_didl(&browse(&ctx, "f0", "BrowseMetadata", 0, 0));
        assert!(file_didl.contains("<item id=\"f0\" parentID=\"0\""), "{file_didl}");
        assert!(
            !file_didl.contains("object.container.storageFolder"),
            "the file's own metadata must never read as the root container: {file_didl}"
        );
    }

    #[test]
    fn pagination_applies_over_the_combined_directories_then_files_sequence() {
        let (_d, _root, scan, tree) = scan_tree(
            "paginate",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("RootFile.mkv", b"d"),
            ],
        );
        // Root's combined DirectChildren sequence is [Movies(d0), Shows(d1), RootFile(f1)] -- 3 total.
        let ctx = test_context(tree, scan);

        let response = browse(&ctx, "0", "BrowseDirectChildren", 1, 1);
        assert!(response.contains("<NumberReturned>1</NumberReturned>"), "{response}");
        assert!(
            response.contains("<TotalMatches>3</TotalMatches>"),
            "TotalMatches must reflect the combined count, not just files: {response}"
        );
        let didl = result_didl(&response);
        assert!(
            didl.contains("id=\"d1\""),
            "starting_index=1 must land on the second entry: {didl}"
        );
        assert!(!didl.contains("id=\"d0\""), "{didl}");
        assert!(!didl.contains("id=\"f1\""), "{didl}");
    }

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
