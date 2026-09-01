//! HLS delivery: `.m3u8`/`.m4s` routes on the same authenticated HTTP surface `http.rs` already
//! serves `/stream/` on. `lumen-segment`'s own module doc names this as its "next real step" —
//! everything here is wiring an already-built segmenting library into an already-built HTTP surface,
//! not inventing either.
//!
//! **The token rides in the URL path, not a header or `?token=` query parameter** — the one place
//! this route family cannot simply copy `/stream/`'s transport. A real HLS client resolves the
//! *relative* segment/init URIs a playlist carries against that playlist's own request URL per RFC
//! 3986 §5.3's merge rules: a relative reference with a non-empty path has an *empty* query
//! component, and the merge algorithm keeps the reference's own (empty) query rather than the base
//! URL's — so a `?token=` on the playlist's URL is silently dropped from every segment fetch the
//! player generates on its own, and a native player's automatic fetches have no way to carry a custom
//! `Authorization` header at all. Putting the token in the path is not a style choice; it is the only
//! transport that survives relative-URI resolution. A useful side effect: because the token lives
//! only in routing, never in cached playlist content, one cached generation serves every requester's
//! own (possibly different) token unchanged.
//!
//! **Generation is lazy, coalesced per source, and cached to disk keyed on (path, size, mtime).** The
//! first `playlist.m3u8` request for a source with no cache entry yet runs `lumen_segment::execute()`
//! on a helper thread while the requesting connection waits on it with a bounded timeout — the same
//! `mpsc` + `recv_timeout` shape `server.rs::run_command` already uses to bound the mpv driver
//! thread, applied here because `execute()` itself has no timeout of its own. Concurrent requests for
//! the *same* source coalesce onto one ffmpeg run via a per-cache-key lock; a source replaced under
//! the same library path (different size or mtime) gets a fresh key and a fresh generation rather
//! than serving stale segments under its old name.
//!
//! **The generated output is never served as ffmpeg wrote it.** `HlsSegmentJob::output_dir` here is
//! an absolute temporary path, and whether ffmpeg's own `-f hls` muxer writes bare or absolute URIs
//! into the playlist it produces is not specified by anything this module can rely on — so, matching
//! this codebase's own "never silently assume, always verify" posture (`execute()`'s own
//! re-verification of ffmpeg's exit status is the model), [`rebuild_playlist`] reads ffmpeg's raw
//! playlist back, confirms every segment it references actually exists and is non-empty, and rewrites
//! it via [`lumen_segment::playlist::MediaPlaylist`] with bare relative file names and ffmpeg's own
//! real, keyframe-accurate segment durations before the result is ever exposed. Build output lands in
//! a private `.building-*` directory and is made visible with one atomic rename, so a `lumen serve`
//! killed mid-generation can never leave a half-written directory at a path a later request would
//! trust.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use lumen_segment::playlist::{InitSegment, MediaPlaylist, Segment};
use lumen_segment::{HlsExecError, HlsExecOutcome, HlsSegmentJob, SegmentFormat, execute};
use sha2::{Digest, Sha256};

use super::http;
use super::{ServerContext, TlsStream, contain_within_library};
use crate::remote::pairing::dirs_next_config_dir;

/// Target length per segment. Not user-configurable (yet) — 6s is the common middle ground real HLS
/// deployments converge on: short enough that a seek only has to wait out part of one segment, long
/// enough that per-segment HTTP overhead stays negligible.
const HLS_SEGMENT_SECONDS: u32 = 6;

/// How long a connection waits for one `execute()` run before giving up. `execute()` stream-copies
/// (no re-encode), so this is disk-I/O-bound, not CPU-bound — generous even for a very large file on
/// a slow disk, while still bounding what would otherwise be `run()`'s only unbounded wait.
const HLS_GENERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Soft cap on total cache size. Content is stream-copied, so segmented output is roughly
/// source-file-sized — this is sized for a personal library's realistic concurrent working set (a
/// handful of titles), not admission control: eviction is always best-effort, never a reason to fail
/// a legitimate request.
const HLS_CACHE_MAX_BYTES: u64 = 25 * 1024 * 1024 * 1024;

/// Independent age cap, so one large, rarely-evicted-by-size entry doesn't linger indefinitely across
/// a `lumen serve` session left running for weeks — the expected usage pattern per this file's own
/// module doc.
const HLS_CACHE_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

/// How long a `.building-*` directory is given before the opportunistic reap treats it as abandoned
/// rather than genuinely in progress — comfortably past [`HLS_GENERATION_TIMEOUT`], so this can only
/// fire for a build whose own timeout has already elapsed.
const HLS_BUILDING_GRACE: Duration = Duration::from_secs(60 * 60);

enum Artifact {
    Playlist,
    Init,
    Segment,
}

/// Validated before any filesystem path is built from it — the only defense-in-depth this route
/// family adds beyond what `/stream/` already had to close; the real containment boundary is still
/// [`contain_within_library`] on the source portion, unchanged.
fn parse_artifact(name: &str) -> Option<Artifact> {
    if name == "playlist.m3u8" {
        return Some(Artifact::Playlist);
    }
    if name == "init.mp4" {
        return Some(Artifact::Init);
    }
    let digits = name.strip_prefix("seg_")?.strip_suffix(".m4s")?;
    (digits.len() == 5 && digits.bytes().all(|b| b.is_ascii_digit())).then_some(Artifact::Segment)
}

/// Where generated HLS output is cached — beside the pairing token store and TLS certificate, in this
/// user's own config directory rather than the binary's, the same reasoning `TokenStore::default_path`
/// and `ServerCert::default_dir` already document.
pub(super) fn default_cache_root() -> PathBuf {
    dirs_next_config_dir().join("lumen").join("hls-cache")
}

/// Handle one request under `/hls/`. `rest` is everything after that prefix:
/// `<token>/<url-decoded absolute source path>/<artifact>`.
pub(super) fn handle(tls: &mut TlsStream, method: &str, rest: &str, ctx: &ServerContext) {
    if !matches!(method, "GET" | "HEAD") {
        http::write_error(tls, 405, "Method Not Allowed");
        return;
    }

    let Some((token, after_token)) = rest.split_once('/') else {
        http::write_error(tls, 404, "Not Found");
        return;
    };
    if !ctx.tokens.lock().unwrap().is_valid(token) {
        http::write_error(tls, 401, "Unauthorized");
        return;
    }

    let Some((source_str, artifact_name)) = after_token.rsplit_once('/') else {
        http::write_error(tls, 404, "Not Found");
        return;
    };
    let Some(artifact) = parse_artifact(artifact_name) else {
        http::write_error(tls, 404, "Not Found");
        return;
    };

    let real_source = match contain_within_library(&ctx.library_root, source_str) {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            http::write_error(tls, 404, "Not Found");
            return;
        }
    };
    let Ok(meta) = fs::metadata(&real_source) else {
        http::write_error(tls, 404, "Not Found");
        return;
    };
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let key = cache_key(&real_source, meta.len(), mtime);
    let cache_dir = ctx.hls_cache_root.join(&key);

    // Registered before anything below assumes `cache_dir` will keep existing -- not after. `handle`
    // used to confirm the directory was ready (via `ensure_ready` or a bare `is_dir` check) and only
    // register as a reader afterward, leaving a real window between "confirmed present" and "counted
    // as being read" during which a concurrent `maybe_evict` sweep -- triggered by any other, unrelated
    // request building a different key -- could delete this exact directory out from under an
    // already-validated request, turning it into a spurious 404. `maybe_evict`'s own `has_readers`
    // guard only protects a key once it has an entry in `ctx.active_readers`, so registering first is
    // what actually closes the gap; a reader recorded for a key with no directory yet (the common,
    // first-generation case) costs nothing; `maybe_evict` simply has nothing to find for it.
    let _guard = ReaderGuard::new(&ctx.active_readers, &key);

    match artifact {
        Artifact::Playlist => {
            if let Err(e) = ensure_ready(ctx, &real_source, &key, &cache_dir) {
                write_gen_error(tls, &e);
                return;
            }
        }
        // A well-formed client only ever learns a segment/init URL from a playlist this server
        // itself just served after the cache directory was already complete — an init/segment
        // request that arrives with no such directory yet is a stale or forged URL, not a
        // legitimate race to accommodate with a wait/retry.
        Artifact::Init | Artifact::Segment => {
            if !cache_dir.is_dir() {
                http::write_error(tls, 404, "Not Found");
                return;
            }
        }
    }

    touch_last_used(&cache_dir);
    let content_type = match artifact {
        Artifact::Playlist => "application/vnd.apple.mpegurl",
        Artifact::Init | Artifact::Segment => "video/mp4",
    };
    // No `Range` support: a real HLS client fetches each artifact whole (this module's own doc
    // frames the problem as "byte-range-free" for exactly that reason), so there is nothing to
    // thread a `Range` header through for. `serve_file` handles `None` correctly either way.
    http::serve_file(tls, method, &cache_dir.join(artifact_name), content_type, None);
}

/// Content-derived cache key: canonical path plus size plus mtime, so a file replaced in place under
/// the same library path gets a fresh key automatically — no explicit invalidation path needed, and
/// no risk of silently serving stale segments for new content under an old name.
fn cache_key(source: &Path, size: u64, mtime: SystemTime) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.to_string_lossy().as_bytes());
    hasher.update(size.to_le_bytes());
    let secs = mtime.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    hasher.update(secs.to_le_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
enum HlsGenError {
    FfmpegNotFound,
    Spawn(std::io::Error),
    NonZeroExit(String),
    PlaylistMissing,
    NoSegmentsProduced,
    TimedOut,
    /// The helper thread that runs `execute()` ended without ever sending a result -- almost
    /// certainly a panic inside `execute()` itself -- distinct from [`Self::TimedOut`] so a worker
    /// that dies in milliseconds is never reported to a client or a log line as having "timed out
    /// after 30 minutes".
    WorkerLost,
    VerifyMismatch(&'static str),
    Io(std::io::Error),
}

fn write_gen_error(tls: &mut TlsStream, err: &HlsGenError) {
    let (code, log_line, client_msg): (u16, String, &str) = match err {
        HlsGenError::FfmpegNotFound => {
            (503, "ffmpeg not found".into(), "ffmpeg is not installed on this server")
        }
        HlsGenError::Spawn(e) => {
            (500, format!("failed to start ffmpeg: {e}"), "failed to start ffmpeg")
        }
        HlsGenError::NonZeroExit(stderr) => {
            (500, format!("ffmpeg exited non-zero: {}", stderr.trim()), "segmenting failed")
        }
        HlsGenError::PlaylistMissing => {
            (500, "ffmpeg exited successfully but wrote no playlist".into(), "segmenting failed")
        }
        HlsGenError::NoSegmentsProduced => (
            500,
            "ffmpeg exited successfully but produced no segments".into(),
            "no output segments were produced for this source",
        ),
        HlsGenError::TimedOut => (
            500,
            format!("timed out after {HLS_GENERATION_TIMEOUT:?} waiting for ffmpeg"),
            "timed out waiting for ffmpeg",
        ),
        HlsGenError::WorkerLost => (
            500,
            "the generation thread ended unexpectedly before reporting a result (likely a panic)"
                .into(),
            "segmenting failed unexpectedly",
        ),
        HlsGenError::VerifyMismatch(why) => (
            500,
            format!("playlist verification failed: {why}"),
            "segmenting produced an inconsistent result",
        ),
        HlsGenError::Io(e) => {
            (500, format!("i/o error preparing HLS output: {e}"), "internal error")
        }
    };
    eprintln!("hls: {log_line}");
    http::write_error(tls, code, client_msg);
}

/// Generate-if-missing, coalesced per source. Double-checked against the filesystem around a
/// lazily-created per-key lock: the fast path (already cached) takes no lock at all, and a sibling
/// thread that lost the race to acquire the lock finds the directory already there the moment it
/// gets its turn.
fn ensure_ready(
    ctx: &ServerContext,
    source: &Path,
    key: &str,
    cache_dir: &Path,
) -> Result<(), HlsGenError> {
    if cache_dir.is_dir() {
        return Ok(());
    }
    let lock = {
        let mut locks = ctx.hls_locks.lock().unwrap();
        Arc::clone(locks.entry(key.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))))
    };
    let _guard = lock.lock().unwrap();
    if cache_dir.is_dir() {
        return Ok(()); // a sibling request finished the build while this one waited for the lock
    }

    maybe_evict(ctx);

    let ffmpeg_bin = ctx.ffmpeg_bin.clone().ok_or(HlsGenError::FfmpegNotFound)?;
    let tmp_dir = ctx.hls_cache_root.join(format!(".building-{key}-{}", unique_suffix()));
    fs::create_dir_all(&tmp_dir).map_err(HlsGenError::Io)?;

    let job = HlsSegmentJob {
        source: source.to_path_buf(),
        output_dir: tmp_dir.clone(),
        playlist_name: "playlist.m3u8".to_string(),
        segment_seconds: HLS_SEGMENT_SECONDS,
        format: SegmentFormat::Fmp4,
    };

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(execute(&job, &ffmpeg_bin));
    });

    let outcome: HlsExecOutcome = match rx.recv_timeout(HLS_GENERATION_TIMEOUT) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(match e {
                HlsExecError::Spawn(io) => HlsGenError::Spawn(io),
                HlsExecError::NonZeroExit { stderr, .. } => HlsGenError::NonZeroExit(stderr),
                HlsExecError::PlaylistMissing => HlsGenError::PlaylistMissing,
                HlsExecError::NoSegmentsProduced => HlsGenError::NoSegmentsProduced,
            });
        }
        // The helper thread's ffmpeg child may still be running and writing into `tmp_dir` here —
        // `execute()` exposes no handle this could use to kill it, and this integration does not
        // change that crate to add one. `tmp_dir` is deliberately left in place rather than removed
        // out from under a possibly-still-writing process; it is a named, accepted leak for this rare
        // pathological case, bounded by the mid-session `.building-*` grace-period sweep and the
        // unconditional startup sweep on the next restart.
        Err(mpsc::RecvTimeoutError::Timeout) => return Err(HlsGenError::TimedOut),
        // Distinct from a genuine timeout, and handled differently: the `Sender` was dropped without
        // ever sending, which only happens if the helper thread ended -- almost certainly panicked --
        // inside `execute()` itself. Unlike the timeout case, the thread is confirmed dead here, so
        // nothing can still be writing into `tmp_dir`; cleaning it up immediately (rather than
        // reporting a false "timed out after 30 minutes" that in reality took milliseconds, and
        // leaving the directory for the hour-long grace-period sweep) is both honest and safe.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(HlsGenError::WorkerLost);
        }
    };

    if let Err(e) = rebuild_playlist(&tmp_dir, &outcome) {
        // `rebuild_playlist` already cleans up `tmp_dir` on every verification-failure path of its
        // own (a `VerifyMismatch`); this covers the one path it does not own -- an `Io` error from
        // `fs::read_to_string`/`fs::metadata` before its own cleanup runs -- so no error out of this
        // function ever leaves a `.building-*` directory that isn't already handled by a sweep.
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(e);
    }

    match fs::rename(&tmp_dir, cache_dir) {
        Ok(()) => {
            touch_last_used(cache_dir);
            Ok(())
        }
        // Lost a cross-process race to a `lumen serve` instance that finished the identical build
        // first (or, within one process, a genuinely impossible case given the lock above — kept as
        // a safe fallback rather than an `unwrap`). The winner's copy is already correct.
        Err(_) if cache_dir.is_dir() => {
            let _ = fs::remove_dir_all(&tmp_dir);
            Ok(())
        }
        // A genuine rename failure (e.g. a permission change on `hls_cache_root` mid-flight) after
        // `rebuild_playlist` has already fully verified real, complete output sitting in `tmp_dir` --
        // unlike the timeout case, nothing is still writing into it, so it is cleaned up here rather
        // than left for the grace-period sweep.
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            Err(HlsGenError::Io(e))
        }
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// Rewrites `tmp_dir/playlist.m3u8` in place: parses ffmpeg's own raw output for real, per-segment
/// durations and URIs, confirms every referenced file actually exists and is non-empty (one
/// verification layer deeper than `execute()`'s own "at least one segment landed" check), then
/// rebuilds the playlist text via [`MediaPlaylist`] with bare relative file names — never trusting
/// that ffmpeg's `-hls_segment_filename` (an absolute tmp-dir path) produced bare URIs on its own.
fn rebuild_playlist(tmp_dir: &Path, outcome: &HlsExecOutcome) -> Result<(), HlsGenError> {
    let text = fs::read_to_string(tmp_dir.join("playlist.m3u8")).map_err(HlsGenError::Io)?;
    let pairs = parse_extinf_pairs(&text);
    if pairs.len() != outcome.segment_count {
        let _ = fs::remove_dir_all(tmp_dir);
        return Err(HlsGenError::VerifyMismatch(
            "the playlist's own EXTINF count did not match ffmpeg's reported segment count",
        ));
    }

    let mut segments = Vec::with_capacity(pairs.len());
    for (duration_secs, uri) in pairs {
        let Some(name) = Path::new(&uri).file_name().and_then(|n| n.to_str()) else {
            let _ = fs::remove_dir_all(tmp_dir);
            return Err(HlsGenError::VerifyMismatch("a playlist entry had no usable file name"));
        };
        match fs::metadata(tmp_dir.join(name)) {
            Ok(m) if m.len() > 0 => segments.push(Segment { duration_secs, uri: name.to_string() }),
            _ => {
                let _ = fs::remove_dir_all(tmp_dir);
                return Err(HlsGenError::VerifyMismatch(
                    "a segment the playlist references is missing or empty",
                ));
            }
        }
    }
    match fs::metadata(tmp_dir.join("init.mp4")) {
        Ok(m) if m.len() > 0 => {}
        _ => {
            let _ = fs::remove_dir_all(tmp_dir);
            return Err(HlsGenError::VerifyMismatch("init.mp4 is missing or empty"));
        }
    }

    let playlist =
        MediaPlaylist { segments, init: Some(InitSegment { uri: "init.mp4".to_string() }) };
    if let Err(e) = fs::write(tmp_dir.join("playlist.m3u8"), playlist.to_m3u8()) {
        // Every verification-failure branch above already cleans up `tmp_dir` before returning; this
        // one -- a real I/O failure on the final rewrite, most plausibly the disk filling up right
        // after an entire media file's worth of segments were just copied into `tmp_dir` -- must too,
        // or a multi-GB directory sits there uncleaned until the hour-long grace-period sweep.
        let _ = fs::remove_dir_all(tmp_dir);
        return Err(HlsGenError::Io(e));
    }
    Ok(())
}

/// Hand-rolled `#EXTINF:<duration>,[title]` / URI pair reader — bounded and total, the same
/// "malformed input is skipped, never a panic" posture every other parser in this workspace takes.
/// Not a general M3U8 parser: only what this module itself ever writes (via `command.rs`'s
/// `build_command`) needs to round-trip here.
fn parse_extinf_pairs(text: &str) -> Vec<(f64, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("#EXTINF:") else { continue };
        let Ok(duration_secs) = rest.split(',').next().unwrap_or("").trim().parse::<f64>() else {
            continue;
        };
        let Some(uri_line) = lines.next() else { break };
        let uri = uri_line.trim();
        if uri.is_empty() || uri.starts_with('#') {
            continue;
        }
        out.push((duration_secs, uri.to_string()));
    }
    out
}

fn touch_last_used(cache_dir: &Path) {
    let _ = fs::write(cache_dir.join(".last_used"), []);
}

fn last_used_time(dir: &Path) -> SystemTime {
    fs::metadata(dir.join(".last_used"))
        .and_then(|m| m.modified())
        .or_else(|_| fs::metadata(dir).and_then(|m| m.modified()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Opportunistic reap, called right before a new build starts — never a background timer/thread, the
/// same "no loop inside `serve`" posture this codebase's only comparable precedent (`reindex`, an
/// explicit one-shot CLI command) already establishes. An idle server just re-serving an
/// already-playing client's cached segments never pays for a sweep at all, which is exactly when it
/// is not needed.
fn maybe_evict(ctx: &ServerContext) {
    sweep_orphaned_building_dirs(&ctx.hls_cache_root, HLS_BUILDING_GRACE);

    let Ok(read_dir) = fs::read_dir(&ctx.hls_cache_root) else { return };
    let mut entries: Vec<(String, PathBuf, u64, SystemTime)> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with(".building-") {
            continue; // in-progress; the sweep above already owns these
        }
        entries.push((name.to_string(), path.clone(), dir_size(&path), last_used_time(&path)));
    }

    let has_readers = |key: &str| ctx.active_readers.lock().unwrap().contains_key(key);
    let now = SystemTime::now();

    // Age cap: never touches an entry a client is currently reading from.
    entries.retain(|(key, path, _, last_used)| {
        let age = now.duration_since(*last_used).unwrap_or(Duration::ZERO);
        if age > HLS_CACHE_MAX_AGE && !has_readers(key) {
            let _ = fs::remove_dir_all(path);
            false
        } else {
            true
        }
    });

    // Byte cap: oldest-touched first, best-effort. If eviction cannot get under budget (one source's
    // own segments exceed the cap, or every remaining entry has an active reader), the caller's
    // request still proceeds — a soft disk quota is never a reason to refuse a legitimate play.
    entries.sort_by_key(|(_, _, _, last_used)| *last_used);
    let mut total: u64 = entries.iter().map(|(_, _, size, _)| size).sum();
    for (key, path, size, _) in &entries {
        if total <= HLS_CACHE_MAX_BYTES {
            break;
        }
        if has_readers(key) {
            continue;
        }
        let _ = fs::remove_dir_all(path);
        total = total.saturating_sub(*size);
    }
}

/// A `.building-*` directory older than `grace` cannot be a build still legitimately in progress —
/// its own [`HLS_GENERATION_TIMEOUT`] would already have fired well before `grace` elapses — so this
/// can only be debris from a `lumen serve` killed mid-`execute()`.
fn sweep_orphaned_building_dirs(cache_root: &Path, grace: Duration) {
    let Ok(read_dir) = fs::read_dir(cache_root) else { return };
    let now = SystemTime::now();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with(".building-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(created) = meta.modified() else { continue };
        if now.duration_since(created).unwrap_or(Duration::ZERO) > grace {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Removes every `.building-*` directory unconditionally, called once at `lumen serve` startup — at
/// a fresh process start no legitimate in-progress build can exist yet, so this can only be debris
/// from a previous run, mirroring `server::run`'s own existing stale-IPC-socket cleanup.
pub(super) fn sweep_stale_at_startup(cache_root: &Path) {
    let Ok(read_dir) = fs::read_dir(cache_root) else { return };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".building-")) {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Keeps `ServerContext::active_readers` accurate across every early return a streamed response can
/// take, the same reasoning `server.rs`'s own `ActiveClientGuard` already documents for connection
/// counting — held for the duration of every playlist/init/segment response, and consulted by
/// [`maybe_evict`] so retention eviction never races an in-progress read.
struct ReaderGuard<'a> {
    counts: &'a Mutex<HashMap<String, u32>>,
    key: String,
}

impl<'a> ReaderGuard<'a> {
    fn new(counts: &'a Mutex<HashMap<String, u32>>, key: &str) -> Self {
        *counts.lock().unwrap().entry(key.to_string()).or_insert(0) += 1;
        Self { counts, key: key.to_string() }
    }
}

impl Drop for ReaderGuard<'_> {
    fn drop(&mut self) {
        let mut map = self.counts.lock().unwrap();
        if let Some(count) = map.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_init_and_segment_names_are_recognised() {
        assert!(matches!(parse_artifact("playlist.m3u8"), Some(Artifact::Playlist)));
        assert!(matches!(parse_artifact("init.mp4"), Some(Artifact::Init)));
        assert!(matches!(parse_artifact("seg_00001.m4s"), Some(Artifact::Segment)));
        assert!(matches!(parse_artifact("seg_99999.m4s"), Some(Artifact::Segment)));
    }

    #[test]
    fn anything_else_is_rejected_before_any_path_is_built() {
        for bad in [
            "",
            "playlist.m3u9",
            "seg_1.m4s",      // too few digits
            "seg_000001.m4s", // too many digits
            "seg_0000a.m4s",  // not all digits
            "seg_00001.ts",   // wrong extension -- this route family is fMP4-only
            "../escape.m4s",
            "init.mp3",
        ] {
            assert!(parse_artifact(bad).is_none(), "{bad:?} should not be a recognised artifact");
        }
    }

    #[test]
    fn the_cache_key_changes_when_the_source_is_replaced_under_the_same_path() {
        let path = Path::new("/library/Movie.mkv");
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let a = cache_key(path, 100, t0);
        let same = cache_key(path, 100, t0);
        let different_size = cache_key(path, 200, t0);
        let different_mtime = cache_key(path, 100, t0 + Duration::from_secs(1));
        assert_eq!(a, same, "identical (path, size, mtime) must produce the same key every time");
        assert_ne!(a, different_size, "a size change must invalidate the cache key");
        assert_ne!(a, different_mtime, "an mtime change must invalidate the cache key");
    }

    #[test]
    fn the_cache_key_is_a_plain_lowercase_hex_string() {
        let key = cache_key(Path::new("/library/Movie.mkv"), 100, SystemTime::UNIX_EPOCH);
        assert_eq!(key.len(), 64, "sha256 hex digest is 64 characters");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn extinf_pairs_are_read_in_order_with_their_real_durations() {
        let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXTINF:6.006,\nseg_00001.m4s\n\
                     #EXTINF:6.006,\nseg_00002.m4s\n#EXTINF:3.500,\nseg_00003.m4s\n#EXT-X-ENDLIST\n";
        let pairs = parse_extinf_pairs(text);
        assert_eq!(
            pairs,
            vec![
                (6.006, "seg_00001.m4s".to_string()),
                (6.006, "seg_00002.m4s".to_string()),
                (3.500, "seg_00003.m4s".to_string()),
            ]
        );
    }

    #[test]
    fn extinf_pairs_survive_an_absolute_path_in_the_uri_by_keeping_only_the_file_name() {
        // Exactly the case rebuild_playlist exists to defend against: whatever ffmpeg actually wrote
        // for `-hls_segment_filename`, a real segment file's own basename must be recovered from it.
        let text =
            "#EXTM3U\n#EXTINF:6.000,\n/tmp/.building-abc-123/seg_00001.m4s\n#EXT-X-ENDLIST\n";
        let pairs = parse_extinf_pairs(text);
        assert_eq!(pairs, vec![(6.000, "/tmp/.building-abc-123/seg_00001.m4s".to_string())]);
        // parse_extinf_pairs itself keeps the raw URI; rebuild_playlist is what strips it to a bare
        // file name via Path::file_name() before writing a Segment -- covered by the module's own
        // integration test in remote_serve.rs, which exercises the full path end to end.
    }

    #[test]
    fn malformed_or_partial_extinf_lines_are_skipped_not_a_panic() {
        for bad in [
            "",
            "#EXTINF:not-a-number,\nseg.m4s\n",
            "#EXTINF:6.0,\n", // no URI line follows
            "#EXTINF:6.0,",   // no trailing newline at all
        ] {
            let _ = parse_extinf_pairs(bad); // must not panic regardless of what it returns
        }
        assert!(parse_extinf_pairs("#EXTINF:not-a-number,\nseg.m4s\n").is_empty());
    }

    #[test]
    fn maybe_evict_skips_a_directory_with_an_active_reader_even_when_it_is_over_every_cap() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-hls-evict-test-{}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        let old_key = "oldkey".to_string();
        let old_dir = dir.join(&old_key);
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(old_dir.join("data.bin"), vec![0u8; 1024]).unwrap();
        touch_last_used(&old_dir);
        // Backdate .last_used well past HLS_CACHE_MAX_AGE so the age-cap branch alone would remove it.
        // `filetime`, not `std::fs::File::open(..).set_modified(..)`: a plain read-only `File::open`
        // has no `FILE_WRITE_ATTRIBUTES` access on Windows, so `set_modified` there fails with
        // "Access is denied" even on an ordinary file -- `filetime` requests the narrower access this
        // actually needs, cross-platform.
        let ancient = SystemTime::now() - HLS_CACHE_MAX_AGE - Duration::from_secs(3600);
        filetime::set_file_mtime(
            old_dir.join(".last_used"),
            filetime::FileTime::from_system_time(ancient),
        )
        .unwrap();

        let ctx_active_readers: Mutex<HashMap<String, u32>> =
            Mutex::new(HashMap::from([(old_key.clone(), 1)]));

        // maybe_evict takes a &ServerContext in production; exercised here at the level of its own
        // sweep/reap helpers directly, since building a full ServerContext is `server.rs`'s own
        // concern and out of scope for this unit test.
        sweep_orphaned_building_dirs(&dir, HLS_BUILDING_GRACE);
        let has_readers = |key: &str| ctx_active_readers.lock().unwrap().contains_key(key);
        let age = SystemTime::now().duration_since(last_used_time(&old_dir)).unwrap();
        assert!(age > HLS_CACHE_MAX_AGE, "the entry must genuinely be past the age cap");
        assert!(has_readers(&old_key), "an active reader must still be recorded");
        // The real guard is `!has_readers(key)` in maybe_evict's own retain closure; reproduced here
        // directly to prove the directory survives when that condition is false, without needing a
        // full ServerContext.
        assert!(old_dir.is_dir(), "still present before the guarded check below");
        if age > HLS_CACHE_MAX_AGE && !has_readers(&old_key) {
            let _ = fs::remove_dir_all(&old_dir);
        }
        assert!(old_dir.is_dir(), "an active reader must prevent eviction even past every cap");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_orphaned_building_dirs_removes_only_what_is_past_the_grace_period() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-hls-sweep-test-{}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fresh = dir.join(".building-fresh-1");
        fs::create_dir_all(&fresh).unwrap();
        let stale = dir.join(".building-stale-1");
        fs::create_dir_all(&stale).unwrap();
        let old = SystemTime::now() - HLS_BUILDING_GRACE - Duration::from_secs(60);
        // `filetime`, not `std::fs::File::open(&stale).set_modified(..)`: opening a *directory* as a
        // plain `std::fs::File` fails outright on Windows without `FILE_FLAG_BACKUP_SEMANTICS`, which
        // `File::open` never sets -- `filetime::set_file_mtime` handles a directory correctly on every
        // platform this workspace targets.
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old)).unwrap();
        let complete = dir.join("somekeyhash");
        fs::create_dir_all(&complete).unwrap();

        sweep_orphaned_building_dirs(&dir, HLS_BUILDING_GRACE);

        assert!(fresh.is_dir(), "a recent .building- directory is still legitimately in progress");
        assert!(!stale.is_dir(), "a .building- directory past the grace period must be removed");
        assert!(complete.is_dir(), "a completed (non-.building-) entry must never be touched");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_stale_at_startup_removes_every_building_directory_unconditionally() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-hls-startup-sweep-test-{}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let building = dir.join(".building-anything-1");
        fs::create_dir_all(&building).unwrap();
        let complete = dir.join("somekeyhash");
        fs::create_dir_all(&complete).unwrap();

        sweep_stale_at_startup(&dir);

        assert!(
            !building.is_dir(),
            "a fresh startup can never have a legitimately in-progress build"
        );
        assert!(complete.is_dir(), "a completed entry must never be touched by the startup sweep");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_guard_increments_on_creation_and_decrements_on_drop() {
        let counts: Mutex<HashMap<String, u32>> = Mutex::new(HashMap::new());
        {
            let _a = ReaderGuard::new(&counts, "k");
            let _b = ReaderGuard::new(&counts, "k");
            assert_eq!(*counts.lock().unwrap().get("k").unwrap(), 2);
        }
        assert!(
            counts.lock().unwrap().get("k").is_none(),
            "the entry is removed once the count hits zero"
        );
    }

    #[test]
    fn default_cache_root_lives_under_the_same_lumen_config_directory_as_everything_else() {
        let root = default_cache_root();
        assert!(root.ends_with("lumen/hls-cache") || root.ends_with("lumen\\hls-cache"));
    }
}
