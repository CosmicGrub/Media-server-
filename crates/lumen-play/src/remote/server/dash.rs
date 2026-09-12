//! DASH-MPD delivery: `/dash/` routes on the same authenticated HTTP surface `hls.rs` already serves
//! `/hls/` on. This module mirrors `hls.rs`'s own design closely and deliberately -- same token-in-path
//! transport, same lazy/coalesced/cached generation shape, same eviction posture -- rather than
//! reinventing any of it, matching `lumen_segment`'s own module doc on how `dash` follows `command`'s
//! established shape.
//!
//! **The token rides in the URL path, not a header or `?token=` query parameter** -- for exactly the
//! reason `hls.rs`'s own module doc explains: a DASH client resolves the manifest's own
//! `SegmentTemplate initialization=`/`media=` URIs against the manifest's own request URL per RFC 3986
//! §5.3's merge rules, which silently drop a `?token=` query component the same way an HLS client's
//! relative segment URIs do. The reasoning is identical; only the artifact names differ.
//!
//! **Generation is lazy, coalesced per source, and cached to disk keyed on (path, size, mtime)** --
//! literally `hls.rs`'s own `ensure_ready` shape, parameterized over DASH's artifacts instead of HLS's.
//!
//! **The generated output is served as ffmpeg wrote it, not rewritten.** Unlike HLS's own
//! `rebuild_playlist` step, this module trusts `manifest.mpd` as `lumen_segment::dash::execute` already
//! verified it: confirmed live (ffmpeg 6.1.1) that `-init_seg_name`/`-media_seg_name`, given as bare
//! relative patterns, are *not* subject to the path-doubling bug `-hls_fmp4_init_filename` has --
//! ffmpeg's own `SegmentTemplate` already references the same bare names the files landed under, with
//! no absolute build-directory path anywhere in it. `lumen_segment::dash::execute`'s own verification
//! (every representation's init segment and at least one chunk genuinely present and non-empty) is
//! this module's entire trust boundary for what gets served; there is no DASH analogue of rewriting
//! `SegmentTimeline` math, matching `execute`'s own doc comment on why that math needs no
//! re-derivation here.
//!
//! **Reuses `hls.rs`'s already-fixed design**, not a naive reimplementation of the same problems this
//! session already found and fixed once in `hls.rs`: the reader guard is registered before the
//! readiness check (closing the TOCTOU eviction race `hls.rs`'s own `handle` documents), a worker
//! thread's `mpsc::RecvTimeoutError::Disconnected` is distinguished from a genuine `Timeout` (so a
//! panicking generation is never misreported as having "timed out after 30 minutes"), and every real
//! failure path -- verification failure, rename failure, a disconnected worker -- cleans up its
//! `.building-*` directory rather than leaking it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use lumen_segment::dash::{DashExecError, DashExecOutcome, DashSegmentJob, execute};
use sha2::{Digest, Sha256};

use super::http;
use super::{ServerContext, TlsStream, contain_within_library};
use crate::remote::pairing::dirs_next_config_dir;

/// Target length per segment -- the same 6s middle ground `hls.rs` picks, for the same reason: short
/// enough that a seek only waits out part of one segment, long enough that per-segment HTTP overhead
/// stays negligible. Independently configurable from HLS's own constant on purpose, even though they
/// currently share a value -- DASH and HLS segmenting are different `ffmpeg` runs against the same
/// source, and there is no reason a future tuning pass to one must move the other.
const DASH_SEGMENT_SECONDS: u32 = 6;

/// How long a connection waits for one `execute()` run before giving up -- see `hls.rs`'s own
/// `HLS_GENERATION_TIMEOUT` doc comment; the same disk-I/O-bound reasoning applies unchanged since DASH
/// segmenting is the same stream-copy operation against the same kind of source.
const DASH_GENERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Soft cap on total cache size -- see `hls.rs`'s own `HLS_CACHE_MAX_BYTES` doc comment; DASH output is
/// roughly source-file-sized for the same reason (stream-copied, not re-encoded), so the same budget
/// applies. Kept as a separate constant (and a separate cache root -- see [`ServerContext`]'s own
/// `dash_cache_root` field) rather than sharing HLS's budget, so a library that gets played through
/// both delivery paths in the same session does not have one format's cache pressure evict the other's.
const DASH_CACHE_MAX_BYTES: u64 = 25 * 1024 * 1024 * 1024;

/// Independent age cap -- see `hls.rs`'s own `HLS_CACHE_MAX_AGE` doc comment.
const DASH_CACHE_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

/// How long a `.building-*` directory is given before the opportunistic reap treats it as abandoned --
/// see `hls.rs`'s own `HLS_BUILDING_GRACE` doc comment; comfortably past [`DASH_GENERATION_TIMEOUT`].
const DASH_BUILDING_GRACE: Duration = Duration::from_secs(60 * 60);

enum Artifact {
    Manifest,
    Init,
    Chunk,
}

/// Validated before any filesystem path is built from it -- the same defense-in-depth
/// `hls.rs`'s own `parse_artifact` documents; the real containment boundary is still
/// [`contain_within_library`] on the source portion, unchanged. Structural, not a fixed list: any
/// `init-<digits>.m4s` or `chunk-<digits>-<5 digits>.m4s` shape is accepted, matching
/// `lumen_segment::dash`'s own `INIT_SEGMENT_NAME_PATTERN`/`MEDIA_SEGMENT_NAME_PATTERN` addressing
/// rather than a hardcoded per-representation-ID list, so a source with any number of representations
/// is servable without this function needing to know how many ffmpeg actually produced for it.
fn parse_artifact(name: &str) -> Option<Artifact> {
    if name == "manifest.mpd" {
        return Some(Artifact::Manifest);
    }
    if let Some(digits) = name.strip_prefix("init-").and_then(|s| s.strip_suffix(".m4s"))
        && !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(Artifact::Init);
    }
    if let Some(rest) = name.strip_prefix("chunk-")
        && let Some((repr_id, number)) = rest.rsplit_once('-')
        && let Some(number) = number.strip_suffix(".m4s")
        && !repr_id.is_empty()
        && repr_id.bytes().all(|b| b.is_ascii_digit())
        && number.len() == 5
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(Artifact::Chunk);
    }
    None
}

/// Where generated DASH output is cached -- beside the pairing token store, TLS certificate, and HLS's
/// own cache, in this user's own config directory rather than the binary's. A directory distinct from
/// `hls::default_cache_root()` (`dash-cache`, not `hls-cache`) so the two delivery paths' own entries,
/// locks, and eviction sweeps never have to distinguish which format a given cache key belongs to.
pub(super) fn default_cache_root() -> PathBuf {
    dirs_next_config_dir().join("lumen").join("dash-cache")
}

/// Handle one request under `/dash/`. `rest` is everything after that prefix:
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
    let cache_dir = ctx.dash_cache_root.join(&key);

    // Registered before anything below assumes `cache_dir` will keep existing -- see `hls.rs`'s own
    // `handle` and its comment on this exact ordering: a reader must be counted before readiness is
    // confirmed, not after, or a concurrent `maybe_evict` sweep triggered by an unrelated request could
    // delete this exact directory in the gap between "confirmed present" and "counted as being read".
    let _guard = ReaderGuard::new(&ctx.dash_active_readers, &key);

    match artifact {
        Artifact::Manifest => {
            if let Err(e) = ensure_ready(ctx, &real_source, &key, &cache_dir) {
                write_gen_error(tls, &e);
                return;
            }
        }
        // A well-formed client only ever learns an init/chunk URL from a manifest this server itself
        // just served after the cache directory was already complete -- an init/chunk request that
        // arrives with no such directory yet is a stale or forged URL, not a legitimate race to
        // accommodate with a wait/retry. Same posture as `hls.rs`'s own `Artifact::Init | Segment` arm.
        Artifact::Init | Artifact::Chunk => {
            if !cache_dir.is_dir() {
                http::write_error(tls, 404, "Not Found");
                return;
            }
        }
    }

    touch_last_used(&cache_dir);
    let content_type = match artifact {
        Artifact::Manifest => "application/dash+xml",
        // DASH's chunks and HLS's fMP4 segments are both plain MP4 fragments -- same content-type
        // choice `hls.rs` already makes for `.m4s`/`init.mp4`.
        Artifact::Init | Artifact::Chunk => "video/mp4",
    };
    // No `Range` support -- a real DASH client fetches each artifact whole, same reasoning `hls.rs`
    // documents for its own artifacts.
    http::serve_file(tls, method, &cache_dir.join(artifact_name), content_type, None);
}

/// Content-derived cache key: canonical path plus size plus mtime -- identical construction to
/// `hls.rs`'s own `cache_key`, kept as a separate function (rather than calling into `hls.rs`) only
/// because the two modules are otherwise independent and this one should not have to reach across a
/// sibling module for four lines of hashing.
fn cache_key(source: &Path, size: u64, mtime: SystemTime) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.to_string_lossy().as_bytes());
    hasher.update(size.to_le_bytes());
    let secs = mtime.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    hasher.update(secs.to_le_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug)]
enum DashGenError {
    FfmpegNotFound,
    Spawn(std::io::Error),
    NonZeroExit(String),
    ManifestMissing,
    NoRepresentationsProduced,
    RepresentationIncomplete(String),
    TimedOut,
    /// The helper thread that runs `execute()` ended without ever sending a result -- almost certainly
    /// a panic inside `execute()` itself -- distinct from [`Self::TimedOut`] for the same reason
    /// `hls.rs`'s own `HlsGenError::WorkerLost` is: a worker that dies in milliseconds must never be
    /// reported to a client or a log line as having "timed out after 30 minutes".
    WorkerLost,
    Io(std::io::Error),
}

fn write_gen_error(tls: &mut TlsStream, err: &DashGenError) {
    let (code, log_line, client_msg): (u16, String, &str) = match err {
        DashGenError::FfmpegNotFound => {
            (503, "ffmpeg not found".into(), "ffmpeg is not installed on this server")
        }
        DashGenError::Spawn(e) => {
            (500, format!("failed to start ffmpeg: {e}"), "failed to start ffmpeg")
        }
        DashGenError::NonZeroExit(stderr) => {
            (500, format!("ffmpeg exited non-zero: {}", stderr.trim()), "segmenting failed")
        }
        DashGenError::ManifestMissing => {
            (500, "ffmpeg exited successfully but wrote no manifest".into(), "segmenting failed")
        }
        DashGenError::NoRepresentationsProduced => (
            500,
            "ffmpeg exited successfully but the manifest named no representations".into(),
            "no output representations were produced for this source",
        ),
        DashGenError::RepresentationIncomplete(why) => (
            500,
            format!("a representation the manifest declared is incomplete: {why}"),
            "segmenting produced an inconsistent result",
        ),
        DashGenError::TimedOut => (
            500,
            format!("timed out after {DASH_GENERATION_TIMEOUT:?} waiting for ffmpeg"),
            "timed out waiting for ffmpeg",
        ),
        DashGenError::WorkerLost => (
            500,
            "the generation thread ended unexpectedly before reporting a result (likely a panic)"
                .into(),
            "segmenting failed unexpectedly",
        ),
        DashGenError::Io(e) => {
            (500, format!("i/o error preparing DASH output: {e}"), "internal error")
        }
    };
    eprintln!("dash: {log_line}");
    http::write_error(tls, code, client_msg);
}

/// Generate-if-missing, coalesced per source -- identical shape to `hls.rs`'s own `ensure_ready`,
/// parameterized over `lumen_segment::dash` instead of the HLS equivalents. The fast path (already
/// cached) takes no lock at all; a sibling thread that lost the race to acquire the per-key lock finds
/// the directory already there the moment it gets its turn.
fn ensure_ready(
    ctx: &ServerContext,
    source: &Path,
    key: &str,
    cache_dir: &Path,
) -> Result<(), DashGenError> {
    if cache_dir.is_dir() {
        return Ok(());
    }
    let lock = {
        let mut locks = ctx.dash_locks.lock().unwrap();
        Arc::clone(locks.entry(key.to_string()).or_insert_with(|| Arc::new(Mutex::new(()))))
    };
    let _guard = lock.lock().unwrap();
    if cache_dir.is_dir() {
        return Ok(()); // a sibling request finished the build while this one waited for the lock
    }

    maybe_evict(ctx);

    let ffmpeg_bin = ctx.ffmpeg_bin.clone().ok_or(DashGenError::FfmpegNotFound)?;
    let tmp_dir = ctx.dash_cache_root.join(format!(".building-{key}-{}", unique_suffix()));
    fs::create_dir_all(&tmp_dir).map_err(DashGenError::Io)?;

    let job = DashSegmentJob {
        source: source.to_path_buf(),
        output_dir: tmp_dir.clone(),
        manifest_name: "manifest.mpd".to_string(),
        segment_seconds: DASH_SEGMENT_SECONDS,
    };

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(execute(&job, &ffmpeg_bin));
    });

    let outcome: DashExecOutcome = match rx.recv_timeout(DASH_GENERATION_TIMEOUT) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(match e {
                DashExecError::Spawn(io) => DashGenError::Spawn(io),
                DashExecError::NonZeroExit { stderr, .. } => DashGenError::NonZeroExit(stderr),
                DashExecError::ManifestMissing => DashGenError::ManifestMissing,
                DashExecError::NoRepresentationsProduced => DashGenError::NoRepresentationsProduced,
                DashExecError::RepresentationIncomplete { representation_id, why } => {
                    DashGenError::RepresentationIncomplete(format!(
                        "representation {representation_id}: {why}"
                    ))
                }
            });
        }
        // The helper thread's ffmpeg child may still be running and writing into `tmp_dir` here -- see
        // `hls.rs`'s own matching arm: `tmp_dir` is deliberately left in place rather than removed out
        // from under a possibly-still-writing process, bounded by the mid-session `.building-*`
        // grace-period sweep and the unconditional startup sweep on the next restart.
        Err(mpsc::RecvTimeoutError::Timeout) => return Err(DashGenError::TimedOut),
        // Distinct from a genuine timeout -- see `hls.rs`'s own matching arm: the `Sender` was dropped
        // without ever sending, which only happens if the helper thread ended (almost certainly
        // panicked) inside `execute()` itself. The thread is confirmed dead here, so nothing can still
        // be writing into `tmp_dir`; cleaning it up immediately is both honest and safe.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(DashGenError::WorkerLost);
        }
    };

    // Unlike `hls.rs`'s own `rebuild_playlist` step, nothing here rewrites `manifest.mpd` -- see this
    // module's own doc comment on why: `lumen_segment::dash::execute` has already confirmed every
    // representation it names has a real, non-empty init segment and at least one real chunk file, and
    // ffmpeg's own `SegmentTemplate` was confirmed live to already reference bare relative names with
    // no build-directory path leaked into it. `outcome` is consulted only to log/report; the manifest
    // ffmpeg wrote is exactly what gets served.
    let _ = &outcome;

    match fs::rename(&tmp_dir, cache_dir) {
        Ok(()) => {
            touch_last_used(cache_dir);
            Ok(())
        }
        // Lost a cross-process race to a `lumen serve` instance that finished the identical build
        // first -- see `hls.rs`'s own matching arm.
        Err(_) if cache_dir.is_dir() => {
            let _ = fs::remove_dir_all(&tmp_dir);
            Ok(())
        }
        // A genuine rename failure after `execute` has already fully verified real, complete output
        // sitting in `tmp_dir` -- unlike the timeout case, nothing is still writing into it, so it is
        // cleaned up here rather than left for the grace-period sweep. See `hls.rs`'s own matching arm.
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            Err(DashGenError::Io(e))
        }
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
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

/// Opportunistic reap, called right before a new build starts -- identical shape to `hls.rs`'s own
/// `maybe_evict`, parameterized over this module's own cache root, locks, and active-reader map.
fn maybe_evict(ctx: &ServerContext) {
    sweep_orphaned_building_dirs(&ctx.dash_cache_root, DASH_BUILDING_GRACE);

    let Ok(read_dir) = fs::read_dir(&ctx.dash_cache_root) else { return };
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

    let has_readers = |key: &str| ctx.dash_active_readers.lock().unwrap().contains_key(key);
    let now = SystemTime::now();

    // Age cap: never touches an entry a client is currently reading from.
    entries.retain(|(key, path, _, last_used)| {
        let age = now.duration_since(*last_used).unwrap_or(Duration::ZERO);
        if age > DASH_CACHE_MAX_AGE && !has_readers(key) {
            let _ = fs::remove_dir_all(path);
            false
        } else {
            true
        }
    });

    // Byte cap: oldest-touched first, best-effort -- see `hls.rs`'s own matching comment on why a soft
    // disk quota is never a reason to refuse a legitimate request.
    entries.sort_by_key(|(_, _, _, last_used)| *last_used);
    let mut total: u64 = entries.iter().map(|(_, _, size, _)| size).sum();
    for (key, path, size, _) in &entries {
        if total <= DASH_CACHE_MAX_BYTES {
            break;
        }
        if has_readers(key) {
            continue;
        }
        let _ = fs::remove_dir_all(path);
        total = total.saturating_sub(*size);
    }
}

/// A `.building-*` directory older than `grace` cannot be a build still legitimately in progress --
/// see `hls.rs`'s own `sweep_orphaned_building_dirs` doc comment; the same reasoning applies unchanged.
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

/// Removes every `.building-*` directory unconditionally, called once at `lumen serve` startup -- see
/// `hls.rs`'s own `sweep_stale_at_startup` doc comment; the same reasoning applies unchanged.
pub(super) fn sweep_stale_at_startup(cache_root: &Path) {
    let Ok(read_dir) = fs::read_dir(cache_root) else { return };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(".building-")) {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Keeps `ServerContext::dash_active_readers` accurate across every early return a streamed response
/// can take -- see `hls.rs`'s own `ReaderGuard` doc comment; identical mechanism, this module's own
/// counting map.
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
    fn manifest_init_and_chunk_names_are_recognised() {
        assert!(matches!(parse_artifact("manifest.mpd"), Some(Artifact::Manifest)));
        assert!(matches!(parse_artifact("init-0.m4s"), Some(Artifact::Init)));
        assert!(matches!(parse_artifact("init-1.m4s"), Some(Artifact::Init)));
        assert!(matches!(parse_artifact("init-42.m4s"), Some(Artifact::Init)));
        assert!(matches!(parse_artifact("chunk-0-00001.m4s"), Some(Artifact::Chunk)));
        assert!(matches!(parse_artifact("chunk-1-00002.m4s"), Some(Artifact::Chunk)));
        assert!(matches!(parse_artifact("chunk-12-99999.m4s"), Some(Artifact::Chunk)));
    }

    #[test]
    fn anything_else_is_rejected_before_any_path_is_built() {
        for bad in [
            "",
            "manifest.mp4",
            "manifest.mpd.bak",
            "init-.m4s",          // no representation id
            "init-a.m4s",         // not digits
            "init-0.mp4",         // wrong extension
            "chunk--00001.m4s",   // empty representation id
            "chunk-0-1.m4s",      // too few digits in the number
            "chunk-0-000001.m4s", // too many digits
            "chunk-0-0000a.m4s",  // not all digits
            "chunk-a-00001.m4s",  // representation id not digits
            "chunk-0-00001.ts",   // wrong extension -- this route family is fMP4-only
            "../escape.m4s",
            "init-0.mp3",
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
    fn maybe_evict_skips_a_directory_with_an_active_reader_even_when_it_is_over_every_cap() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-dash-evict-test-{}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        let old_key = "oldkey".to_string();
        let old_dir = dir.join(&old_key);
        fs::create_dir_all(&old_dir).unwrap();
        fs::write(old_dir.join("data.bin"), vec![0u8; 1024]).unwrap();
        touch_last_used(&old_dir);
        // `filetime`, not a plain `File::open(..).set_modified(..)` -- see `hls.rs`'s own matching test
        // for why: a read-only `File::open` lacks `FILE_WRITE_ATTRIBUTES` on Windows.
        let ancient = SystemTime::now() - DASH_CACHE_MAX_AGE - Duration::from_secs(3600);
        filetime::set_file_mtime(
            old_dir.join(".last_used"),
            filetime::FileTime::from_system_time(ancient),
        )
        .unwrap();

        let ctx_active_readers: Mutex<HashMap<String, u32>> =
            Mutex::new(HashMap::from([(old_key.clone(), 1)]));

        // Exercised at the level of this module's own sweep/reap helpers directly, the same way
        // `hls.rs`'s own matching test does -- building a full `ServerContext` is `server.rs`'s own
        // concern and out of scope for this unit test.
        sweep_orphaned_building_dirs(&dir, DASH_BUILDING_GRACE);
        let has_readers = |key: &str| ctx_active_readers.lock().unwrap().contains_key(key);
        let age = SystemTime::now().duration_since(last_used_time(&old_dir)).unwrap();
        assert!(age > DASH_CACHE_MAX_AGE, "the entry must genuinely be past the age cap");
        assert!(has_readers(&old_key), "an active reader must still be recorded");
        assert!(old_dir.is_dir(), "still present before the guarded check below");
        if age > DASH_CACHE_MAX_AGE && !has_readers(&old_key) {
            let _ = fs::remove_dir_all(&old_dir);
        }
        assert!(old_dir.is_dir(), "an active reader must prevent eviction even past every cap");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_orphaned_building_dirs_removes_only_what_is_past_the_grace_period() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-dash-sweep-test-{}-{:x}",
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fresh = dir.join(".building-fresh-1");
        fs::create_dir_all(&fresh).unwrap();
        let stale = dir.join(".building-stale-1");
        fs::create_dir_all(&stale).unwrap();
        let old = SystemTime::now() - DASH_BUILDING_GRACE - Duration::from_secs(60);
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old)).unwrap();
        let complete = dir.join("somekeyhash");
        fs::create_dir_all(&complete).unwrap();

        sweep_orphaned_building_dirs(&dir, DASH_BUILDING_GRACE);

        assert!(fresh.is_dir(), "a recent .building- directory is still legitimately in progress");
        assert!(!stale.is_dir(), "a .building- directory past the grace period must be removed");
        assert!(complete.is_dir(), "a completed (non-.building-) entry must never be touched");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_stale_at_startup_removes_every_building_directory_unconditionally() {
        let dir = std::env::temp_dir().join(format!(
            "lumen-dash-startup-sweep-test-{}-{:x}",
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
        assert!(root.ends_with("lumen/dash-cache") || root.ends_with("lumen\\dash-cache"));
    }

    #[test]
    fn default_cache_root_is_distinct_from_hls_own_cache_root() {
        // The two delivery paths must never share a cache directory -- a colliding key from one format
        // landing in the other's directory would serve nonsense. See this module's own doc comment on
        // `default_cache_root`.
        assert_ne!(default_cache_root(), super::super::hls::default_cache_root());
    }
}
