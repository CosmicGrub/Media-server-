//! The index itself, and the incremental reconciliation algorithm.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lumen_identity::{ContentSketch, FileIdentity, FsFingerprint, ScanVerdict, classify};
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

                    match verdict {
                        ScanVerdict::Unchanged => {
                            // Only reachable if the probe's own fresh sketch happens to match what
                            // was already on file despite the fingerprint differing (e.g. a copy that
                            // preserved mtime to the second but not the nanosecond) -- correct to
                            // record as unchanged rather than a spurious Modified.
                            report.unchanged += 1;
                        }
                        ScanVerdict::Modified => report.modified += 1,
                        ScanVerdict::New => report.new += 1,
                        ScanVerdict::Moved { from_sketch } => {
                            report.moved += 1;
                            if let Some(old_path) = missing_by_sketch.remove(&from_sketch) {
                                claimed_by_move.insert(old_path);
                            }
                        }
                    }
                    fresh.insert(
                        path.clone(),
                        IndexRecord {
                            path: path.clone(),
                            fingerprint: fp,
                            probe: result,
                            tombstoned: false,
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
        };
        let bundle = record.bundle();
        assert_eq!(bundle.value(FieldGroup::Titles, "title"), Some("A Movie"));
        assert_eq!(bundle.value(FieldGroup::ReleaseDates, "year"), Some("2020"));
    }
}
