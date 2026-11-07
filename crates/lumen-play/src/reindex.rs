//! Incremental library reindexing -- `lumen-index` wired to this crate's own scanner and probing.
//!
//! `lumen_index::Index::reindex` does the reconciliation (what changed, what moved, what vanished)
//! but deliberately knows nothing about walking a filesystem or reading a media file's bytes -- both
//! of those already exist, tested, in `scan.rs`. This module is the seam: `scan::candidate_paths` for
//! "what's on disk right now", `scan::classify` for "what is this file", called only when the index
//! says a path actually needs it.

use std::path::{Path, PathBuf};

use lumen_index::{Index, ProbeResult, ReindexReport, load, save};

use crate::scan;

/// Default location for a library's persisted index: a hidden file at the library root.
///
/// `scan::candidate_paths`'s own hidden-file skip rule (the same one `scan::scan`'s walker uses)
/// already excludes any dotfile from being treated as a media candidate, so the index quietly keeps
/// itself out of its own library listing with no special-casing required.
pub fn default_index_path(root: &Path) -> PathBuf {
    root.join(".lumen-index")
}

/// `persist::save`'s atomic temp-file-then-rename protects a reindex against a crash mid-write, but
/// nothing stops a *second* `lumen reindex` (or `lumen verify`) process from loading the same
/// on-disk snapshot while this one is still between its own `load` and `save` -- each would discover
/// a different newly-added file, and whichever saves second silently clobbers the file with its own
/// stale view, erasing the first process's already-persisted record. An exclusive lock on a sidecar
/// `.lock` file, held for the whole load-reindex-save cycle, closes that window: a second process
/// blocks here until the first's lock is dropped, by which point the first's save has already landed
/// and the second's own `load` picks it up instead of racing it.
fn lock_index(db: &Path) -> Result<std::fs::File, String> {
    let mut lock_path = db.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("locking {}: {e}", lock_path.display()))?;
    fs4::FileExt::lock(&file).map_err(|e| format!("locking {}: {e}", lock_path.display()))?;
    Ok(file)
}

/// Run one incremental reindex pass against `db`: load the persisted index (or start empty),
/// reconcile it against what `scan::candidate_paths` finds under `root` right now -- probing only
/// what a cheap fingerprint says has actually changed -- and save the result back.
///
/// Returns the loaded-and-updated index plus the report of what happened, so a caller can print a
/// summary, decide whether to tell paired clients the library changed, or both.
pub fn run(root: &Path, db: &Path) -> Result<(Index, ReindexReport), String> {
    let _lock = lock_index(db)?;

    let mut index = load(db).map_err(|e| format!("reading {}: {e}", db.display()))?;
    let observed = scan::candidate_paths(std::slice::from_ref(&root.to_path_buf()));

    let report = index
        .reindex(&observed, |path, _fingerprint| probe_one(path))
        .map_err(|e| e.to_string())?;

    save(db, &index).map_err(|e| format!("writing {}: {e}", db.display()))?;
    Ok((index, report))
}

/// The probe closure proper: real content-sniffed classification via `scan::classify`, called only
/// for a path `Index::reindex` has already decided is new or changed.
///
/// `identify: true` is not optional here the way it is for a plain `lumen scan` -- a persisted index
/// with no content sketch could never recognise a move, which is half the reason to persist one at
/// all. `None` means the path could not even be `stat`ed a second time between the caller's walk and
/// here (a genuine race, not a judgement about the file); every other outcome, however messy, is
/// reported through `ProbeResult` and stays indexed rather than being silently dropped.
fn probe_one(path: &Path) -> Option<ProbeResult> {
    let size = std::fs::metadata(path).ok()?.len();
    let f = scan::classify(path, size, true);
    let notes = f.notes();
    Some(ProbeResult {
        title: f.parsed.title,
        year: f.parsed.year,
        sketch: f.identity,
        needs_review: (!notes.is_empty()).then(|| notes.join("; ")),
    })
}

/// A short, human-readable line for the CLI -- not the only way to consume a [`ReindexReport`], but
/// the one `lumen reindex` prints.
pub fn summarize(index: &Index, report: &ReindexReport) -> String {
    format!(
        "index v{}: {} files ({} new, {} modified, {} moved, {} tombstoned, {} unchanged{})",
        index.version(),
        index.len(),
        report.new,
        report.modified,
        report.moved,
        report.tombstoned,
        report.unchanged,
        if report.failed > 0 { format!(", {} need review", report.failed) } else { String::new() },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let anchor = 0u8;
            let dir = std::env::temp_dir().join(format!(
                "lumen-reindex-{tag}-{}-{:x}",
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
            std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mkv_bytes() -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3];
        v.extend(std::iter::repeat_n(0u8, 64));
        v
    }

    #[test]
    fn a_first_reindex_discovers_everything_as_new() {
        let d = TempDir::new("first");
        d.file("Movie.mkv", &mkv_bytes());
        let db = d.0.join(".lumen-index");

        let (index, report) = run(&d.0, &db).unwrap();

        assert_eq!(report.new, 1);
        assert_eq!(index.len(), 1);
        assert!(db.exists(), "the index must actually be persisted to disk");
    }

    #[test]
    fn a_second_reindex_over_an_untouched_library_changes_nothing() {
        let d = TempDir::new("stable");
        d.file("Movie.mkv", &mkv_bytes());
        let db = d.0.join(".lumen-index");

        let (_, first) = run(&d.0, &db).unwrap();
        let (index, second) = run(&d.0, &db).unwrap();

        assert_eq!(second.unchanged, 1);
        assert_eq!(second.new, 0);
        assert_eq!(
            index.version(),
            1,
            "version bumps once on discovery, not again when nothing changed"
        );
        let _ = first;
    }

    #[test]
    fn the_index_file_itself_never_appears_in_its_own_library() {
        let d = TempDir::new("self-exclude");
        d.file("Movie.mkv", &mkv_bytes());
        let db = default_index_path(&d.0);

        run(&d.0, &db).unwrap();
        let (index, _) = run(&d.0, &db).unwrap();

        assert_eq!(index.len(), 1, "only the real movie, never the index's own dotfile");
    }

    #[test]
    fn a_second_reindex_process_picks_up_what_the_first_persisted() {
        let d = TempDir::new("reload");
        d.file("Movie.mkv", &mkv_bytes());
        let db = d.0.join(".lumen-index");

        run(&d.0, &db).unwrap();

        // Simulates a fresh `lumen reindex` invocation in a new process: nothing carried over in
        // memory, only what `run` persisted to `db`.
        let (index, report) = run(&d.0, &db).unwrap();
        assert_eq!(
            report.new, 0,
            "a fresh process must load prior state from disk, not start blind"
        );
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn lock_index_excludes_a_second_acquirer_while_the_first_holds_it() {
        let d = TempDir::new("lock-mutex");
        let db = d.0.join(".lumen-index");

        let first = lock_index(&db).unwrap();

        // The same mechanism a real second `lumen reindex` process would use to open the sidecar
        // lock file: a fresh `File` handle on the identical path. `flock` locks are scoped to the
        // open file description, not the process, so this is a faithful stand-in for "a second
        // process tries to start its own cycle" without actually spawning one.
        let mut lock_path = db.as_os_str().to_os_string();
        lock_path.push(".lock");
        let second_handle = std::fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
        assert!(
            matches!(fs4::FileExt::try_lock(&second_handle), Err(fs4::TryLockError::WouldBlock)),
            "a second process must not be able to acquire the lock while the first still holds it"
        );

        drop(first);
        fs4::FileExt::try_lock(&second_handle).expect(
            "once the first process's lock is released, a second must be able to acquire it",
        );
    }

    #[test]
    fn two_overlapping_reindex_cycles_lose_neither_discovery() {
        // Reproduces the finding's own race: two "processes" load the same on-disk snapshot, each
        // discovers a different newly-added file, and -- without the lock -- whichever saves second
        // used to silently clobber the file with its own stale view, erasing the first process's
        // already-persisted record. `lock_index` closes that window by holding the lock for the
        // whole load-reindex-save cycle, so a second process cannot even begin its own `load` until
        // the first's `save` has already landed.
        let d = TempDir::new("race");
        let existing = d.file("existing.mkv", &mkv_bytes());
        let movie_a = d.file("Movie A.mkv", &mkv_bytes());
        let movie_b = d.file("Movie B.mkv", &mkv_bytes());
        let db = d.0.join(".lumen-index");

        // Baseline, already indexed and persisted before either process starts -- matches the
        // finding's own repro's starting state ("baseline saved: 1 live record(s)").
        let mut baseline = Index::default();
        baseline.reindex(std::slice::from_ref(&existing), |path, _| probe_one(path)).unwrap();
        save(&db, &baseline).unwrap();

        let (lock_acquired_tx, lock_acquired_rx) = std::sync::mpsc::channel::<()>();
        let (cycle_done_tx, cycle_done_rx) = std::sync::mpsc::channel::<()>();
        let db_a = db.clone();
        let existing_a = existing.clone();
        let movie_a_a = movie_a.clone();

        // "Process A": acquires the lock first (signalled below, so ordering is deterministic
        // rather than a timing guess), then runs a full load-reindex-save cycle discovering
        // `Movie A.mkv`, and only releases the lock once that closure returns.
        let a = std::thread::spawn(move || {
            let _lock = lock_index(&db_a).unwrap();
            lock_acquired_tx.send(()).unwrap();

            let mut index = load(&db_a).unwrap();
            index
                .reindex(&[existing_a.clone(), movie_a_a.clone()], |path, _| probe_one(path))
                .unwrap();
            save(&db_a, &index).unwrap();

            cycle_done_tx.send(()).unwrap();
            // `_lock` drops here, at the end of the closure -- strictly after `save` and after the
            // `cycle_done` signal above.
        });

        // Wait until process A definitely holds the lock before "process B" tries for it, so the
        // test exercises a real, deterministic contention rather than a race on who gets there
        // first.
        lock_acquired_rx.recv().unwrap();

        // This is exactly where, before the fix, process B would load the same stale baseline
        // process A started from and go on to silently erase `Movie A.mkv` with its own save. With
        // the lock in place this call blocks -- a real OS-level `flock` wait, not a timeout -- until
        // process A's lock (and therefore its whole cycle, including its `save`) is gone.
        let lock_b = lock_index(&db).unwrap();
        assert!(
            cycle_done_rx.try_recv().is_ok(),
            "process B must not be able to start its own cycle until process A's has fully finished"
        );

        let mut index_b = load(&db).unwrap();
        index_b
            .reindex(&[existing.clone(), movie_a.clone(), movie_b.clone()], |path, _| {
                probe_one(path)
            })
            .unwrap();
        save(&db, &index_b).unwrap();
        drop(lock_b);
        a.join().unwrap();

        let final_index = load(&db).unwrap();
        assert!(
            final_index.get(&movie_a).is_some(),
            "process A's discovery must survive process B's save"
        );
        assert!(final_index.get(&movie_b).is_some(), "process B's own discovery must be present");
        assert!(final_index.get(&existing).is_some(), "the original baseline record must survive");
    }
}
