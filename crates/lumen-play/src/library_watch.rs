//! A debounced, self-trigger-proof filesystem watcher over one library root -- `docs/15` §A's
//! automatic-rescan phase-2 item, factored out of `remote::server` so `dlna` can run its own instance.
//!
//! **Code reuse, not shared state.** Each caller gets its own `notify` watcher, its own thread, and
//! its own `on_change` callback -- `remote::server::spawn_library_watcher` and `dlna::run` each call
//! [`spawn`] separately, against their own separately-scanned copies of the same library, and nothing
//! here couples the two. That is deliberate: `dlna.rs`'s module doc explains why the DLNA listener is
//! separate infrastructure from the paired control channel (a different, weaker trust posture that
//! may run with or without the other), and a single shared watcher fanning out to both would be the
//! first piece of state tying them back together. The cost -- two inotify watches on the same tree,
//! two full re-walks per real change when both are running -- is a handful of file descriptors and a
//! second scan of a personal library, and buying that back is not worth the coupling.
//!
//! **What this module does not decide.** It never scans anything itself and never knows what a
//! "rescan" means to its caller -- it only knows how to tell "something under `root` really changed,
//! and the burst has settled" apart from noise, and to call `on_change` exactly once per such event.
//! What the caller does in `on_change` (re-walk and swap a `Scan`, bump a version counter, log) is
//! the caller's own definition of a rescan, kept next to the manual trigger it already has where one
//! exists, so a manual and an automatic rescan can never drift apart.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::Watcher;

/// How long the watcher waits, after the most recent change event, before it actually calls
/// `on_change` -- `docs/15` §A's automatic-rescan phase-2 item. A single logical change (a batch copy
/// of dozens of files, or even one file's own create+write+close sequence on some platforms and
/// filesystems) arrives as a burst of many raw events; without coalescing them, a 50-file copy would
/// trigger up to 50 overlapping full re-walks racing each other instead of one that sees the finished
/// result. 1.5 seconds is comfortably longer than the gap between events within one such burst (they
/// land within milliseconds of each other) while still being short enough that a person watching a
/// single dropped-in file appear sees it show up in about the time it takes to glance back at the
/// screen, not something that reads as "did it even notice."
pub(crate) const WATCHER_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How long the watcher discards *every* event, of any kind, right after an `on_change` it just ran
/// -- filtering `EventKind::Access` alone (see [`spawn`]'s own comment) closes the self-triggering
/// loop confirmed live on Linux's inotify backend, but real Windows CI caught the same class of bug
/// surviving that filter: `ReadDirectoryChangesW` reports a rescan's own read of the library it just
/// walked as a *different* event shape than inotify's `IN_OPEN`/`IN_CLOSE` did, one this filter did
/// not anticipate and a second platform-specific kind-by-kind allowlist would be fragile to keep
/// chasing. Discarding unconditionally for a bounded window after every rescan closes the whole class
/// at once, regardless of which kind a given platform's backend happens to use for a rescan's own
/// footprint. This does not lose a real external change permanently: one landing inside this window
/// is simply picked up by whatever the *next* trigger's full re-walk finds, whenever that next trigger
/// fires -- the same "soft, eventually-consistent, a manual re-walk is the fallback" contract this
/// whole feature already has, not a hard real-time guarantee.
pub(crate) const POST_RESCAN_QUIET_PERIOD: Duration = WATCHER_DEBOUNCE;

/// Starts a background thread that watches `root` for filesystem changes and calls `on_change`
/// automatically, once a burst of events has gone quiet. Uses `notify`'s own recommended backend per
/// platform (inotify on Linux, `ReadDirectoryChangesW` on Windows, FSEvents on macOS) in recursive
/// mode, so a change anywhere under the root -- not just directly inside it -- is seen.
///
/// `label` prefixes every line this watcher logs (`"library watcher"`, `"DLNA library watcher"`) so an
/// operator reading one terminal with both instances running can tell which one is speaking.
/// `on_change` is called on the watcher's own thread, never concurrently with itself -- the thread
/// does not go back to listening until the callback has returned and the quiet period after it has
/// elapsed.
///
/// The watch covers *everything* under `root`, with no path-based exclusions: this module cannot
/// know which subdirectories are "really" library and which are not, and a wrong guess would be a
/// silently missed change. The one consequence worth naming: `lumen serve` writes its own state
/// (the pairing token store, the HLS/DASH segment caches) under the OS config directory, so an
/// operator who serves a root that *contains* that directory -- their whole home directory, say --
/// will see every segment the paired channel writes trigger a full re-walk and, for DLNA, a
/// `SystemUpdateID` bump with no library change behind it. Harmless but wasteful; the fix is to
/// serve the media directory rather than its parent, and the test fixtures in this crate keep their
/// config directory *beside* the library root for exactly this reason.
///
/// Never fails the caller. Initializing a watcher or starting the watch can fail for reasons entirely
/// outside this server's control -- permission denied, an exhausted inotify watch limit, an
/// unsupported filesystem -- and none of those are reasons the rest of `lumen serve` should refuse to
/// start. This mirrors the exact posture `remote::server::run` already takes with its cache-directory
/// creation and `ffmpeg` resolution: log a clear warning and continue running with that one piece of
/// functionality degraded or absent, never abort the whole server over it. Here, "absent" means
/// automatic refresh simply does not happen for this caller; whatever manual fallback the caller has
/// (the paired channel's `rescan` command; for DLNA, restarting `lumen serve`) is still there as the
/// fallback it already was before this function existed.
pub(crate) fn spawn(
    root: &Path,
    label: &'static str,
    mut on_change: impl FnMut() + Send + 'static,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = match notify::recommended_watcher(tx) {
        Ok(w) => w,
        Err(e) => {
            log(&format!(
                "warning: could not start the {label} ({e}); automatic rescans are disabled for \
                 this session"
            ));
            return;
        }
    };
    if let Err(e) = watcher.watch(root, notify::RecursiveMode::Recursive) {
        log(&format!(
            "warning: {label} could not watch {} for changes ({e}); automatic rescans are disabled \
             for this session",
            root.display()
        ));
        return;
    }
    log(&format!("{label}: watching {} for library changes", root.display()));

    std::thread::spawn(move || {
        // The watcher must outlive every event this thread ever reads from `rx` -- dropping it would
        // stop delivery immediately, the same "must outlive its user" shape `Mpv`'s own IPC
        // connection has in `remote::server::drive_mpv`. Moving it into this closure, never let go of
        // until the thread itself exits, is what keeps it alive for exactly that long and no longer.
        let _watcher = watcher;
        let closed = || {
            log(&format!(
                "{label}: event channel closed; automatic rescans are disabled for the rest of this \
                 session"
            ));
        };
        loop {
            // Block for the first event that actually means something changed. `EventKind::Access`
            // (a plain open/read/close, nothing created, written, removed, or renamed) is filtered
            // out here and below, not just as an optimization: a rescan's own walk opens every file
            // to sniff its header (see `scan.rs`'s module doc), and on Linux `notify`'s inotify
            // backend watches `IN_OPEN`/`IN_CLOSE` right alongside real changes. Without this
            // filter, every rescan's own read of the library it had just walked would look exactly
            // like a fresh external change, and this loop would never stop rescanning itself -- a
            // real self-triggering loop this feature's own live test caught by actually running long
            // enough to notice, not something reasoning about "a rescan doesn't write to the watched
            // directory" alone would have surfaced.
            loop {
                let event = match rx.recv() {
                    Ok(event) => event,
                    Err(_) => {
                        closed();
                        return;
                    }
                };
                match event {
                    Ok(e) if e.kind.is_access() => continue,
                    Ok(_) => break,
                    Err(e) => {
                        log(&format!("{label}: event error: {e}"));
                        continue;
                    }
                }
            }
            // Keep waiting for `WATCHER_DEBOUNCE` past the most recent *real* event, not the first --
            // this is what coalesces a burst (a multi-file copy, or one file's own create+write+close
            // sequence) into a single rescan instead of one per raw event. Tracked as an explicit
            // deadline rather than simply looping `recv_timeout(WATCHER_DEBOUNCE)` so that an
            // `Access` event arriving during the wait is discarded without pushing the deadline back
            // out, for the same self-triggering reason it was filtered out above.
            let mut deadline = Instant::now() + WATCHER_DEBOUNCE;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(Ok(e)) if e.kind.is_access() => {}
                    Ok(Ok(_)) => deadline = Instant::now() + WATCHER_DEBOUNCE,
                    Ok(Err(e)) => log(&format!("{label}: event error: {e}")),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        closed();
                        return;
                    }
                }
            }
            on_change();

            // Unconditionally discard every event for `POST_RESCAN_QUIET_PERIOD` before going back to
            // waiting for the next real change -- see that constant's own doc for why this exists on
            // top of the `is_access()` filter above, confirmed necessary by a real Windows CI failure.
            let quiet_until = Instant::now() + POST_RESCAN_QUIET_PERIOD;
            loop {
                let remaining = quiet_until.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(_) => {} // Any event, any kind, any platform -- discarded without comment.
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        closed();
                        return;
                    }
                }
            }
        }
    });
}
