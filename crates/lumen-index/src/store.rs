//! The index itself, and the incremental reconciliation algorithm.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lumen_identity::{
    ContentSketch, FileDigest, FileIdentity, FsFingerprint, ScanVerdict, classify,
};
use lumen_meta::{
    FieldGroup, MetadataBundle, MetadataFragment, ProviderRanking, Source, merge_fragments,
};

use crate::fingerprint::fs_fingerprint;

/// What a caller-supplied probe learned about a file that needed one -- new, moved, or modified.
///
/// Deliberately minimal: this is the honest v1 boundary for what the index persists per file. It is
/// enough to exercise `lumen-meta`'s field-merge system for real (see [`IndexRecord::bundle`]) even
/// though the only fragment source wired in today is the filename parse -- a provider fragment slots
/// in later without changing this shape, since `merge_fragments` already merges any number of them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub title: String,
    pub year: Option<u16>,
    pub sketch: Option<ContentSketch>,
    /// `Some(reason)` when the probe found something wrong but the file should stay indexed rather
    /// than be dropped -- the same "never let one bad file abort the batch" posture as
    /// `lumen-probe`'s truncation property and `verify_duplicate_group`'s per-file error handling.
    pub needs_review: Option<String>,
}

/// A whole-file digest this record was last confirmed against, and when -- `docs/15` §B, the
/// Integrity & Self-Healing Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    pub digest: FileDigest,
    pub at_unix_secs: u64,
}

/// One file's persisted record.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexRecord {
    pub path: PathBuf,
    pub fingerprint: FsFingerprint,
    pub probe: ProbeResult,
    /// The path stopped appearing on disk, so the record is kept rather than deleted -- a resume
    /// position or watch history keyed by content identity must survive a temporarily unavailable
    /// mount, not silently orphan the moment a network share hiccups.
    ///
    /// A path reappearing exactly where a tombstoned record left off is treated as a fresh discovery
    /// rather than a silent revival: resurrecting a possibly years-stale title/sketch without
    /// re-probing would be the same kind of quiet, unverified claim `verify_duplicate_group` exists
    /// to refuse to make about duplicate files.
    pub tombstoned: bool,
    /// The last whole-file digest [`Index::verify`] confirmed for this record, and when. `None`
    /// means never verified -- a fresh record from `reindex` always starts here, deliberately: a
    /// full-file read is real I/O `reindex` exists specifically to avoid paying on every pass, so
    /// establishing a baseline is `verify`'s job alone, not something a probe does opportunistically.
    pub last_verified: Option<Verified>,
    /// The most recent [`Index::verify`] pass over this record found a digest that did not match
    /// `last_verified` -- which is therefore still the last *confirmed-good* state, not the
    /// mismatching one. Drives top-priority re-selection (`docs/15` §B) until a later pass either
    /// confirms the same old digest again (a transient read is the honest read of that outcome) or
    /// `reindex` sees the file actually change, which starts verification over from a clean baseline.
    pub mismatch_pending: bool,
}

impl IndexRecord {
    /// The merged metadata bundle for this file, computed fresh on every call rather than cached.
    ///
    /// The only input today is the filename parse -- cheap, pure, and therefore safe to recompute on
    /// read, which means the bundle can never drift out of sync with the record that produced it.
    /// This is also the one line in the whole crate that actually calls `lumen_meta::merge_fragments`
    /// -- the moment `lumen-meta` stops being a crate nothing depends on.
    pub fn bundle(&self) -> MetadataBundle {
        let mut fragment = MetadataFragment::new(Source::Derived).with(
            FieldGroup::Titles,
            "title",
            self.probe.title.clone(),
        );
        if let Some(year) = self.probe.year {
            fragment = fragment.with(FieldGroup::ReleaseDates, "year", year.to_string());
        }
        merge_fragments(
            &MetadataBundle::default(),
            std::slice::from_ref(&fragment),
            &ProviderRanking::default(),
        )
    }
}

/// Tally of what one [`Index::reindex`] pass actually did, for a human or a log line to report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReindexReport {
    pub unchanged: usize,
    pub new: usize,
    pub modified: usize,
    pub moved: usize,
    pub tombstoned: usize,
    pub failed: usize,
}

impl ReindexReport {
    /// Whether anything actually changed -- the signal a caller uses to decide whether
    /// `library_version` needs bumping and paired clients need telling.
    pub fn changed(&self) -> bool {
        self.new > 0 || self.modified > 0 || self.moved > 0 || self.tombstoned > 0
    }
}

/// What one [`Index::verify`] pass found -- `docs/15` §B.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// Digest matched what was stored from a previous verification.
    pub confirmed: usize,
    /// No prior digest existed; this pass recorded the first one. Not a confirmation of anything --
    /// there was nothing yet to compare against.
    pub baseline_established: usize,
    /// Path, plus the unix-seconds timestamp of the *previous* successful verification -- so a
    /// caller can say how long ago the file was last known good, not just that it currently isn't.
    /// The record's own stored verification is deliberately left at that previous timestamp too
    /// (see `Index::verify`'s doc comment) rather than silently overwritten with the mismatching one.
    pub mismatched: Vec<(PathBuf, u64)>,
    /// Path and reason, for an entry that could not even be read this pass -- reported, never
    /// guessed at as either a confirmation or a mismatch.
    pub read_failed: Vec<(PathBuf, String)>,
    /// Total bytes read this pass, across every entry that was actually read (a failed read
    /// contributes nothing -- see `Index::verify`'s own doc comment). Counted from the entry's
    /// fingerprint size recorded at the last `reindex`, not a live re-stat of the file: if the file
    /// has genuinely grown or shrunk since then, this (and the byte budget itself, which uses the
    /// same figure to decide what fits) reports what was *expected* to be read, not necessarily the
    /// exact byte count `digest_of` really consumed. The digest comparison itself is unaffected --
    /// it always hashes the file's real, current bytes regardless of what the stale fingerprint
    /// says -- only this figure and the budget's own admission decision use the earlier estimate.
    pub bytes_read: u64,
    /// Entries that were due for re-verification but did not fit in this pass's budget. Not a
    /// failure -- they are simply carried over to the next invocation, oldest-verified-first.
    pub skipped_by_budget: usize,
}

impl VerifyReport {
    /// Whether this pass found something a person should look at.
    pub fn found_a_problem(&self) -> bool {
        !self.mismatched.is_empty() || !self.read_failed.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct Index {
    by_path: BTreeMap<PathBuf, IndexRecord>,
    version: u64,
}

impl Index {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn set_version(&mut self, v: u64) {
        self.version = v;
    }

    /// Insert a record as-is, bypassing reconciliation -- for loading from disk, and for tests.
    pub fn insert_loaded(&mut self, record: IndexRecord) {
        self.by_path.insert(record.path.clone(), record);
    }

    pub fn get(&self, path: &Path) -> Option<&IndexRecord> {
        self.by_path.get(path).filter(|r| !r.tombstoned)
    }

    /// Live (non-tombstoned) records only -- what a library listing shows.
    pub fn iter(&self) -> impl Iterator<Item = &IndexRecord> {
        self.by_path.values().filter(|r| !r.tombstoned)
    }

    /// Every record, tombstoned or not -- what persistence needs to write.
    pub fn all_including_tombstoned(&self) -> impl Iterator<Item = &IndexRecord> {
        self.by_path.values()
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reconcile against the set of paths a caller's own walker found on disk this pass, probing
    /// only what a cheap fingerprint check says actually needs it.
    ///
    /// `observed` is every media path the caller considers part of the library right now.
    /// `lumen-index` deliberately does not walk a filesystem itself: `lumen-play`'s `scan.rs` already
    /// has a careful, tested walker (symlink-loop guard, hidden/skip-dir rules, sample detection),
    /// and a second implementation here would be exactly the kind of divergence
    /// `CONTRIBUTING.md`'s Rule 1 warns against for the playback ladder -- the same reasoning applies
    /// to "what counts as a file in this library."
    ///
    /// `probe` is called only for a path with no live record at that exact path, or whose stored
    /// `(size, mtime)` fingerprint disagrees with a fresh `stat` -- the actual incremental win: a
    /// library that has not changed since the last pass triggers zero calls to it. It receives the
    /// path and its freshly-read fingerprint so it never has to re-stat. Returning `None` means the
    /// probe itself failed outright (could not even open the file); returning
    /// `Some(ProbeResult { needs_review: Some(_), .. })` means it opened but found something wrong --
    /// the file stays indexed either way, never silently dropped.
    pub fn reindex(
        &mut self,
        observed: &[PathBuf],
        mut probe: impl FnMut(&Path, &FsFingerprint) -> Option<ProbeResult>,
    ) -> std::io::Result<ReindexReport> {
        let mut report = ReindexReport::default();
        let observed_set: BTreeSet<&PathBuf> = observed.iter().collect();

        // Candidate move sources: live records whose path was NOT observed this pass. A record whose
        // path *is* observed can never be a move source for a *different* observed path -- it is
        // either unchanged or itself being re-probed as Modified at the same path.
        let mut missing_by_sketch: BTreeMap<ContentSketch, PathBuf> = self
            .by_path
            .iter()
            .filter(|(p, r)| !r.tombstoned && !observed_set.contains(p))
            .filter_map(|(p, r)| r.probe.sketch.map(|s| (s, p.clone())))
            .collect();
        let mut claimed_by_move: BTreeSet<PathBuf> = BTreeSet::new();

        let mut fresh: BTreeMap<PathBuf, IndexRecord> = BTreeMap::new();

        for path in observed {
            let fp = match fs_fingerprint(path) {
                Ok(fp) => fp,
                // Vanished between the caller's walk and this call -- not this pass's problem to
                // solve; if it is genuinely gone, the "missing" sweep below tombstones it once it
                // stops appearing in `observed` on a later pass.
                Err(_) => continue,
            };

            let known_here = self.by_path.get(path).filter(|r| !r.tombstoned);
            if let Some(known) = known_here {
                if known.fingerprint.size == fp.size && known.fingerprint.mtime_ns == fp.mtime_ns {
                    report.unchanged += 1;
                    fresh.insert(path.clone(), known.clone());
                    continue;
                }
            }

            match probe(path, &fp) {
                None => {
                    report.failed += 1;
                    // A hard probe failure keeps whatever was already known, so a transient read
                    // error never erases history; a path with nothing known simply is not carried
                    // into `fresh` and gets tried again next pass.
                    if let Some(known) = known_here {
                        fresh.insert(path.clone(), known.clone());
                    }
                }
                Some(result) => {
                    if result.needs_review.is_some() {
                        report.failed += 1;
                    }
                    let current =
                        FileIdentity { fs: fp, sketch: result.sketch.unwrap_or(ContentSketch(0)) };
                    let known_identity = known_here.map(|r| FileIdentity {
                        fs: r.fingerprint,
                        sketch: r.probe.sketch.unwrap_or(ContentSketch(0)),
                    });
                    let verdict = classify(&current, known_identity.as_ref(), |sketch| {
                        missing_by_sketch.contains_key(&sketch)
                    });

                    // What verification history (if any) survives into the fresh record. A move or a
                    // same-content "unchanged" both mean the bytes provably have not changed -- see
                    // the two verdict arms below for why each one is provably safe to carry forward --
                    // so a previously-established digest (and any unresolved-mismatch flag) is still
                    // valid, and re-verifying from scratch would be wasted I/O. `New`/`Modified` mean
                    // the opposite: this content has never been confirmed, so verification -- and any
                    // suspicion carried over from whatever used to be at this path -- starts clean.
                    let mut carried: Option<(Option<Verified>, bool)> = None;

                    match verdict {
                        ScanVerdict::Unchanged => {
                            // In practice unreachable today: `classify`'s own `Unchanged` condition
                            // (`lumen_identity::classify`) compares exactly the same two fields --
                            // `fs.size` and `fs.mtime_ns` -- the fast-path check above (lines ~232-
                            // 237) already tested and only fell through here because it did *not*
                            // match. Kept rather than collapsed into `Modified` in case `classify`'s
                            // own definition of "unchanged" ever grows more tolerant than an exact
                            // fingerprint match (a copy that preserves size but not mtime to full
                            // nanosecond precision would be the natural next case) -- carrying
                            // forward the previous verification is the provably-correct behavior the
                            // moment that becomes reachable, and there is no honest way to test for
                            // "unreachable" other than leaving the arm dead until it is not.
                            report.unchanged += 1;
                            carried = known_here.map(|r| (r.last_verified, r.mismatch_pending));
                        }
                        ScanVerdict::Modified => report.modified += 1,
                        ScanVerdict::New => report.new += 1,
                        ScanVerdict::Moved { from_sketch } => {
                            report.moved += 1;
                            if let Some(old_path) = missing_by_sketch.remove(&from_sketch) {
                                carried = self
                                    .by_path
                                    .get(&old_path)
                                    .map(|r| (r.last_verified, r.mismatch_pending));
                                claimed_by_move.insert(old_path);
                            }
                        }
                    }
                    let (last_verified, mismatch_pending) = carried.unwrap_or((None, false));
                    fresh.insert(
                        path.clone(),
                        IndexRecord {
                            path: path.clone(),
                            fingerprint: fp,
                            probe: result,
                            tombstoned: false,
                            last_verified,
                            mismatch_pending,
                        },
                    );
                }
            }
        }

        // Tombstone every previously-live record whose path was not observed this pass and was not
        // just claimed as the source of a move to somewhere else in `observed`.
        for (path, record) in &self.by_path {
            if record.tombstoned || observed_set.contains(path) || claimed_by_move.contains(path) {
                continue;
            }
            let mut tombstone = record.clone();
            tombstone.tombstoned = true;
            fresh.insert(path.clone(), tombstone);
            report.tombstoned += 1;
        }

        self.by_path = fresh;
        if report.changed() {
            self.version += 1;
        }
        Ok(report)
    }

    /// Re-verify live records against their last confirmed digest, tier-prioritised and
    /// budget-bounded -- `docs/15` §B.
    ///
    /// Selection is a strict four-tier priority order, most urgent first, not a flat
    /// oldest-verified-first queue:
    ///
    /// 1. **Unresolved mismatch.** A previous pass already found this record's bytes did not match
    ///    what was last confirmed, and nothing since has cleared that (a later match, or `reindex`
    ///    seeing the file legitimately change). The single most actionable thing this engine can
    ///    report is a known problem nobody has looked at yet, so these are always selected first,
    ///    every pass, regardless of the staleness interval below.
    /// 2. **Never verified.** Unknown integrity status -- this file could already be silently
    ///    corrupt and nothing would know. Always selected, same as tier 1.
    /// 3. **Flagged at probe time.** `needs_review` was set when the file was last (re-)indexed
    ///    (an extension mismatch, a truncated-looking size, ...) -- an unrelated kind of suspicion,
    ///    but still a reason to check this file's bytes sooner than one nothing has ever flagged.
    ///    Selected once due, same interval rule as tier 4.
    /// 4. **Routine.** Due for its regular re-check. Ordered oldest-confirmed-first, and the
    ///    interval itself is risk-adjusted by file size (see [`risk_adjusted_interval_secs`]) -- a
    ///    50 GB remux comes due for re-verification sooner than a 200 MB episode on the same base
    ///    interval, because it has more bytes exposed to the same bit-rot risk over the same time.
    ///
    /// `now_unix_secs` is caller-supplied rather than read from the clock in here, so a pass is a
    /// pure function of its inputs and testable without real time elapsing. `budget_bytes` bounds
    /// cumulative file size processed this call -- except the first eligible entry always runs
    /// regardless of its own size, so a library with one enormous file still makes progress instead
    /// of never being touched. `digest_of` does the actual, expensive, whole-file read; lumen-index
    /// does not touch a filesystem here any more than it does in [`Index::reindex`]. Its `Err` means
    /// the path could not be read at all this pass (never guessed at as a mismatch, which is a
    /// distinct and stronger claim) -- and costs nothing against the budget, since nothing was
    /// actually read; a permission-denied file must not silently starve a readable one right behind
    /// it in the same tier.
    ///
    /// Budget consumption is a strict prefix of the sorted list, not best-fit bin-packing: the
    /// moment one entry (after the always-exempt first) does not fit, every entry behind it is
    /// skipped too, even ones that would themselves fit within what remains. Continuing past a
    /// skipped entry to find a smaller one further down the list would let a lower-priority tier run
    /// ahead of a higher-priority entry this same pass just skipped for being too large -- silently
    /// reordering the tiers `priority` above exists to establish.
    ///
    /// A mismatch is **never** silently accepted as the new normal: the record's `last_verified`
    /// stays at the previous confirmed-good value (see [`VerifyReport::mismatched`]'s own doc), and
    /// `mismatch_pending` is set so tier 1 picks the record straight back up on the very next pass,
    /// for as long as the problem remains unresolved.
    pub fn verify(
        &mut self,
        now_unix_secs: u64,
        reverify_after_secs: u64,
        budget_bytes: u64,
        mut digest_of: impl FnMut(&Path) -> Result<FileDigest, String>,
    ) -> VerifyReport {
        let mut report = VerifyReport::default();

        let mut due: Vec<(PathBuf, Priority, Option<u64>, u64)> = self
            .by_path
            .values()
            .filter(|r| !r.tombstoned)
            .filter_map(|r| {
                priority(r, now_unix_secs, reverify_after_secs).map(|p| {
                    (r.path.clone(), p, r.last_verified.map(|v| v.at_unix_secs), r.fingerprint.size)
                })
            })
            .collect();
        due.sort_by_key(|(_, p, last, _)| (*p, *last));

        let mut bytes_budgeted = 0u64;
        // A dedicated flag for "has anything run yet", not a proxy on `bytes_budgeted > 0` -- a
        // proxy is wrong the moment the first entry (or a run of them) is legitimately zero bytes,
        // which would otherwise keep granting the "always exempt" pass to every entry after it too.
        let mut attempted_any = false;
        let mut i = 0;
        while i < due.len() {
            let (path, _, _, size) = due[i].clone();
            if attempted_any && bytes_budgeted.saturating_add(size) > budget_bytes {
                break;
            }
            attempted_any = true;

            match digest_of(&path) {
                Err(e) => report.read_failed.push((path, e)),
                Ok(digest) => {
                    bytes_budgeted = bytes_budgeted.saturating_add(size);
                    report.bytes_read = report.bytes_read.saturating_add(size);
                    let record = self.by_path.get_mut(&path).expect("path came from self.by_path");
                    match record.last_verified {
                        None => {
                            record.last_verified =
                                Some(Verified { digest, at_unix_secs: now_unix_secs });
                            record.mismatch_pending = false;
                            report.baseline_established += 1;
                        }
                        Some(prev) if prev.digest == digest => {
                            record.last_verified =
                                Some(Verified { digest, at_unix_secs: now_unix_secs });
                            record.mismatch_pending = false;
                            report.confirmed += 1;
                        }
                        Some(prev) => {
                            record.mismatch_pending = true;
                            report.mismatched.push((path, prev.at_unix_secs));
                        }
                    }
                }
            }
            i += 1;
        }
        report.skipped_by_budget += due.len() - i;
        report
    }
}

/// Selection tier for [`Index::verify`] -- derive order is the priority order (earlier variant =
/// more urgent), so sorting a list of these ascending puts the most urgent entries first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    UnresolvedMismatch,
    NeverVerified,
    FlaggedAtProbeTime,
    Routine,
}

/// A larger file carries more bit-rot exposure for the same elapsed time than a small one, so it
/// comes due for re-verification sooner on the same base interval -- halved per doubling in size
/// above a 4 GiB baseline, floored at a quarter of the base interval so one enormous file can never
/// demand re-checking every few minutes and dominate every pass's budget indefinitely.
fn risk_adjusted_interval_secs(base_secs: u64, size: u64) -> u64 {
    const BASELINE: u64 = 4 * 1024 * 1024 * 1024;
    if size <= BASELINE || base_secs == 0 {
        return base_secs;
    }
    let doublings = (size / BASELINE).ilog2().min(2);
    base_secs >> doublings
}

/// The tier a record belongs in for this pass, or `None` if it is not due at all. `None` for tiers
/// 1 and 2 (unresolved-mismatch, never-verified) is never returned -- both are always due, by
/// definition: an unresolved problem and an unknown integrity status do not become less urgent by
/// waiting.
fn priority(record: &IndexRecord, now_unix_secs: u64, base_reverify_secs: u64) -> Option<Priority> {
    if record.mismatch_pending {
        return Some(Priority::UnresolvedMismatch);
    }
    let Some(verified) = record.last_verified else {
        return Some(Priority::NeverVerified);
    };
    let interval = risk_adjusted_interval_secs(base_reverify_secs, record.fingerprint.size);
    if now_unix_secs.saturating_sub(verified.at_unix_secs) < interval {
        return None;
    }
    if record.probe.needs_review.is_some() {
        Some(Priority::FlaggedAtProbeTime)
    } else {
        Some(Priority::Routine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, title: &str, sketch: u128) -> ProbeResult {
        let _ = path;
        ProbeResult {
            title: title.into(),
            year: None,
            sketch: Some(ContentSketch(sketch)),
            needs_review: None,
        }
    }

    fn write_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lumen-index-store-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_brand_new_file_is_probed_and_recorded_as_new() {
        let dir = tmp_dir("new");
        let path = write_file(&dir, "a.mkv", b"movie bytes");

        let mut idx = Index::default();
        let report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "A Movie", 1))
            })
            .unwrap();

        assert_eq!(report.new, 1);
        assert_eq!(report.unchanged, 0);
        assert!(report.changed());
        assert_eq!(idx.get(&path).unwrap().probe.title, "A Movie");
        assert_eq!(idx.version(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn re_reindexing_an_untouched_library_probes_nothing() {
        let dir = tmp_dir("stable");
        let path = write_file(&dir, "a.mkv", b"movie bytes");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "A Movie", 1))
        })
        .unwrap();
        let version_after_first = idx.version();

        let mut probe_calls = 0usize;
        let report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                probe_calls += 1;
                Some(rec(p.to_str().unwrap(), "A Movie", 1))
            })
            .unwrap();

        assert_eq!(probe_calls, 0, "an unchanged file must never be re-probed");
        assert_eq!(report.unchanged, 1);
        assert!(!report.changed());
        assert_eq!(
            idx.version(),
            version_after_first,
            "version must not bump when nothing changed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn editing_a_file_triggers_a_reprobe_and_is_reported_as_modified() {
        let dir = tmp_dir("modify");
        let path = write_file(&dir, "a.mkv", b"original");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "Original Title", 1))
        })
        .unwrap();

        // Ensure the fingerprint actually differs even on filesystems with 1s mtime resolution.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"a rather different, longer set of bytes").unwrap();

        let report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "Updated Title", 2))
            })
            .unwrap();

        assert_eq!(report.modified, 1);
        assert_eq!(idx.get(&path).unwrap().probe.title, "Updated Title");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_full_reindex_verify_modify_reindex_verify_cycle_never_confuses_a_real_reencode_with_corruption()
     {
        // The end-to-end contract `reindex` and `verify` are jointly supposed to keep: a file that
        // legitimately changes (a re-encode, an upgraded remux) must reindex as `Modified` and start
        // verification over clean -- never surface as a "mismatch" against bytes that were never a
        // fair comparison to begin with. A regression here would either (a) falsely flag every
        // intentional re-encode as if the media were failing, or (b) the opposite failure this
        // engine exists to prevent: silently accepting a genuinely corrupted new version as an
        // unquestioned fresh baseline. Both are exactly the class of bug a green unit-level test
        // suite for `verify` and `reindex` in isolation would not, on its own, catch.
        let dir = tmp_dir("full-cycle");
        let path = write_file(&dir, "a.mkv", b"the original encode's bytes");
        let digest_of = |p: &Path| -> Result<FileDigest, String> {
            let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
            lumen_identity::digest_reader(&mut f).map_err(|e| e.to_string())
        };

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "A Movie", 1))
        })
        .unwrap();
        let first_verify = idx.verify(1_000, 24 * 60 * 60, u64::MAX, digest_of);
        assert_eq!(first_verify.baseline_established, 1);
        assert!(!first_verify.found_a_problem());

        // A genuine re-encode: different bytes, at the same path, discovered by a fresh reindex --
        // not by `verify` stumbling on it, which is the failure this whole engine exists to catch.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"a completely different, legitimately re-encoded set of bytes")
            .unwrap();
        let reindex_report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "A Movie (Remastered)", 2))
            })
            .unwrap();
        assert_eq!(reindex_report.modified, 1);
        assert!(
            idx.get(&path).unwrap().last_verified.is_none(),
            "a genuinely modified file must start verification over clean, not compare new bytes \
             against an old baseline that was never a fair comparison"
        );

        let second_verify = idx.verify(2_000, 24 * 60 * 60, u64::MAX, digest_of);
        assert_eq!(
            second_verify.baseline_established, 1,
            "the re-encode establishes a fresh baseline, exactly as a first-ever verify would"
        );
        assert!(
            !second_verify.found_a_problem(),
            "a legitimate re-encode must never be reported as if the media were failing: {:?}",
            second_verify
        );

        // Now corrupt the file *without* telling reindex -- the scenario `verify` alone exists to
        // catch, distinct from the modify-then-reindex case above.
        std::fs::write(&path, b"corrupted after the fact, reindex never saw this change").unwrap();
        let corruption_report = idx.verify(2_000 + 24 * 60 * 60, 24 * 60 * 60, u64::MAX, digest_of);
        assert_eq!(corruption_report.mismatched.len(), 1, "silent corruption must be caught");
        assert!(corruption_report.found_a_problem());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_renamed_file_with_identical_content_is_recognized_as_moved_not_delete_and_create() {
        let dir = tmp_dir("move");
        let old_path = write_file(&dir, "old-name.mkv", b"identical bytes, new home");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&old_path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "A Movie", 42))
        })
        .unwrap();

        std::fs::rename(&old_path, dir.join("new-name.mkv")).unwrap();
        let new_path = dir.join("new-name.mkv");

        let report = idx
            .reindex(std::slice::from_ref(&new_path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "A Movie", 42))
            })
            .unwrap();

        assert_eq!(report.moved, 1, "same content sketch at a new path must be Moved, not New");
        assert_eq!(report.new, 0);
        assert_eq!(report.tombstoned, 0, "the vacated path is a move source, not a real loss");
        assert!(idx.get(&old_path).is_none(), "the old path is no longer live");
        assert!(idx.get(&new_path).is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_moved_files_verification_history_survives_the_move() {
        // The end-to-end regression for the carry-forward fix `reindex`'s `Moved` arm applies: real
        // verification history, established against real bytes on disk, must not reset to "never
        // verified" just because the same file moved. A real `Index::verify` against the actual
        // written file, not a hand-built `Verified` value, is what makes this a genuine regression
        // test rather than one that could pass even if the plumbing between `verify` and `reindex`
        // were subtly wrong.
        let dir = tmp_dir("move-carries-verification");
        let old_path = write_file(&dir, "old-name.mkv", b"identical bytes, new home");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&old_path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "A Movie", 42))
        })
        .unwrap();

        let digest_of = |p: &Path| -> Result<FileDigest, String> {
            let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
            lumen_identity::digest_reader(&mut f).map_err(|e| e.to_string())
        };
        let verify_report = idx.verify(1_000, 24 * 60 * 60, u64::MAX, digest_of);
        assert_eq!(verify_report.baseline_established, 1);
        let established = idx.get(&old_path).unwrap().last_verified;
        assert!(established.is_some(), "the setup itself must have established real history");

        std::fs::rename(&old_path, dir.join("new-name.mkv")).unwrap();
        let new_path = dir.join("new-name.mkv");

        let reindex_report = idx
            .reindex(std::slice::from_ref(&new_path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "A Movie", 42))
            })
            .unwrap();
        assert_eq!(reindex_report.moved, 1);

        let after_move = idx.get(&new_path).unwrap();
        assert_eq!(
            after_move.last_verified, established,
            "the same bytes moving to a new path must not lose their confirmed-good history"
        );
        assert!(!after_move.mismatch_pending);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_disappears_is_tombstoned_not_deleted() {
        let dir = tmp_dir("vanish");
        let path = write_file(&dir, "a.mkv", b"here today");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "A Movie", 7))
        })
        .unwrap();
        std::fs::remove_file(&path).unwrap();

        let report =
            idx.reindex(&[], |_p, _fp| unreachable!("nothing observed, nothing to probe")).unwrap();

        assert_eq!(report.tombstoned, 1);
        assert!(idx.get(&path).is_none(), "a tombstoned record is not part of the live listing");
        assert!(
            idx.all_including_tombstoned().any(|r| r.path == path && r.tombstoned),
            "but its history is kept, not erased"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_reappearing_at_its_old_path_is_a_fresh_discovery_not_a_silent_revival() {
        let dir = tmp_dir("revive");
        let path = write_file(&dir, "a.mkv", b"first version");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "Old Title", 1))
        })
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        idx.reindex(&[], |_p, _fp| unreachable!()).unwrap();
        assert!(idx.get(&path).is_none());

        std::fs::write(&path, b"a completely different file that happens to share a name").unwrap();
        let report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                Some(rec(p.to_str().unwrap(), "New Title", 99))
            })
            .unwrap();

        assert_eq!(
            report.new, 1,
            "must be probed and recorded fresh, never assumed to be the old file"
        );
        assert_eq!(idx.get(&path).unwrap().probe.title, "New Title");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_probe_that_returns_none_keeps_the_prior_record_and_is_retried_next_time() {
        let dir = tmp_dir("failprobe");
        let path = write_file(&dir, "a.mkv", b"original");

        let mut idx = Index::default();
        idx.reindex(std::slice::from_ref(&path), |p, _fp| {
            Some(rec(p.to_str().unwrap(), "Known Good", 1))
        })
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, b"changed while the disk was about to misbehave").unwrap();

        let report = idx.reindex(std::slice::from_ref(&path), |_p, _fp| None).unwrap();

        assert_eq!(report.failed, 1);
        assert_eq!(
            idx.get(&path).unwrap().probe.title,
            "Known Good",
            "a hard probe failure must not erase the last known-good record"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_probe_reporting_needs_review_stays_indexed_rather_than_being_dropped() {
        let dir = tmp_dir("review");
        let path = write_file(&dir, "a.mkv", b"partially readable");

        let mut idx = Index::default();
        let report = idx
            .reindex(std::slice::from_ref(&path), |p, _fp| {
                Some(ProbeResult {
                    title: p.to_string_lossy().into_owned(),
                    year: None,
                    sketch: None,
                    needs_review: Some("probe timed out after 2 chunks".into()),
                })
            })
            .unwrap();

        assert_eq!(report.failed, 1);
        assert_eq!(report.new, 1, "still counted as discovered, not silently skipped");
        assert!(idx.get(&path).is_some());
        assert!(idx.get(&path).unwrap().probe.needs_review.is_some());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bundle_actually_calls_lumen_meta_merge_fragments() {
        let record = IndexRecord {
            path: PathBuf::from("/lib/a.mkv"),
            fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 0, mtime_ns: 0 },
            probe: ProbeResult {
                title: "A Movie".into(),
                year: Some(2020),
                sketch: None,
                needs_review: None,
            },
            tombstoned: false,
            last_verified: None,
            mismatch_pending: false,
        };
        let bundle = record.bundle();
        assert_eq!(bundle.value(FieldGroup::Titles, "title"), Some("A Movie"));
        assert_eq!(bundle.value(FieldGroup::ReleaseDates, "year"), Some("2020"));
    }

    // ---------------------------------------------------------------------------------------
    // Index::verify -- tier/priority/risk-based selection (docs/15 §B)
    // ---------------------------------------------------------------------------------------

    mod verify_tests {
        use super::*;

        const DAY: u64 = 24 * 60 * 60;

        fn seeded_index(paths_and_sizes: &[(&str, u64)]) -> Index {
            let mut idx = Index::default();
            for (p, size) in paths_and_sizes {
                idx.insert_loaded(IndexRecord {
                    path: PathBuf::from(p),
                    fingerprint: FsFingerprint { device_id: 0, inode: 0, size: *size, mtime_ns: 0 },
                    probe: ProbeResult {
                        title: (*p).into(),
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

        fn digest(n: u128) -> FileDigest {
            FileDigest(n)
        }

        #[test]
        fn a_never_verified_record_establishes_a_baseline_not_a_confirmation() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            let report = idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));

            assert_eq!(report.baseline_established, 1);
            assert_eq!(report.confirmed, 0);
            assert!(!report.found_a_problem());
            let rec = idx.get(Path::new("/a.mkv")).unwrap();
            assert_eq!(
                rec.last_verified,
                Some(Verified { digest: digest(1), at_unix_secs: 1_000 })
            );
            assert!(!rec.mismatch_pending);
        }

        #[test]
        fn a_matching_re_verify_is_confirmed_not_a_second_baseline() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));
            // Well past the interval, so the second call is actually due.
            let report = idx.verify(1_000 + 2 * DAY, DAY, u64::MAX, |_| Ok(digest(1)));

            assert_eq!(report.confirmed, 1);
            assert_eq!(report.baseline_established, 0);
        }

        #[test]
        fn a_mismatch_is_flagged_and_never_silently_folded_into_the_baseline() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));
            let report = idx.verify(1_000 + 2 * DAY, DAY, u64::MAX, |_| Ok(digest(2)));

            assert_eq!(report.mismatched, vec![(PathBuf::from("/a.mkv"), 1_000)]);
            assert!(report.found_a_problem());
            let rec = idx.get(Path::new("/a.mkv")).unwrap();
            assert_eq!(
                rec.last_verified,
                Some(Verified { digest: digest(1), at_unix_secs: 1_000 }),
                "the stored record must still say the OLD digest was last confirmed good, not silently \
                 accept the mismatching one"
            );
            assert!(rec.mismatch_pending);
        }

        #[test]
        fn an_unresolved_mismatch_is_selected_again_even_before_its_normal_interval_would_be_due() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));
            idx.verify(1_000 + 2 * DAY, DAY, u64::MAX, |_| Ok(digest(2))); // flags a mismatch

            // Only one second later -- nowhere near due on the normal interval -- but tier 1 (an
            // unresolved mismatch) is selected unconditionally.
            let report = idx.verify(1_000 + 2 * DAY + 1, DAY, u64::MAX, |_| Ok(digest(2)));
            assert_eq!(report.mismatched.len(), 1, "still unresolved, selected again immediately");
        }

        #[test]
        fn a_later_match_clears_a_previously_flagged_mismatch() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));
            idx.verify(1_000 + 2 * DAY, DAY, u64::MAX, |_| Ok(digest(2))); // mismatch
            assert!(idx.get(Path::new("/a.mkv")).unwrap().mismatch_pending);

            // The bytes are back to matching what was last confirmed good (a transient issue that
            // resolved itself, or a filesystem hiccup) -- must clear the flag, not stay stuck flagged.
            idx.verify(1_000 + 2 * DAY + 1, DAY, u64::MAX, |_| Ok(digest(1)));
            assert!(!idx.get(Path::new("/a.mkv")).unwrap().mismatch_pending);
        }

        #[test]
        fn tier_order_is_mismatch_then_never_verified_then_flagged_then_routine() {
            let mut idx = Index::default();
            idx.insert_loaded(IndexRecord {
                path: PathBuf::from("/routine.mkv"),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 100, mtime_ns: 0 },
                probe: ProbeResult {
                    title: "r".into(),
                    year: None,
                    sketch: None,
                    needs_review: None,
                },
                tombstoned: false,
                last_verified: Some(Verified { digest: digest(9), at_unix_secs: 0 }),
                mismatch_pending: false,
            });
            idx.insert_loaded(IndexRecord {
                path: PathBuf::from("/flagged.mkv"),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 100, mtime_ns: 0 },
                probe: ProbeResult {
                    title: "f".into(),
                    year: None,
                    sketch: None,
                    needs_review: Some("extension mismatch".into()),
                },
                tombstoned: false,
                last_verified: Some(Verified { digest: digest(9), at_unix_secs: 0 }),
                mismatch_pending: false,
            });
            idx.insert_loaded(IndexRecord {
                path: PathBuf::from("/never.mkv"),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 100, mtime_ns: 0 },
                probe: ProbeResult {
                    title: "n".into(),
                    year: None,
                    sketch: None,
                    needs_review: None,
                },
                tombstoned: false,
                last_verified: None,
                mismatch_pending: false,
            });
            idx.insert_loaded(IndexRecord {
                path: PathBuf::from("/mismatched.mkv"),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 100, mtime_ns: 0 },
                probe: ProbeResult {
                    title: "m".into(),
                    year: None,
                    sketch: None,
                    needs_review: None,
                },
                tombstoned: false,
                last_verified: Some(Verified { digest: digest(9), at_unix_secs: 0 }),
                mismatch_pending: true,
            });

            // Budget for exactly one 100-byte file per call -- forces the selection to prove its
            // order one entry at a time rather than just "everything eligible, in some order".
            // `skipped_by_budget` is deliberately not asserted here: every still-due entry this
            // pass didn't reach counts toward it every time, by design (see the dedicated budget
            // tests below) -- what this test checks is the *order* entries get reached in, across
            // calls, as higher tiers stop being due once they're processed.
            let mut order = Vec::new();
            for _ in 0..4 {
                idx.verify(10 * DAY, DAY, 100, |p| {
                    order.push(p.to_path_buf());
                    Ok(digest(9))
                });
            }

            assert_eq!(
                order,
                vec![
                    PathBuf::from("/mismatched.mkv"),
                    PathBuf::from("/never.mkv"),
                    PathBuf::from("/flagged.mkv"),
                    PathBuf::from("/routine.mkv"),
                ]
            );
        }

        #[test]
        fn not_due_yet_is_not_selected_at_all() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));

            let mut calls = 0;
            let report = idx.verify(1_000 + 1, DAY, u64::MAX, |_p| {
                calls += 1;
                Ok(digest(9))
            });
            let _ = report;
            assert_eq!(calls, 0, "confirmed one second ago, nowhere near a day-long interval");
        }

        #[test]
        fn a_larger_file_becomes_due_sooner_than_a_small_one_on_the_same_base_interval() {
            let mut idx =
                seeded_index(&[("/small.mkv", 1024), ("/huge.mkv", 32 * 1024 * 1024 * 1024)]);
            idx.verify(0, DAY, u64::MAX, |_| Ok(digest(1)));

            // A quarter of the base interval later: the risk-adjusted interval for a 32 GiB file
            // (3 doublings above the 4 GiB baseline, capped at a quarter) is due; a 1 KiB file's
            // plain day-long interval is not.
            let mut seen = Vec::new();
            idx.verify(DAY / 4, DAY, u64::MAX, |p| {
                seen.push(p.to_path_buf());
                Ok(digest(1))
            });

            assert_eq!(seen, vec![PathBuf::from("/huge.mkv")]);
        }

        #[test]
        fn read_failure_is_reported_and_never_guessed_at_as_a_mismatch_or_a_confirmation() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            let report = idx.verify(1_000, DAY, u64::MAX, |_| Err("permission denied".to_string()));

            assert_eq!(
                report.read_failed,
                vec![(PathBuf::from("/a.mkv"), "permission denied".into())]
            );
            assert_eq!(report.confirmed, 0);
            assert_eq!(report.baseline_established, 0);
            assert!(report.mismatched.is_empty());
            assert!(idx.get(Path::new("/a.mkv")).unwrap().last_verified.is_none());
        }

        #[test]
        fn the_first_eligible_entry_always_runs_even_if_it_alone_exceeds_the_budget() {
            let mut idx = seeded_index(&[("/huge.mkv", 999_999_999)]);
            let report = idx.verify(1_000, DAY, 10, |_| Ok(digest(1)));

            assert_eq!(
                report.baseline_established, 1,
                "a budget of 10 bytes must not starve the only file"
            );
            assert_eq!(report.skipped_by_budget, 0);
        }

        #[test]
        fn verifying_an_empty_index_is_trivially_a_no_op() {
            let mut idx = Index::default();
            let report = idx.verify(1_000, DAY, u64::MAX, |_| Ok(digest(1)));
            assert_eq!(report, VerifyReport::default());
        }

        #[test]
        fn a_budget_that_only_fits_the_first_of_several_stops_there() {
            let mut idx = seeded_index(&[("/a.mkv", 100), ("/b.mkv", 100), ("/c.mkv", 100)]);
            let report = idx.verify(1_000, DAY, 150, |_| Ok(digest(1)));

            assert_eq!(report.baseline_established, 1);
            assert_eq!(report.skipped_by_budget, 2);
        }

        #[test]
        fn repeated_bounded_passes_eventually_cover_every_entry_exactly_once() {
            let entries: Vec<(&str, u64)> =
                vec![("/a.mkv", 100), ("/b.mkv", 100), ("/c.mkv", 100), ("/d.mkv", 100)];
            let mut idx = seeded_index(&entries);

            let mut covered = std::collections::BTreeSet::new();
            for i in 0..entries.len() {
                let report = idx.verify(1_000 + i as u64, DAY, 100, |p| {
                    covered.insert(p.to_path_buf());
                    Ok(digest(1))
                });
                assert_eq!(
                    report.baseline_established, 1,
                    "each bounded pass makes exactly one entry of progress"
                );
            }

            assert_eq!(
                covered.len(),
                entries.len(),
                "no entry starved across repeated bounded passes"
            );
        }

        #[test]
        fn a_tombstoned_record_is_never_selected() {
            let mut idx = seeded_index(&[("/a.mkv", 100)]);
            {
                let rec = idx.by_path.get_mut(Path::new("/a.mkv")).unwrap();
                rec.tombstoned = true;
            }
            let mut calls = 0;
            idx.verify(1_000, DAY, u64::MAX, |_| {
                calls += 1;
                Ok(digest(1))
            });
            assert_eq!(calls, 0);
        }

        #[test]
        fn a_leading_zero_size_entry_does_not_grant_every_entry_behind_it_an_unbounded_pass() {
            // Regression: the "first entry always runs" exemption used to be approximated by
            // `bytes_budgeted > 0`, which stayed false -- and so kept exempting every subsequent
            // entry too -- for as long as every entry processed so far happened to be zero bytes.
            let mut idx = seeded_index(&[("/a-empty.mkv", 0), ("/z-huge.mkv", 999_999_999)]);
            let report = idx.verify(1_000, DAY, 10, |_| Ok(digest(1)));

            assert_eq!(
                report.baseline_established, 1,
                "only the true first entry (zero bytes) should run against a 10-byte budget"
            );
            assert_eq!(report.skipped_by_budget, 1, "the huge entry must not sneak in behind it");
        }

        #[test]
        fn a_failed_read_does_not_consume_the_budget_a_readable_file_behind_it_needs() {
            // Regression: bytes_budgeted/report.bytes_read used to be charged the entry's nominal
            // size *before* attempting the read, so a run of unreadable files could exhaust an
            // entire pass's budget without a single real byte read.
            let mut idx = seeded_index(&[("/a-unreadable.mkv", 999_999_999), ("/b-real.mkv", 100)]);
            let report = idx.verify(1_000, DAY, 200, |p| {
                if p == Path::new("/a-unreadable.mkv") {
                    Err("permission denied".into())
                } else {
                    Ok(digest(1))
                }
            });

            assert_eq!(report.read_failed.len(), 1);
            assert_eq!(
                report.baseline_established, 1,
                "the second file must still be reached -- the failed read cost it nothing"
            );
            assert_eq!(report.bytes_read, 100, "only the bytes actually read are counted");
            assert_eq!(report.skipped_by_budget, 0);
        }

        #[test]
        fn budget_accumulation_never_overflows_even_with_near_u64_max_sizes() {
            // Regression: `bytes_budgeted + size` and `bytes_budgeted += size` could overflow (panic
            // in a debug build, silently wrap and bypass the budget in release) for large enough
            // sizes. Saturating arithmetic must hold regardless.
            let mut idx = seeded_index(&[("/a.mkv", u64::MAX), ("/b.mkv", u64::MAX)]);
            // A finite, realistic-shaped budget rather than `u64::MAX` itself: with the budget also
            // at the saturation ceiling, `saturating_add` making the true (unsaturated) sum look
            // exactly equal to an equally-saturated budget would falsely read as "still fits" --
            // not a wraparound bug, but not what this test means to exercise either.
            let report = idx.verify(1_000, DAY, 1_000_000_000, |_| Ok(digest(1)));

            // The first entry is always exempt and runs; the second cannot possibly fit behind a
            // budget already saturated by the first, and must be skipped, not panic or wrap around
            // into fitting.
            assert_eq!(report.baseline_established, 1);
            assert_eq!(report.skipped_by_budget, 1);
        }

        #[test]
        fn a_budget_skip_never_lets_a_lower_tier_entry_run_ahead_of_a_skipped_higher_tier_one() {
            // Regression: the old loop used `continue` on a budget miss, so it kept scanning for a
            // later entry that *did* fit -- letting a small, low-priority entry run in the same pass
            // a large, higher-priority entry was skipped from. A bounded pass must consume a strict
            // prefix of the priority-sorted list, never skip over a miss to reach a smaller one.
            let mut idx = Index::default();
            for (path, size, needs_review) in
                [("/never-a.mkv", 900, None), ("/never-b.mkv", 900, None)]
            {
                idx.insert_loaded(IndexRecord {
                    path: PathBuf::from(path),
                    fingerprint: FsFingerprint { device_id: 0, inode: 0, size, mtime_ns: 0 },
                    probe: ProbeResult {
                        title: path.into(),
                        year: None,
                        sketch: None,
                        needs_review,
                    },
                    tombstoned: false,
                    last_verified: None,
                    mismatch_pending: false,
                });
            }
            // A same-tier (routine) small file that would fit the budget on its own, but only
            // because a same-priority large file ahead of it in path order was skipped for size.
            idx.insert_loaded(IndexRecord {
                path: PathBuf::from("/z-small-routine.mkv"),
                fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 10, mtime_ns: 0 },
                probe: ProbeResult {
                    title: "z".into(),
                    year: None,
                    sketch: None,
                    needs_review: None,
                },
                tombstoned: false,
                last_verified: Some(Verified { digest: digest(9), at_unix_secs: 0 }),
                mismatch_pending: false,
            });

            // Budget fits the first 900-byte entry alone, nothing more.
            let mut order = Vec::new();
            let report = idx.verify(10 * DAY, DAY, 950, |p| {
                order.push(p.to_path_buf());
                Ok(digest(1))
            });

            assert_eq!(order, vec![PathBuf::from("/never-a.mkv")], "must stop at the first miss");
            assert_eq!(
                report.skipped_by_budget, 2,
                "both entries behind the miss count as skipped"
            );
        }

        #[test]
        fn risk_adjusted_interval_halves_and_quarters_at_the_documented_thresholds() {
            let base = 8 * DAY; // divisible by 4, so no rounding ambiguity in the assertions below
            const GIB: u64 = 1024 * 1024 * 1024;
            assert_eq!(risk_adjusted_interval_secs(base, GIB), base, "below baseline: unchanged");
            assert_eq!(risk_adjusted_interval_secs(base, 4 * GIB), base, "at baseline: unchanged");
            assert_eq!(
                risk_adjusted_interval_secs(base, 8 * GIB),
                base / 2,
                "one doubling: halved"
            );
            assert_eq!(
                risk_adjusted_interval_secs(base, 16 * GIB),
                base / 4,
                "two doublings: quartered"
            );
            assert_eq!(
                risk_adjusted_interval_secs(base, 64 * GIB),
                base / 4,
                "capped at a quarter, never keeps halving indefinitely"
            );
        }
    }
}
