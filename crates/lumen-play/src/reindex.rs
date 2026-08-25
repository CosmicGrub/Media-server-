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

/// Run one incremental reindex pass against `db`: load the persisted index (or start empty),
/// reconcile it against what `scan::candidate_paths` finds under `root` right now -- probing only
/// what a cheap fingerprint says has actually changed -- and save the result back.
///
/// Returns the loaded-and-updated index plus the report of what happened, so a caller can print a
/// summary, decide whether to tell paired clients the library changed, or both.
pub fn run(root: &Path, db: &Path) -> Result<(Index, ReindexReport), String> {
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
}
