//! Background library integrity verification -- `docs/15-next-generation-engines.md` §B, wired to
//! this crate's own persisted index and a real, whole-file read.
//!
//! `lumen_index::Index::verify` decides which files are due, in what priority order, and enforces
//! the byte budget; this module supplies the one thing it deliberately does not do itself: actually
//! reading a file's bytes. It reuses `lumen_identity::digest_reader` -- the same streaming,
//! short-read-tolerant loop `ContentSketch`'s own `sketch_reader` already uses -- so a whole-file
//! hash never needs the file in memory at once.
//!
//! **Rate limiting, honestly scoped.** `lumen serve` has no notion yet of "something is currently
//! playing, back off" that a background pass inside the running server could consult -- that
//! integration does not exist. `lumen verify` is therefore a standalone invocation, not something
//! wired into `serve`'s own loop, and the byte budget below is the real rate limit: one run reads at
//! most `budget_bytes` before stopping, so a library too large to verify in one sitting is covered by
//! repeated invocations (a daily scheduled task, the same way `Install-LumenServeTask.ps1` already
//! keeps `serve` running) rather than one pass that never ends. Backing off specifically because a
//! movie is playing is the honest phase-2 item this leaves open, the same way Engine A left a live
//! filesystem watcher for later rather than claiming one that does not exist.

use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use lumen_identity::{FileDigest, digest_reader};
use lumen_index::{Index, VerifyReport, load, save};

/// A library goes a month between required re-checks by default -- long enough that this never
/// meaningfully competes with day-to-day use, short enough that bit rot on an actively-used library
/// does not sit undetected for years. Larger files come due sooner than this on their own --
/// `lumen_index`'s own risk-adjusted interval, not something this module has to know about.
pub const DEFAULT_REVERIFY_DAYS: u64 = 30;

/// How much file content one `lumen verify` invocation reads before stopping, by default -- a few
/// minutes of spinning-disk I/O, not an all-night job. A caller wanting an entire large library
/// verified does so via repeated invocations, each one picking up where the last left off,
/// tier-and-oldest-first, never favouring the same subset of files forever.
pub const DEFAULT_BUDGET_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Run one verification pass against `db`: load the persisted index, re-check whatever
/// [`lumen_index::Index::verify`] says is due within `budget_bytes`, and save the result back --
/// including any mismatch found, so it stays flagged for the next invocation until resolved.
pub fn run(
    db: &Path,
    reverify_days: u64,
    budget_bytes: u64,
) -> Result<(Index, VerifyReport), String> {
    let mut index = load(db).map_err(|e| format!("reading {}: {e}", db.display()))?;
    let now = unix_now();
    let reverify_after_secs = reverify_days.saturating_mul(24 * 60 * 60);

    let report = index.verify(now, reverify_after_secs, budget_bytes, |path| {
        digest_of(path).map_err(|e| e.to_string())
    });

    save(db, &index).map_err(|e| format!("writing {}: {e}", db.display()))?;
    Ok((index, report))
}

fn digest_of(path: &Path) -> io::Result<FileDigest> {
    let mut f = std::fs::File::open(path)?;
    digest_reader(&mut f)
}

fn unix_now() -> u64 {
    // A clock that somehow reads before the epoch has bigger problems than this pass; treating
    // that as "now is 0" rather than panicking is consistent with `lumen-play`'s own posture
    // elsewhere of degrading rather than crashing on an unreadable-but-non-fatal condition.
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A short, human-readable summary for the CLI.
pub fn summarize(report: &VerifyReport) -> String {
    let mut s = format!(
        "verify: {} confirmed, {} newly baselined, {} unchecked ({} bytes read)",
        report.confirmed, report.baseline_established, report.skipped_by_budget, report.bytes_read,
    );
    if !report.mismatched.is_empty() {
        s.push_str(&format!(
            "\n{} MISMATCH{}:",
            report.mismatched.len(),
            if report.mismatched.len() == 1 { "" } else { "ES" }
        ));
        for (path, since) in &report.mismatched {
            s.push_str(&format!(
                "\n  {} -- bytes changed since it was last confirmed good ({})",
                path.display(),
                format_since(*since)
            ));
        }
    }
    if !report.read_failed.is_empty() {
        s.push_str(&format!(
            "\n{} file{} could not be read this pass:",
            report.read_failed.len(),
            if report.read_failed.len() == 1 { "" } else { "s" }
        ));
        for (path, reason) in &report.read_failed {
            s.push_str(&format!("\n  {} -- {reason}", path.display()));
        }
    }
    s
}

/// A relative, human-readable age for a unix timestamp -- not a full date library for one line of
/// CLI output. Falls back to the raw timestamp for anything stranger than "some number of days ago",
/// which a clock that reads before the epoch or a caller in the far future both are.
fn format_since(unix_secs: u64) -> String {
    let now = unix_now();
    match now.checked_sub(unix_secs) {
        Some(elapsed) => {
            let days = elapsed / (24 * 60 * 60);
            if days == 0 {
                "less than a day ago".to_string()
            } else if days == 1 {
                "1 day ago".to_string()
            } else {
                format!("{days} days ago")
            }
        }
        None => format!("unix time {unix_secs}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let anchor = 0u8;
            let dir = std::env::temp_dir().join(format!(
                "lumen-verify-{tag}-{}-{:x}",
                std::process::id(),
                std::ptr::from_ref(&anchor) as usize
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seeded_index(records: &[(&Path, u64)]) -> Index {
        use lumen_identity::FsFingerprint;
        use lumen_index::{IndexRecord, ProbeResult};

        let mut idx = Index::default();
        for (path, size) in records {
            idx.insert_loaded(IndexRecord {
                path: (*path).to_path_buf(),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: *size, mtime_ns: 0 },
                probe: ProbeResult {
                    title: path.to_string_lossy().into_owned(),
                    year: None,
                    sketch: None,
                    needs_review: None,
                },
                tombstoned: false,
                last_verified: None,
                mismatch_pending: false,
            });
        }
        idx
    }

    #[test]
    fn a_first_verify_reads_the_real_file_and_establishes_a_baseline() {
        let d = TempDir::new("baseline");
        let path = d.file("a.mkv", b"real bytes on a real disk");
        let db = d.0.join(".lumen-index");
        save(&db, &seeded_index(&[(&path, 26)])).unwrap();

        let (index, report) = run(&db, DEFAULT_REVERIFY_DAYS, DEFAULT_BUDGET_BYTES).unwrap();

        assert_eq!(report.baseline_established, 1);
        assert!(!report.found_a_problem());
        assert!(index.get(&path).unwrap().last_verified.is_some());
    }

    #[test]
    fn a_real_byte_change_between_two_verify_passes_is_flagged() {
        let d = TempDir::new("mismatch");
        let path = d.file("a.mkv", b"original content, twenty bytes");
        let db = d.0.join(".lumen-index");
        save(&db, &seeded_index(&[(&path, 31)])).unwrap();

        run(&db, DEFAULT_REVERIFY_DAYS, DEFAULT_BUDGET_BYTES).unwrap();

        // Force the record due regardless of the real day-scale interval, by re-verifying with an
        // interval of zero -- this is exercising the real digest_of/File::open path end to end, not
        // re-testing lumen_index's own due-date logic (already covered there).
        std::fs::write(&path, b"the bytes have genuinely changed since the baseline").unwrap();
        let (index, report) = run(&db, 0, DEFAULT_BUDGET_BYTES).unwrap();

        assert_eq!(report.mismatched.len(), 1, "{report:?}", report = summarize(&report));
        assert!(index.get(&path).unwrap().mismatch_pending);
    }

    #[test]
    fn a_read_failure_is_reported_rather_than_guessed_at() {
        let d = TempDir::new("readfail");
        let missing = d.0.join("does-not-exist.mkv");
        let db = d.0.join(".lumen-index");
        save(&db, &seeded_index(&[(&missing, 100)])).unwrap();

        let (_, report) = run(&db, DEFAULT_REVERIFY_DAYS, DEFAULT_BUDGET_BYTES).unwrap();

        assert_eq!(report.read_failed.len(), 1);
        assert!(report.found_a_problem());
    }

    #[test]
    fn a_second_process_picks_up_what_the_first_persisted() {
        let d = TempDir::new("reload");
        let path = d.file("a.mkv", b"stable content");
        let db = d.0.join(".lumen-index");
        save(&db, &seeded_index(&[(&path, 14)])).unwrap();

        run(&db, DEFAULT_REVERIFY_DAYS, DEFAULT_BUDGET_BYTES).unwrap();
        // A fresh `run` call with nothing carried over in memory -- simulates a new `lumen verify`
        // process the next day finding the file still due but already baselined.
        let (_, second) = run(&db, 0, DEFAULT_BUDGET_BYTES).unwrap();

        assert_eq!(second.confirmed, 1, "must load the prior baseline from disk, not start blind");
    }

    #[test]
    fn summarize_names_the_mismatched_path_and_the_read_failure_reason() {
        let report = VerifyReport {
            confirmed: 1,
            baseline_established: 0,
            mismatched: vec![(PathBuf::from("/lib/broken.mkv"), 0)],
            read_failed: vec![(PathBuf::from("/lib/gone.mkv"), "not found".into())],
            bytes_read: 12345,
            skipped_by_budget: 0,
        };
        let s = summarize(&report);
        assert!(s.contains("/lib/broken.mkv"), "{s}");
        assert!(s.contains("/lib/gone.mkv"), "{s}");
        assert!(s.contains("not found"), "{s}");
    }
}
