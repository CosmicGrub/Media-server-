//! DLNA `MediaServer` support for `lumen serve` -- the `--dlna` opt-in surface: SSDP announcement
//! plus a plain, unauthenticated HTTP server answering `ContentDirectory`'s `Browse` and `Search`
//! actions and streaming the files they list.
//!
//! **`Search`, Stage 3: files only, over a bounded criteria subset.** `handle_search` walks
//! [`LibraryTree`] recursively from the named container and returns every matching *file* -- never a
//! container, since DIDL-Lite `Search` conventionally answers "find these items", and "list this
//! container's own children" (directories included) is already `Browse(DirectChildren)`'s job.
//! Recognised criteria are exactly `lumen_discovery::SearchCriteria`'s two real cases (`"*"` and
//! single-clause `dc:title contains "..."`) -- see that type's own doc, and `content_directory.rs`'s
//! module doc, for why the full UPnP search grammar is deliberately not implemented here.
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
//! -- three disjoint prefixes, so no two distinct objects can ever share an id (see [`LibraryTree`]'s
//! own doc for the bare-integer collision this replaced). `<n>` is a number handed out once per path,
//! the first time a [`LibraryTree`] build sees that path, and carried forward unchanged by every later
//! build (see [`LibraryTree::build`]); it is *not* a position in any listing.
//!
//! **The library refreshes itself; an id names the same path for the life of the process.** `run`
//! spawns its own `crate::library_watch` instance over `library_root` (the same debounced,
//! self-trigger-proof watcher the paired control channel runs for its own `library_version`, reused
//! as code and not as state -- see [`run`]'s doc). When a file is added, removed, or renamed on disk,
//! [`refresh_library`] re-walks the root, rebuilds the [`LibraryTree`] with the previous tree's ids
//! carried forward, swaps scan and tree in together, and increments `SystemUpdateID`. A refresh
//! therefore only ever *adds* ids (for paths that are new) and *retires* ids (for paths that are
//! gone, which then answer `701`/`404` honestly); it never re-points an existing id at a different
//! file. That matters beyond tidiness: the `<res>` URL in every `Browse` response is
//! `/dlna/stream/f<n>`, and a renderer re-requests that exact URL, on a fresh connection with a
//! `Range` header, every time the viewer seeks. `SystemUpdateID` moving obliges a control point to
//! re-`Browse` its cached *listings* -- nothing in `ContentDirectory:1` says a `res` URI already handed
//! to the renderer stops identifying the resource it was handed for -- so an id that shifted with a
//! refresh would splice another file's bytes into a playing stream as a perfectly valid-looking
//! `206`. Ids are still never persisted anywhere, so a `lumen serve` restart resets both the
//! numbering and `SystemUpdateID` (back to 1), which is spec-permitted -- clients cache neither
//! across a server's own `ssdp:byebye`/re-announce.

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use lumen_discovery::{
    Announcement, BrowseFlag, DeviceIdentity, DidlObject, DidlResource, ObjectClass, Responder,
    SearchCriteria, build_browse_response, build_cd_search_response, build_device_description,
    build_didl_lite, build_get_current_connection_ids_response, build_get_protocol_info_response,
    build_get_system_update_id_response, build_soap_fault, connection_manager_scpd,
    content_directory_scpd, parse_browse_request, parse_search_request,
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
/// together would reintroduce exactly the coupling this module exists to avoid. For the same reason
/// it runs its *own* `crate::library_watch` instance over the root rather than being fanned out to by
/// `remote::server`'s: the watcher module is shared code, not shared state, and each side's refresh
/// (`refresh_library` here, `rescan_library` there) replaces only its own snapshot and bumps only its
/// own counter. Two watches and two re-walks per real change is the accepted price of that.
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

    let library = RwLock::new(LibrarySnapshot::scan(&library_root, None));

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
    // once here is all the SSDP thread, the library watcher, and this function's own later use of
    // `log` need to each get their own handle to the same closure. `dyn` rather than the concrete
    // type because `library_watch::spawn` takes it as `Arc<dyn Fn>`, the same shape
    // `remote::server::run` already hands its own watcher.
    let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
    {
        let log = Arc::clone(&log);
        std::thread::spawn(move || responder.run(RENOTIFY_INTERVAL, |m| log(m)));
    }

    let listener = TcpListener::bind((bind, port))
        .map_err(|e| format!("cannot listen on {bind}:{port} for DLNA: {e}"))?;
    log(&format!("DLNA: advertising \"{friendly_name}\" at {base_url}/dlna/desc.xml"));

    let ctx = Arc::new(DlnaContext {
        library,
        system_update_id: AtomicU32::new(INITIAL_SYSTEM_UPDATE_ID),
        library_root,
        base_url,
        friendly_name,
        uuid,
    });

    // Spawned after `ctx` exists so the watcher's callback can share the exact same `Arc` every
    // connection thread below reads from -- never fatal to `run()` itself: see
    // `library_watch::spawn`'s own doc for why a watcher that cannot start only costs this session
    // its automatic refresh, not the whole DLNA listener.
    {
        let ctx = Arc::clone(&ctx);
        let root = ctx.library_root.clone();
        let on_change_log = Arc::clone(&log);
        crate::library_watch::spawn(
            &root,
            "DLNA library watcher",
            move || {
                let (file_count, system_update_id) = refresh_library(&ctx);
                on_change_log(&format!(
                    "DLNA: library changed -> rescanned, {file_count} playable files, \
                     SystemUpdateID now {system_update_id}"
                ));
            },
            Arc::clone(&log),
        );
    }

    for incoming in listener.incoming() {
        let Ok(stream) = incoming else { continue };
        let ctx = Arc::clone(&ctx);
        std::thread::spawn(move || handle_connection(stream, &ctx));
    }
    Ok(())
}

/// The `SystemUpdateID` a freshly started `lumen serve --dlna` reports before any refresh has
/// happened. `1` and not `0` because `1` is the constant every `Browse`/`Search` response carried as
/// its `UpdateID` before the counter was real -- a client that cached a listing from an older build
/// and then talks to this one sees the same starting value it always did, and only ever sees it move
/// when the library actually changed. Nothing else depends on the exact number; the spec only requires
/// that it change whenever the content does.
const INITIAL_SYSTEM_UPDATE_ID: u32 = 1;

struct DlnaContext {
    /// The library as this listener currently knows it: one `Scan` and the `LibraryTree` built from
    /// it, held *together* under one lock -- see [`LibrarySnapshot`] for why they cannot be two.
    /// `RwLock`, not `Mutex`: every request is a reader, and only a watcher-triggered
    /// [`refresh_library`] ever writes, briefly, after doing its walk outside the lock entirely. Two
    /// TVs browsing at once never wait on each other, and a refresh waits only for in-flight
    /// *response construction* -- every reader drops the guard before writing a response body to
    /// its socket (see `handle_browse`), so a slow client can never hold a refresh, and therefore
    /// every other reader queued behind that pending writer, hostage. The one exception is the
    /// short SOAP-fault paths (`701`/`708`, a few hundred fixed bytes), written with the guard still
    /// held: a write that small lands entirely in the kernel's send buffer and returns without ever
    /// waiting on the peer, so releasing first would buy nothing and cost a second code shape.
    library: RwLock<LibrarySnapshot>,
    /// UPnP `ContentDirectory:1`'s `SystemUpdateID` (a `ui4`, hence `u32`): starts at
    /// [`INITIAL_SYSTEM_UPDATE_ID`] and is incremented exactly once per completed [`refresh_library`],
    /// *inside* that function's write-lock critical section. Readers load it while holding the read
    /// lock, which is what makes the `(snapshot, UpdateID)` pair a `Browse` reports consistent: a
    /// response can never carry the new tree with the old id or vice versa, because no writer can be
    /// between the swap and the increment while any reader holds the lock. `GetSystemUpdateID` alone
    /// reads it lock-free -- it reports no content, so it has no pair to keep consistent. Wraps at
    /// `u32::MAX`, which the spec explicitly permits for a `ui4` counter.
    system_update_id: AtomicU32,
    /// The canonicalized root `run` scanned and watches -- what [`refresh_library`] re-walks, and the
    /// root `LibraryTree::build` computes every file's directory ancestry against (see `run`'s own
    /// comment on why it must be the canonical form).
    library_root: PathBuf,
    base_url: String,
    friendly_name: String,
    /// Generated once in [`run`] and carried here so every `desc.xml` response agrees with the UUID
    /// already baked into the SSDP announcements and the response's own LOCATION URL.
    uuid: String,
}

/// One consistent view of the library: a [`Scan`] and the [`LibraryTree`] built from exactly that
/// scan, replaced as a unit and never separately.
///
/// **Why one struct under one lock, rather than the `Arc<Mutex<Scan>>` plus bare `LibraryTree` this
/// replaced.** Every `"f<n>"` id is a position in `scan.playable()`'s enumeration, and every
/// `DirNode::child_files` entry and `LibraryTree::file_parent` slot is that same position, computed
/// from that same enumeration by `LibraryTree::build`. A request that read the tree from before a
/// refresh and the scan from after it would resolve `child_files[k]` against a different file list
/// than the one it was built from -- listing a file under the wrong folder, streaming the wrong file
/// for an id, or indexing past the end. Two locks taken in sequence cannot rule that out (a writer
/// can slip between them); one lock around the pair can, trivially, and costs nothing extra since a
/// refresh has to replace both anyway.
struct LibrarySnapshot {
    scan: Scan,
    tree: LibraryTree,
}

impl LibrarySnapshot {
    /// Walk `library_root` and build the matching tree, carrying every id `previous` had already
    /// assigned forward (see [`LibraryTree::build`]) -- the same two steps `run` performed inline at
    /// startup before refreshes existed, now the one definition both the startup scan (`previous` =
    /// `None`: every id is fresh) and every [`refresh_library`] share. Done entirely outside any
    /// lock when called with `None`: a walk of a large library over a network share can take
    /// seconds, and nothing a `Browse` reads is touched until the finished result is swapped in.
    /// [`refresh_library`] splits the two steps itself, for the reason its own doc gives.
    fn scan(library_root: &PathBuf, previous: Option<&LibraryTree>) -> Self {
        let scan = crate::scan::scan(
            std::slice::from_ref(library_root),
            &crate::scan::ScanOptions::default(),
        );
        let tree = LibraryTree::build(&scan, library_root, previous);
        Self { scan, tree }
    }
}

/// Re-walk `ctx.library_root`, swap the resulting [`LibrarySnapshot`] in, and bump
/// `SystemUpdateID` -- the one place either happens. Called by the watcher `run` spawns; the unit
/// tests call it directly to exercise the exact same path without waiting on real filesystem events.
/// Returns `(playable file count, new SystemUpdateID)`, the pair the watcher's log line reports.
///
/// The walk -- the only part that touches the disk, and the only part that can take seconds -- runs
/// first, with no lock held. Under the write lock happen exactly three things, together: the new
/// tree is built with the *current* tree's ids carried forward (an in-memory pass over the file
/// list, microseconds for a personal library -- and it has to read the tree being replaced, which
/// only the lock can hold still), the snapshot is swapped, and the counter is incremented. Doing
/// all three in one critical section is what guarantees both that a reader never observes the new
/// snapshot with the old id or vice versa (see `DlnaContext::system_update_id`), and that two
/// refreshes can never each carry forward from the same predecessor and hand the same fresh id to
/// two different new files.
fn refresh_library(ctx: &DlnaContext) -> (usize, u32) {
    let scan = crate::scan::scan(
        std::slice::from_ref(&ctx.library_root),
        &crate::scan::ScanOptions::default(),
    );
    let file_count = scan.playable().count();
    let mut guard = ctx.library.write().unwrap();
    let tree = LibraryTree::build(&scan, &ctx.library_root, Some(&guard.tree));
    *guard = LibrarySnapshot { scan, tree };
    let system_update_id = ctx.system_update_id.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
    drop(guard);
    (file_count, system_update_id)
}

/// One directory in the served library's own folder hierarchy -- a DLNA `StorageFolder` container.
///
/// `LibraryTree::dirs[0]` is always the root, with the UPnP-mandated fixed id `"0"`; every other
/// node's id is `"d<n>"`, where `<n>` was assigned from an incrementing counter the first time any
/// [`LibraryTree::build`] created a node for this directory's library-relative path, and is carried
/// forward by every later build (see that function's doc).
#[derive(Debug)]
struct DirNode {
    id: String,
    parent_id: String,
    /// The directory's own basename. Empty for the root -- unused, since `BrowseMetadata("0")`
    /// already hardcodes the root's title as `"lumen"` in `handle_browse`.
    name: String,
    /// Indices into the owning [`LibraryTree`]'s own `dirs`.
    child_dirs: Vec<usize>,
    /// Indices into `scan.playable()`'s own enumeration -- the same ordering `handle_browse` already
    /// built its (Stage 1, flat) file list from. The file's object id is `LibraryTree::file_ids` at
    /// that same index.
    child_files: Vec<usize>,
}

/// The served library's real folder hierarchy, built from the same [`Scan`] and library root every
/// playable file was found under -- once at startup and again on every [`refresh_library`], always
/// alongside the scan it indexes into (see [`LibrarySnapshot`]).
///
/// **Why this exists, not just a flat list**: a smart TV's Browse UI should show `Movies`, a show's
/// own folder, `Season 01`, and so on -- not one giant flat list of every file in the collection.
/// Building this per scan, rather than walking the filesystem again per request, keeps every request
/// a pure in-memory lookup.
///
/// **Fixes a real, latent id-collision bug along the way.** Stage 1 gave playable files bare-integer
/// object ids ("0", "1", ...) -- the same string space the root container's own fixed id ("0") lives
/// in. A client asking `Browse("0", BrowseMetadata)` to inspect "the object with id 0" could never
/// actually reach file index 0's metadata; the root-vs-file check in `handle_browse` ran first and
/// always won, no matter what the client actually meant. Every directory now gets a `"d<n>"` id and
/// every file an `"f<n>"` id -- three disjoint
/// prefixes (`"0"`, `"d..."`, `"f..."`), so no two distinct objects can ever share a string again.
///
/// **Ids are per path and survive a rebuild.** `<n>` comes from a counter that only ever counts up
/// (`next_dir_id`/`next_file_id`, carried from build to build), and a path that already had an id
/// in the previous tree keeps it -- see [`LibraryTree::build`] for why a position in the file list
/// would not do.
#[derive(Debug)]
struct LibraryTree {
    dirs: Vec<DirNode>,
    /// `DirNode::id` -> index into `dirs`, for every node including the root -- `O(1)` id lookups for
    /// both `Browse` flags, instead of a linear scan per request.
    by_id: HashMap<String, usize>,
    /// Each non-root directory's library-relative path -> its `"d<n>"` id: what the *next* build
    /// reads to hand the same directory the same id again. Relative, not absolute, only because that
    /// is the form `build` already computes for its own memoization; the root never changes within
    /// one `DlnaContext`, so either would do.
    dir_id_by_rel_path: HashMap<PathBuf, String>,
    /// The `"f<n>"` id of each playable file -- indexed exactly the way `scan.playable()`'s own
    /// enumeration is, the same way `file_parent` and every `DirNode::child_files` entry are.
    file_ids: Vec<String>,
    /// The inverse of `file_ids`: `"f<n>"` -> index into `scan.playable()`'s enumeration, for the
    /// `Browse(Metadata "f<n>")` and `/dlna/stream/f<n>` lookups that start from an id a client sent
    /// back. `O(1)`, like `by_id`.
    file_by_id: HashMap<String, usize>,
    /// Each playable file's absolute path -> its `"f<n>"` id: what the *next* build reads to carry
    /// that id forward. Absolute because that is exactly what `ScannedFile::path` holds, so the
    /// lookup key on the next build is the same bytes with no re-derivation to get subtly wrong.
    file_id_by_path: HashMap<PathBuf, String>,
    /// The next `"d<n>"`/`"f<n>"` number a build may hand out. Carried forward so a retired id
    /// (a removed file's) is never re-issued to a different path within one process: a renderer
    /// still holding the old URL gets an honest `404`, never another file's bytes.
    next_dir_id: u64,
    next_file_id: u64,
    /// The parent directory's own id, for each playable file -- indexed exactly the way
    /// `scan.playable()`'s own enumeration (and therefore `file_ids`) already is. Looked up by a
    /// `Browse(Metadata)` naming a bare file id directly, and by `handle_search` for every matched
    /// file (a file three levels under the searched container must report its own true immediate
    /// parent, not the container that was searched from) -- every `DirectChildren` listing already
    /// has its directory's id in hand from the [`DirNode`] being listed, so it never needs this.
    file_parent: Vec<String>,
}

impl LibraryTree {
    /// Build the hierarchy. A directory node is created only while walking an actual playable file's
    /// own ancestor chain, memoized by that directory's path relative to `library_root` so the same
    /// real directory -- reached as an ancestor of two different playable files -- is only ever
    /// created once. A directory nothing playable lives under (recursively) is therefore never
    /// visited at all, and so never gets a node -- the "empty directories are never listed" guarantee
    /// falls straight out of this construction rather than needing a second, separate check.
    ///
    /// **`previous` is what makes ids stable across a refresh.** Every directory (by library-relative
    /// path) and every file (by absolute path) that `previous` already had an id for gets that same
    /// id again; only a path `previous` never saw draws a fresh number from the carried-forward
    /// counter. `None` -- the startup build, and every test that constructs a tree from nothing --
    /// numbers everything from zero in walk order, which is exactly the numbering the pre-refresh
    /// code produced, so nothing that only ever builds once sees a difference. Ids were originally
    /// positions in `scan.playable()`'s path-sorted list, and that was a real bug once refreshes
    /// existed: dropping `Aardvark.mkv` into the library shifted every other file's number, and the
    /// `/dlna/stream/f<n>` URL a renderer was mid-playback on silently started serving a different
    /// file's bytes on its next `Range` request (see the module doc). Keying by path is the smallest
    /// fix that closes it; a content hash would survive renames too, but would cost a read of every
    /// file per refresh for a property no renderer relies on.
    fn build(scan: &Scan, library_root: &Path, previous: Option<&LibraryTree>) -> Self {
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
        let mut dir_id_by_rel_path: HashMap<PathBuf, String> = HashMap::new();
        let mut file_ids = Vec::new();
        let mut file_by_id: HashMap<String, usize> = HashMap::new();
        let mut file_id_by_path: HashMap<PathBuf, String> = HashMap::new();
        let mut next_dir_id: u64 = previous.map_or(0, |p| p.next_dir_id);
        let mut next_file_id: u64 = previous.map_or(0, |p| p.next_file_id);
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
                        let id = match previous.and_then(|p| p.dir_id_by_rel_path.get(&rel)) {
                            Some(kept) => kept.clone(),
                            None => {
                                let id = format!("d{next_dir_id}");
                                next_dir_id += 1;
                                id
                            }
                        };
                        dirs.push(DirNode {
                            id: id.clone(),
                            parent_id: dirs[parent_idx].id.clone(),
                            name: name.clone(),
                            child_dirs: Vec::new(),
                            child_files: Vec::new(),
                        });
                        let new_idx = dirs.len() - 1;
                        dirs[parent_idx].child_dirs.push(new_idx);
                        by_id.insert(id.clone(), new_idx);
                        by_rel_dir.insert(rel.clone(), new_idx);
                        dir_id_by_rel_path.insert(rel.clone(), id);
                        new_idx
                    }
                };
            }
            let position = file_parent.len();
            dirs[parent_idx].child_files.push(position);
            file_parent.push(dirs[parent_idx].id.clone());

            let id = match previous.and_then(|p| p.file_id_by_path.get(&f.path)) {
                Some(kept) => kept.clone(),
                None => {
                    let id = format!("f{next_file_id}");
                    next_file_id += 1;
                    id
                }
            };
            file_by_id.insert(id.clone(), position);
            file_id_by_path.insert(f.path.clone(), id.clone());
            file_ids.push(id);
        }

        Self {
            dirs,
            by_id,
            dir_id_by_rel_path,
            file_ids,
            file_by_id,
            file_id_by_path,
            next_dir_id,
            next_file_id,
            file_parent,
        }
    }

    fn dir_by_id(&self, id: &str) -> Option<&DirNode> {
        self.by_id.get(id).map(|&i| &self.dirs[i])
    }

    /// The `"f<n>"` id of the playable file at `position` in `scan.playable()`'s enumeration --
    /// every place that lists a file (both `Browse` flags, `Search`) goes through this so the id in a
    /// `<item id>` and the id in its `<res>` URL can never come from two different schemes.
    fn file_id(&self, position: usize) -> &str {
        &self.file_ids[position]
    }

    /// The inverse: the position in `scan.playable()`'s enumeration of the file `id` names, or `None`
    /// for an id this tree never issued -- including one a *previous* tree issued for a file that has
    /// since gone away, which is the whole point of not re-issuing numbers.
    fn file_position(&self, id: &str) -> Option<usize> {
        self.file_by_id.get(id).copied()
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

/// Dispatches `POST /dlna/cd/control` on its `SOAPACTION` header -- the same header-based routing
/// `handle_connection_manager_control` already uses for its own three actions. Exactly the three
/// actions `content_directory_scpd` declares (`Browse`, `Search`, `GetSystemUpdateID`) are answered;
/// anything else is the `401 Invalid Action` fault the SCPD's own doc promises never to need for an
/// action it declared.
fn handle_content_directory_control(
    tcp: &mut TcpStream,
    req: &HttpRequest,
    body: &[u8],
    ctx: &DlnaContext,
) {
    let soap = String::from_utf8_lossy(body);
    let action = req.header("soapaction").unwrap_or("");
    if action.contains("#Browse") {
        handle_browse(tcp, &soap, ctx);
    } else if action.contains("#Search") {
        handle_search(tcp, &soap, ctx);
    } else if action.contains("#GetSystemUpdateID") {
        // No body to parse (the action has no in-arguments) and no snapshot to read: the counter
        // alone is the whole answer, so no lock is taken -- see `DlnaContext::system_update_id`.
        let id = ctx.system_update_id.load(Ordering::Acquire);
        write_ok(
            tcp,
            "text/xml; charset=\"utf-8\"",
            build_get_system_update_id_response(id).as_bytes(),
        );
    } else {
        write_soap_fault(tcp, 401, "Invalid Action");
    }
}

fn handle_browse(tcp: &mut TcpStream, soap: &str, ctx: &DlnaContext) {
    let Some(browse) = parse_browse_request(soap) else {
        write_soap_fault(tcp, 402, "Invalid Args");
        return;
    };

    // One read guard for the whole lookup, so the tree and the file list it indexes into are the same
    // snapshot (see `LibrarySnapshot`), and the `UpdateID` loaded under it is the one that snapshot
    // was published with (see `DlnaContext::system_update_id`). Released -- explicitly, below --
    // before a single response byte is written.
    let library = ctx.library.read().unwrap();
    let update_id = ctx.system_update_id.load(Ordering::Acquire);
    let files: Vec<_> = library.scan.playable().collect();
    let tree = &library.tree;

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
                    id: tree.file_id(i).to_string(),
                    parent_id: dir.id.clone(),
                    title: f.label(),
                    class: object_class_for(f.container),
                    resource: Some(DidlResource {
                        url: format!("{}/dlna/stream/{}", ctx.base_url, tree.file_id(i)),
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
            } else if let Some(i) = tree.file_position(&browse.object_id) {
                match (files.get(i), tree.file_parent.get(i)) {
                    (Some(f), Some(parent_id)) => (
                        vec![DidlObject {
                            id: tree.file_id(i).to_string(),
                            parent_id: parent_id.clone(),
                            title: f.label(),
                            class: object_class_for(f.container),
                            resource: Some(DidlResource {
                                url: format!("{}/dlna/stream/{}", ctx.base_url, tree.file_id(i)),
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
                // Neither a known directory nor a known file -- a "d<n>"/"f<n>" this process never
                // issued, one it issued for a path that has since gone away, or a bare-numeric
                // legacy id from Stage 1's now-retired flat scheme.
                write_soap_fault(tcp, 701, "No such object");
                return;
            }
        }
    };

    let didl = build_didl_lite(&objects);
    let response = build_browse_response(&didl, objects.len() as u32, total as u32, update_id);
    // `objects` borrows nothing from the snapshot (every `DidlObject` owns its strings), so the lock
    // can go before the socket write -- a client draining this response slowly must not be able to
    // stall a refresh, and through it every other reader, behind its own socket.
    drop(library);
    write_ok(tcp, "text/xml; charset=\"utf-8\"", response.as_bytes());
}

/// `Search`: every playable *file* (never a container -- see this module's own doc) anywhere under
/// the named container, recursively, matching `criteria`. `701` if `container_id` names no real
/// directory; `708` ("Unsupported or invalid search criteria") if the criteria string parsed to
/// [`SearchCriteria::Unsupported`] rather than one of the two shapes this responder actually
/// evaluates. Pagination is `starting_index`/`requested_count` over the matched flat list, the exact
/// same skip/take-over-a-flat-sequence shape `handle_browse`'s own `DirectChildren` case already
/// uses.
fn handle_search(tcp: &mut TcpStream, soap: &str, ctx: &DlnaContext) {
    let Some(search) = parse_search_request(soap) else {
        write_soap_fault(tcp, 402, "Invalid Args");
        return;
    };
    // Same one-guard-for-the-whole-lookup shape as `handle_browse`, for the same consistency reason.
    let library = ctx.library.read().unwrap();
    let update_id = ctx.system_update_id.load(Ordering::Acquire);
    let tree = &library.tree;
    if tree.dir_by_id(&search.container_id).is_none() {
        write_soap_fault(tcp, 701, "No such object");
        return;
    }

    let files: Vec<_> = library.scan.playable().collect();

    let matched: Vec<usize> = match &search.criteria {
        SearchCriteria::MatchAll => collect_file_indices_recursive(tree, &search.container_id),
        SearchCriteria::TitleContains(text) => {
            let needle = text.to_ascii_lowercase();
            collect_file_indices_recursive(tree, &search.container_id)
                .into_iter()
                .filter(|&i| {
                    files.get(i).is_some_and(|f| f.label().to_ascii_lowercase().contains(&needle))
                })
                .collect()
        }
        SearchCriteria::Unsupported => {
            write_soap_fault(tcp, 708, "Unsupported or invalid search criteria");
            return;
        }
    };

    // Pagination over the matched flat list -- the same skip/take-over-a-single-sequence shape
    // `handle_browse`'s DirectChildren case already applies to its own combined dirs-then-files list.
    let total = matched.len();
    let start = search.starting_index as usize;
    let count = if search.requested_count == 0 { total } else { search.requested_count as usize };

    let objects: Vec<DidlObject> = matched
        .into_iter()
        .skip(start)
        .take(count)
        .filter_map(|i| {
            files.get(i).map(|f| DidlObject {
                id: tree.file_id(i).to_string(),
                // The file's own true immediate parent (from `LibraryTree::file_parent`), never
                // `search.container_id` -- a search recursing several levels down must report each
                // result's real containing directory, exactly like `Browse(Metadata)` on a file
                // already does.
                parent_id: tree.file_parent.get(i).cloned().unwrap_or_default(),
                title: f.label(),
                class: object_class_for(f.container),
                resource: Some(DidlResource {
                    url: format!("{}/dlna/stream/{}", ctx.base_url, tree.file_id(i)),
                    mime_type: content_type_for(f.container, f.extension.as_deref()).to_string(),
                    size_bytes: Some(f.size),
                }),
            })
        })
        .collect();

    let didl = build_didl_lite(&objects);
    let response = build_cd_search_response(&didl, objects.len() as u32, total as u32, update_id);
    drop(library); // Before the socket write, for the reason `handle_browse` gives.
    write_ok(tcp, "text/xml; charset=\"utf-8\"", response.as_bytes());
}

/// Every index into `scan.playable()`'s own enumeration for a file anywhere under `container_id`,
/// recursively -- the walk `handle_search` needs and `handle_browse`'s `DirectChildren` case does not
/// (that case only ever wants one directory's own direct children).
fn collect_file_indices_recursive(tree: &LibraryTree, container_id: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if let Some(dir) = tree.dir_by_id(container_id) {
        collect_dir_files_recursive(tree, dir, &mut out);
    }
    out
}

fn collect_dir_files_recursive(tree: &LibraryTree, dir: &DirNode, out: &mut Vec<usize>) {
    out.extend(dir.child_files.iter().copied());
    for &child_idx in &dir.child_dirs {
        collect_dir_files_recursive(tree, &tree.dirs[child_idx], out);
    }
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
    let library = ctx.library.read().unwrap();
    // `/dlna/stream/<id>` URLs carry the same "f<n>" ids `build_didl_lite`-driven items do (see
    // `handle_browse` and `handle_search`), so the lookup is the tree's own id -> position map, never
    // a parse of `<n>` as a list position. Resolved against whatever snapshot is current *now*, which
    // is safe precisely because a refresh carries ids forward per path (see `LibraryTree::build`): a
    // URL a renderer is mid-playback on names the same file after a refresh as before it, and names
    // nothing (`404`) only if that file really is gone.
    let Some(f) = library.tree.file_position(id).and_then(|i| library.scan.playable().nth(i))
    else {
        write_error(tcp, 404, "Not Found");
        return;
    };
    let path = f.path.clone();
    let content_type = content_type_for(f.container, f.extension.as_deref()).to_string();
    drop(library);

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
        let tree = LibraryTree::build(&scan, &root, None);
        (d, root, scan, tree)
    }

    /// A context over an already-built snapshot, exactly as `run` assembles one at startup --
    /// `system_update_id` at [`INITIAL_SYSTEM_UPDATE_ID`], no watcher (the refresh tests call
    /// `refresh_library` directly, so they never have to wait on real filesystem events or the
    /// debounce). `root` is what a refresh re-walks, so it must be the same canonical root the
    /// snapshot was scanned from.
    fn test_context(tree: LibraryTree, scan: Scan, root: PathBuf) -> DlnaContext {
        DlnaContext {
            library: RwLock::new(LibrarySnapshot { scan, tree }),
            system_update_id: AtomicU32::new(INITIAL_SYSTEM_UPDATE_ID),
            library_root: root,
            base_url: "http://127.0.0.1:7891".to_string(),
            friendly_name: "test".to_string(),
            uuid: "00000000-0000-4000-8000-000000000000".to_string(),
        }
    }

    /// The `<UpdateID>` a `Browse`/`Search` response carries, or the `<Id>` a `GetSystemUpdateID`
    /// response carries -- both are the plain decimal text of one leaf element.
    fn leaf_u32(response: &str, tag: &str) -> u32 {
        let open = format!("<{tag}>");
        let start = response.find(&open).unwrap_or_else(|| panic!("no <{tag}> in: {response}"))
            + open.len();
        let end = start + response[start..].find('<').expect("a closing tag");
        response[start..end].parse().unwrap_or_else(|_| panic!("<{tag}> is not a u32: {response}"))
    }

    /// Drives `handle_content_directory_control` with a real `#GetSystemUpdateID` SOAPACTION over a
    /// real loopback connection, the same way [`browse`]/[`search`] drive their own actions, and
    /// returns the full response text. The body is the spec's own empty-argument-list envelope.
    fn get_system_update_id(ctx: &DlnaContext) -> String {
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
            "\"urn:schemas-upnp-org:service:ContentDirectory:1#GetSystemUpdateID\"".to_string(),
        );
        let req = HttpRequest { method: "POST".into(), path: "/dlna/cd/control".into(), headers };
        let body = "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
                    <s:Body><u:GetSystemUpdateID xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\"/>\
                    </s:Body></s:Envelope>";
        handle_content_directory_control(&mut server_stream, &req, body.as_bytes(), ctx);
        drop(server_stream);

        reader.join().unwrap()
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

    fn search_soap(container_id: &str, criteria: &str, starting_index: u32, count: u32) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">\
             <s:Body><u:Search xmlns:u=\"urn:schemas-upnp-org:service:ContentDirectory:1\">\
             <ContainerID>{container_id}</ContainerID><SearchCriteria>{criteria}</SearchCriteria>\
             <Filter>*</Filter><StartingIndex>{starting_index}</StartingIndex>\
             <RequestedCount>{count}</RequestedCount><SortCriteria></SortCriteria>\
             </u:Search></s:Body></s:Envelope>"
        )
    }

    /// As [`browse`], but drives `handle_content_directory_control` with a real `#Search` SOAPACTION
    /// and a `Search` SOAP body over a real loopback TCP connection.
    fn search(
        ctx: &DlnaContext,
        container_id: &str,
        criteria: &str,
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
            "\"urn:schemas-upnp-org:service:ContentDirectory:1#Search\"".to_string(),
        );
        let req = HttpRequest { method: "POST".into(), path: "/dlna/cd/control".into(), headers };
        let body = search_soap(container_id, criteria, starting_index, count);
        handle_content_directory_control(&mut server_stream, &req, body.as_bytes(), ctx);
        drop(server_stream);

        reader.join().unwrap()
    }

    /// Drives `route` with a real `GET /dlna/stream/<id>` -- the request a renderer sends for the
    /// `<res>` URL a `Browse` handed it, including the fresh-connection `Range` re-request every real
    /// renderer issues on a seek -- over a real loopback TCP connection, returning the status code and
    /// the body bytes. Goes through `route` rather than `serve_stream` directly so the prefix-stripping
    /// the real path takes is exercised too.
    fn fetch_stream(ctx: &DlnaContext, id: &str, range: Option<&str>) -> (u16, Vec<u8>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let reader = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            let mut resp = Vec::new();
            stream.read_to_end(&mut resp).unwrap();
            resp
        });

        let (mut server_stream, _) = listener.accept().unwrap();
        let mut headers = HashMap::new();
        if let Some(range) = range {
            headers.insert("range".to_string(), range.to_string());
        }
        let req = HttpRequest { method: "GET".into(), path: format!("/dlna/stream/{id}"), headers };
        route(&mut server_stream, &req, b"", ctx);
        drop(server_stream);

        let raw = reader.join().unwrap();
        let header_end = find_header_end(&raw).expect("a complete response header block");
        let status: u16 = std::str::from_utf8(&raw[..header_end])
            .unwrap()
            .split_whitespace()
            .nth(1)
            .expect("a status code")
            .parse()
            .expect("a numeric status code");
        (status, raw[header_end..].to_vec())
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

    /// Find the `parentID` attribute of the `<item ...>` element whose `<dc:title>` contains
    /// `title_substr`, in a DIDL-Lite document -- used to assert a `Search` result names its own real
    /// containing directory, not the container the search started from.
    fn item_parent_id(didl: &str, title_substr: &str) -> String {
        let title_pos = didl.find(title_substr).unwrap_or_else(|| {
            panic!("no item titled containing {title_substr:?} found in DIDL-Lite: {didl}")
        });
        let item_start =
            didl[..title_pos].rfind("<item id=\"").expect("a preceding <item> open tag");
        let marker = "parentID=\"";
        let p = didl[item_start..].find(marker).expect("an <item> must carry parentID")
            + item_start
            + marker.len();
        let end = didl[p..].find('"').expect("a closing quote for parentID");
        didl[p..p + end].to_string()
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
        let tree = LibraryTree::build(&scan, &root, None);

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
        let (_d, root, scan, tree) = scan_tree(
            "root-children",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("Shows/Show/Season 01/S01E02.mkv", b"c"),
                ("RootFile.mkv", b"d"),
            ],
        );
        let ctx = test_context(tree, scan, root);

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
        let (_d, root, scan, tree) = scan_tree(
            "dir-children",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("RootFile.mkv", b"d"),
            ],
        );
        // "Shows" is the second directory created (Movies is created first, from the
        // alphabetically-earlier `Movies/Interstellar.mkv`), so it is "d1".
        let ctx = test_context(tree, scan, root);

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
        let (_d, root, scan, tree) =
            scan_tree("metadata", &[("Movies/Interstellar.mkv", b"a"), ("RootFile.mkv", b"d")]);
        let ctx = test_context(tree, scan, root);

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
        let (_d, root, scan, tree) = scan_tree("collision", &[("OnlyFile.mkv", b"x")]);
        let ctx = test_context(tree, scan, root);

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
    fn a_refresh_after_a_file_is_added_on_disk_makes_browse_list_it_and_advances_system_update_id()
    {
        let (d, root, scan, tree) = scan_tree("refresh-add", &[("Movies/Interstellar.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

        // Before: exactly the one file, at the starting UpdateID every response always carried.
        let before = browse(&ctx, "0", "BrowseDirectChildren", 0, 0);
        assert_eq!(leaf_u32(&before, "UpdateID"), INITIAL_SYSTEM_UPDATE_ID, "{before}");
        let movies_before = result_didl(&browse(&ctx, "d0", "BrowseDirectChildren", 0, 0));
        assert!(movies_before.contains("Interstellar"), "{movies_before}");
        assert!(!movies_before.contains("Tenet"), "{movies_before}");

        // A file lands on disk. Nothing this context serves changes until a refresh runs -- the
        // snapshot is a snapshot, not a live view of the filesystem.
        d.file("Movies/Tenet.mkv", b"b");
        let unrefreshed = result_didl(&browse(&ctx, "d0", "BrowseDirectChildren", 0, 0));
        assert!(!unrefreshed.contains("Tenet"), "no refresh has run yet: {unrefreshed}");

        // The exact function the watcher's callback calls, invoked directly.
        let (file_count, id) = refresh_library(&ctx);
        assert_eq!(file_count, 2, "the re-walk must see both files");
        assert_eq!(id, INITIAL_SYSTEM_UPDATE_ID + 1, "one completed refresh, one increment");
        assert_eq!(ctx.system_update_id.load(Ordering::Acquire), id);

        let after = browse(&ctx, "d0", "BrowseDirectChildren", 0, 0);
        assert_eq!(leaf_u32(&after, "UpdateID"), id, "{after}");
        let movies_after = result_didl(&after);
        assert!(movies_after.contains("Interstellar"), "{movies_after}");
        assert!(movies_after.contains("Tenet"), "the new file must now be listed: {movies_after}");
        assert_eq!(movies_after.matches("<item").count(), 2, "{movies_after}");
    }

    #[test]
    fn a_refresh_keeps_every_surviving_objects_id_and_only_a_new_path_draws_a_fresh_one() {
        // `scan::scan` sorts by path, so a file that sorts *before* every existing one would have
        // shifted every id when `"f<n>"` was a list position. Now it must not: Yak stays "f0" (and
        // Shows stays "d0") through a refresh that puts new entries ahead of both in walk order, the
        // newcomers get numbers the previous tree never issued, and the rebuilt tree's
        // `child_files` still index the rebuilt scan with no off-by-one.
        let (d, root, scan, tree) =
            scan_tree("refresh-stable-ids", &[("Shows/Zebra.mkv", b"z"), ("Shows/Yak.mkv", b"y")]);
        let ctx = test_context(tree, scan, root);
        let f0_before = result_didl(&browse(&ctx, "f0", "BrowseMetadata", 0, 0));
        assert!(f0_before.contains("<item id=\"f0\" parentID=\"d0\""), "{f0_before}");
        assert!(f0_before.contains("<dc:title>Yak</dc:title>"), "{f0_before}");

        // Both sort first: a file directly under the root, and a whole new directory, ahead of `Shows/`.
        d.file("Aardvark.mkv", b"a");
        d.file("Anthology/Ant.mkv", b"ant");
        refresh_library(&ctx);

        let f0_after = result_didl(&browse(&ctx, "f0", "BrowseMetadata", 0, 0));
        assert!(f0_after.contains("<dc:title>Yak</dc:title>"), "f0 is still Yak: {f0_after}");
        assert!(
            f0_after.contains("<item id=\"f0\" parentID=\"d0\""),
            "and its parent is still the Shows node, still d0: {f0_after}"
        );
        let f1_after = result_didl(&browse(&ctx, "f1", "BrowseMetadata", 0, 0));
        assert!(f1_after.contains("<dc:title>Zebra</dc:title>"), "{f1_after}");

        let shows = result_didl(&browse(&ctx, "d0", "BrowseDirectChildren", 0, 0));
        assert!(shows.contains("Yak") && shows.contains("Zebra"), "{shows}");
        assert!(!shows.contains("Aardvark") && !shows.contains("Ant"), "{shows}");

        // The two newcomers drew the next unused numbers in walk order -- `scan` sorts by full path,
        // and "Aardvark.mkv" < "Anthology/Ant.mkv" < "Shows/...", so Aardvark is f2, Ant is f3, and
        // Anthology is the first directory created after Shows: d1.
        let root_didl = result_didl(&browse(&ctx, "0", "BrowseDirectChildren", 0, 0));
        assert!(root_didl.contains("<container id=\"d1\" parentID=\"0\""), "{root_didl}");
        assert!(root_didl.contains("<dc:title>Anthology</dc:title>"), "{root_didl}");
        assert!(root_didl.contains("<item id=\"f2\" parentID=\"0\""), "{root_didl}");
        assert!(root_didl.contains("<dc:title>Aardvark</dc:title>"), "{root_didl}");
        let anthology = result_didl(&browse(&ctx, "d1", "BrowseDirectChildren", 0, 0));
        assert!(anthology.contains("<item id=\"f3\" parentID=\"d1\""), "{anthology}");
        assert!(anthology.contains("<dc:title>Ant</dc:title>"), "{anthology}");
    }

    #[test]
    fn a_retired_id_is_never_reissued_and_a_path_that_comes_back_is_a_new_object() {
        // Remove Yak, refresh, then put a file back at the very same path and refresh again. The
        // counter only counts up, so the returned file is a brand-new object with a fresh id and the
        // old "f0" stays dead -- a renderer that cached the old URL cannot be handed bytes it did not
        // ask for just because a path was reused, and a control point that saw SystemUpdateID move
        // sees a genuinely new item, which is what actually happened on disk.
        let (d, root, scan, tree) =
            scan_tree("retired-ids", &[("Yak.mkv", b"y"), ("Zebra.mkv", b"z")]);
        let ctx = test_context(tree, scan, root);
        assert_eq!(fetch_stream(&ctx, "f0", None), (200, b"y".to_vec()));

        std::fs::remove_file(d.0.join("Yak.mkv")).unwrap();
        refresh_library(&ctx);
        assert_eq!(fetch_stream(&ctx, "f0", None).0, 404);
        let gone = browse(&ctx, "f0", "BrowseMetadata", 0, 0);
        assert!(gone.contains("<errorCode>701</errorCode>"), "{gone}");
        assert_eq!(fetch_stream(&ctx, "f1", None), (200, b"z".to_vec()), "Zebra is untouched");

        d.file("Yak.mkv", b"y2");
        refresh_library(&ctx);
        assert_eq!(fetch_stream(&ctx, "f0", None).0, 404, "the retired id stays retired");
        let root_didl = result_didl(&browse(&ctx, "0", "BrowseDirectChildren", 0, 0));
        assert!(root_didl.contains("<item id=\"f2\" parentID=\"0\""), "{root_didl}");
        assert!(root_didl.contains("<item id=\"f1\" parentID=\"0\""), "{root_didl}");
        assert!(!root_didl.contains("id=\"f0\""), "{root_didl}");
        assert_eq!(fetch_stream(&ctx, "f2", None), (200, b"y2".to_vec()));
    }

    #[test]
    fn a_stream_url_handed_out_before_a_refresh_still_serves_the_same_file_after_it() {
        // The scenario a real renderer produces: it is playing `Yak.mkv` via the `<res>` URL a
        // `Browse` gave it, a file that sorts *before* Yak lands on disk, the watcher refreshes, and
        // the renderer's next seek is a fresh `GET` of that same URL with a `Range` header. It must
        // get Yak's bytes -- never a `206` full of some other file's, and never a `404` for a file
        // that is still right there on disk.
        let (d, root, scan, tree) =
            scan_tree("stable-stream", &[("Shows/Zebra.mkv", b"zebra"), ("Shows/Yak.mkv", b"yak")]);
        let ctx = test_context(tree, scan, root);
        let shows = result_didl(&browse(&ctx, "d0", "BrowseDirectChildren", 0, 0));
        let yak_id = {
            let pos = shows.find("<dc:title>Yak</dc:title>").unwrap();
            let open = shows[..pos].rfind("<item id=\"").unwrap() + "<item id=\"".len();
            shows[open..open + shows[open..].find('"').unwrap()].to_string()
        };
        let (status, body) = fetch_stream(&ctx, &yak_id, None);
        assert_eq!((status, body.as_slice()), (200, &b"yak"[..]));

        d.file("Aardvark.mkv", b"aardvark"); // Sorts first: before `Shows/` and everything in it.
        refresh_library(&ctx);

        let (status, body) = fetch_stream(&ctx, &yak_id, Some("bytes=1-"));
        assert_eq!(status, 206, "a Range re-request of a still-present file is a real 206");
        assert_eq!(
            body, b"ak",
            "the bytes after a refresh must come from the same file the URL was handed out for"
        );

        // And a file that really did go away is an honest 404 on its old URL -- while the survivors'
        // URLs keep working, untouched by the removal.
        std::fs::remove_file(d.0.join("Shows/Zebra.mkv")).unwrap();
        let zebra_id = {
            let pos = shows.find("<dc:title>Zebra</dc:title>").unwrap();
            let open = shows[..pos].rfind("<item id=\"").unwrap() + "<item id=\"".len();
            shows[open..open + shows[open..].find('"').unwrap()].to_string()
        };
        refresh_library(&ctx);
        assert_eq!(fetch_stream(&ctx, &zebra_id, None).0, 404, "Zebra is gone: {zebra_id}");
        assert_eq!(fetch_stream(&ctx, &yak_id, None), (200, b"yak".to_vec()));
    }

    #[test]
    fn every_browse_and_search_response_reports_the_contexts_current_system_update_id() {
        let (_d, root, scan, tree) =
            scan_tree("update-id-echo", &[("Movies/Interstellar.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

        // Advance the counter past its starting value by real refreshes, not by poking the atomic --
        // the claim is that responses echo whatever the *refresh path* left there.
        refresh_library(&ctx);
        refresh_library(&ctx);
        refresh_library(&ctx);
        let current = ctx.system_update_id.load(Ordering::Acquire);
        assert_eq!(current, INITIAL_SYSTEM_UPDATE_ID + 3);

        for flag in ["BrowseDirectChildren", "BrowseMetadata"] {
            let response = browse(&ctx, "0", flag, 0, 0);
            assert_eq!(leaf_u32(&response, "UpdateID"), current, "{flag}: {response}");
        }
        let file_meta = browse(&ctx, "f0", "BrowseMetadata", 0, 0);
        assert_eq!(leaf_u32(&file_meta, "UpdateID"), current, "{file_meta}");
        let search_response = search(&ctx, "0", "*", 0, 0);
        assert_eq!(leaf_u32(&search_response, "UpdateID"), current, "{search_response}");
        let title_search = search(&ctx, "0", "dc:title contains \"inter\"", 0, 0);
        assert_eq!(leaf_u32(&title_search, "UpdateID"), current, "{title_search}");
    }

    #[test]
    fn get_system_update_id_answers_over_the_control_endpoint_and_tracks_every_refresh() {
        let (_d, root, scan, tree) = scan_tree("get-system-update-id", &[("A.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

        let response = get_system_update_id(&ctx);
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("<u:GetSystemUpdateIDResponse"), "{response}");
        assert_eq!(leaf_u32(&response, "Id"), INITIAL_SYSTEM_UPDATE_ID, "{response}");

        let (_, after_first) = refresh_library(&ctx);
        assert_eq!(leaf_u32(&get_system_update_id(&ctx), "Id"), after_first);
        let (_, after_second) = refresh_library(&ctx);
        assert_eq!(leaf_u32(&get_system_update_id(&ctx), "Id"), after_second);
        assert_eq!(after_second, after_first + 1);

        // And it is the same number a Browse issued right now would report -- one counter, not two.
        let browse_now = browse(&ctx, "0", "BrowseDirectChildren", 0, 0);
        assert_eq!(leaf_u32(&browse_now, "UpdateID"), after_second, "{browse_now}");
    }

    #[test]
    fn an_undeclared_content_directory_action_is_still_a_401_fault() {
        // The SCPD now declares three actions; anything else must remain the honest `Invalid Action`
        // fault, not fall through into one of the real handlers by accident of substring matching.
        let (_d, root, scan, tree) = scan_tree("invalid-action", &[("A.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

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
            "\"urn:schemas-upnp-org:service:ContentDirectory:1#GetSearchCapabilities\"".to_string(),
        );
        let req = HttpRequest { method: "POST".into(), path: "/dlna/cd/control".into(), headers };
        handle_content_directory_control(&mut server_stream, &req, b"", &ctx);
        drop(server_stream);
        let response = reader.join().unwrap();
        assert!(response.contains("<errorCode>401</errorCode>"), "{response}");
    }

    #[test]
    fn pagination_applies_over_the_combined_directories_then_files_sequence() {
        let (_d, root, scan, tree) = scan_tree(
            "paginate",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("RootFile.mkv", b"d"),
            ],
        );
        // Root's combined DirectChildren sequence is [Movies(d0), Shows(d1), RootFile(f1)] -- 3 total.
        let ctx = test_context(tree, scan, root);

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
    fn search_match_all_finds_every_file_recursively_not_just_direct_children() {
        let (_d, root, scan, tree) = scan_tree(
            "search-matchall",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("Shows/Show/Season 01/S01E02.mkv", b"c"),
                ("RootFile.mkv", b"d"),
            ],
        );
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "0", "*", 0, 0);
        assert!(response.contains("<NumberReturned>4</NumberReturned>"), "{response}");
        assert!(response.contains("<TotalMatches>4</TotalMatches>"), "{response}");
        let didl = result_didl(&response);
        for expected in ["Interstellar", "S01E01", "S01E02"] {
            assert!(didl.contains(expected), "{expected} missing: {didl}");
        }
        assert!(
            !didl.contains("<container"),
            "Search results are items only, never containers: {didl}"
        );

        // A file two directories below the searched container must report its own real immediate
        // parent (the Season 01 directory), never the root container that was searched from.
        assert_ne!(
            item_parent_id(&didl, "S01E01"),
            "0",
            "a deeply nested file's parentID must be its real directory, not the root it was \
             searched from: {didl}"
        );
    }

    #[test]
    fn search_title_contains_matches_case_insensitively_and_only_the_right_files() {
        let (_d, root, scan, tree) = scan_tree(
            "search-title",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Chernobyl/Season 01/Chernobyl.S01E01.mkv", b"b"),
                ("Shows/Chernobyl/Season 01/Chernobyl.S01E02.mkv", b"c"),
            ],
        );
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "0", "dc:title contains \"CHERNOBYL\"", 0, 0);
        assert!(response.contains("<NumberReturned>2</NumberReturned>"), "{response}");
        let didl = result_didl(&response);
        assert!(didl.contains("S01E01"), "{didl}");
        assert!(didl.contains("S01E02"), "{didl}");
        assert!(!didl.contains("Interstellar"), "the unrelated title must not match: {didl}");
    }

    #[test]
    fn search_from_a_non_root_container_only_returns_files_under_that_subtree() {
        let (_d, root, scan, tree) = scan_tree(
            "search-subtree",
            &[
                ("Movies/Interstellar.mkv", b"a"),
                ("Shows/Show/Season 01/S01E01.mkv", b"b"),
                ("RootFile.mkv", b"d"),
            ],
        );
        // "Shows" is the second directory created (Movies is created first, from the
        // alphabetically-earlier `Movies/Interstellar.mkv`), so it is "d1" -- matching
        // `a_directorys_direct_children_returns_only_its_own_children`'s own fixture and reasoning.
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "d1", "*", 0, 0);
        assert!(response.contains("<NumberReturned>1</NumberReturned>"), "{response}");
        let didl = result_didl(&response);
        assert!(didl.contains("S01E01"), "{didl}");
        assert!(!didl.contains("Interstellar"), "{didl}");
        assert!(!didl.contains("RootFile"), "{didl}");
    }

    #[test]
    fn search_pagination_works_like_browses_over_the_matched_flat_list() {
        let (_d, root, scan, tree) =
            scan_tree("search-paginate", &[("A.mkv", b"a"), ("B.mkv", b"b"), ("C.mkv", b"c")]);
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "0", "*", 1, 1);
        assert!(response.contains("<NumberReturned>1</NumberReturned>"), "{response}");
        assert!(
            response.contains("<TotalMatches>3</TotalMatches>"),
            "TotalMatches must reflect every match, not just the returned page: {response}"
        );
    }

    #[test]
    fn search_with_an_unknown_container_id_is_a_701_fault() {
        let (_d, root, scan, tree) = scan_tree("search-unknown", &[("A.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "d999", "*", 0, 0);
        assert!(response.contains("<errorCode>701</errorCode>"), "{response}");
    }

    #[test]
    fn search_with_unsupported_criteria_is_a_708_fault() {
        let (_d, root, scan, tree) = scan_tree("search-unsupported", &[("A.mkv", b"a")]);
        let ctx = test_context(tree, scan, root);

        let response = search(&ctx, "0", "dc:creator contains \"X\"", 0, 0);
        assert!(response.contains("<errorCode>708</errorCode>"), "{response}");
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
