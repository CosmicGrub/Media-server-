//! On-disk format: one record per line, tab-separated fields, atomic write.
//!
//! Not JSON, on purpose -- the same reasoning `lumen-play`'s `crate::json` and `TokenStore` already
//! settled on for this workspace: a hand-rolled line format costs nothing beyond `std` and stays
//! trivial to read by eye or `grep` in a text editor, which a media library's own index file is
//! exactly the kind of thing a user may reasonably want to do.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use lumen_identity::{ContentSketch, FileDigest, FsFingerprint};

use crate::store::{Index, IndexRecord, ProbeResult, Verified};

const HEADER: &str = "LUMEN-INDEX v1";

/// A tab or newline inside a field (a path or title genuinely can contain either) would otherwise
/// corrupt the line format, so both are backslash-escaped along with the escape character itself.
fn escape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            // A trailing backslash or an unrecognised escape has no correct interpretation. Keeping
            // the backslash literally is the honest choice: guessing at what the writer meant would
            // silently invent a byte that was never in the original field.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `-` alone means "this field is empty" -- reserved for exactly that, which means a real field
/// whose entire content genuinely is one literal hyphen (a file named `-`, a title reduced to one
/// hyphen by some upstream parsing step) cannot be written as a bare `-` too, or it would read back
/// as empty on the next load. `escape` never produces a bare `-` for anything except its own literal
/// input `"-"` (any string containing a real backslash comes out with that backslash doubled), so
/// `\-` is a safe, unambiguous stand-in reserved for exactly this one collision.
fn field_or_dash(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else if s == "-" {
        "\\-".to_string()
    } else {
        escape(s)
    }
}

fn read_field(s: &str) -> String {
    if s == "-" {
        String::new()
    } else if s == "\\-" {
        "-".to_string()
    } else {
        unescape(s)
    }
}

/// Field count for the original format (before `last_verified`/`mismatch_pending` existed) and for
/// the current one. [`line_to_record`] accepts either, so an index file written by an older build
/// keeps loading rather than being silently discarded the moment this format grew two columns.
const FIELDS_V1: usize = 10;
const FIELDS_CURRENT: usize = 13;

fn record_to_line(r: &IndexRecord) -> String {
    let fp = r.fingerprint;
    let mut line = String::new();
    let _ = write!(
        line,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        field_or_dash(&r.path.to_string_lossy()),
        fp.device_id,
        fp.inode,
        fp.size,
        fp.mtime_ns,
        r.probe.sketch.map_or_else(|| "-".to_string(), ContentSketch::to_hex),
        field_or_dash(&r.probe.title),
        r.probe.year.map_or_else(|| "-".to_string(), |y| y.to_string()),
        field_or_dash(r.probe.needs_review.as_deref().unwrap_or("")),
        u8::from(r.tombstoned),
        r.last_verified.map_or_else(|| "-".to_string(), |v| v.digest.to_hex()),
        r.last_verified.map_or_else(|| "-".to_string(), |v| v.at_unix_secs.to_string()),
        u8::from(r.mismatch_pending),
    );
    line
}

/// `None` for a line that cannot be parsed, rather than an error that would abort loading every
/// other record after it -- the same "one bad entry never takes down the batch" posture used
/// throughout this codebase (`lumen-probe`'s truncation property, `verify_duplicate_group`'s
/// per-file error handling). A corrupt line is dropped and rediscovered on the next reindex.
fn line_to_record(line: &str) -> Option<IndexRecord> {
    // Unbounded, not `splitn(FIELDS_CURRENT, ..)` -- every field a real writer produces has its own
    // tab characters escaped (see `escape`), so a well-formed line never contains more than 9 or 12
    // real tabs to begin with. Capping the split at `FIELDS_CURRENT` would silently fold any *extra*
    // real tabs a corrupted line happened to contain into the final field instead of rejecting it;
    // an exact length check below catches that case instead. (A line truncated to exactly 9 real
    // tabs is still indistinguishable from a genuine v1 line either way -- the two formats are
    // deliberately prefix-compatible -- but that residual case degrades safely: the record is read
    // back as "never verified", which is both the honest answer for what a truncated line actually
    // proves and self-corrects on the very next `verify` pass, the highest-priority tier after an
    // unresolved mismatch.)
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != FIELDS_V1 && f.len() != FIELDS_CURRENT {
        return None;
    }
    let path = PathBuf::from(read_field(f[0]));
    let device_id: u64 = f[1].parse().ok()?;
    let inode: u64 = f[2].parse().ok()?;
    let size: u64 = f[3].parse().ok()?;
    let mtime_ns: i128 = f[4].parse().ok()?;
    let sketch = if f[5] == "-" { None } else { ContentSketch::from_hex(f[5]) };
    let title = read_field(f[6]);
    let year = if f[7] == "-" { None } else { f[7].parse().ok() };
    let needs_review = if f[8] == "-" { None } else { Some(read_field(f[8])) };
    let tombstoned = f[9] == "1";

    // A v1 line (10 fields, predating verification entirely) has no history to reconstruct --
    // `None`/`false` is not a guess, it is the literal truth for a file nothing has ever verified.
    let (last_verified, mismatch_pending) = if f.len() == FIELDS_CURRENT {
        let verified = match (f[10], f[11]) {
            ("-", "-") => None,
            (digest_hex, secs) if digest_hex != "-" && secs != "-" => Some(Verified {
                digest: FileDigest::from_hex(digest_hex)?,
                at_unix_secs: secs.parse().ok()?,
            }),
            // One dash and one real value is not a shape this writer ever produces -- treat as
            // unverified rather than guessing which half to trust.
            _ => None,
        };
        (verified, f[12] == "1")
    } else {
        (None, false)
    };

    Some(IndexRecord {
        path,
        fingerprint: FsFingerprint { device_id, inode, size, mtime_ns },
        probe: ProbeResult { title, year, sketch, needs_review },
        tombstoned,
        last_verified,
        mismatch_pending,
    })
}

/// Load an index from disk. A missing file is an empty, version-0 index -- the first `reindex` on a
/// library with no prior state, not an error.
pub fn load(path: &Path) -> io::Result<Index> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Index::default()),
        Err(e) => return Err(e),
    };

    let mut lines = text.lines();
    let mut version = 0u64;
    if let Some(first) = lines.next() {
        if let Some(rest) = first.strip_prefix(HEADER) {
            version = rest.trim().parse().unwrap_or(0);
        } else {
            // Not our header at all -- an empty/garbage file. Treat as fresh rather than erroring;
            // the next save overwrites it with a well-formed one.
            return Ok(Index::default());
        }
    }

    let mut index = Index::default();
    for line in lines {
        if let Some(rec) = line_to_record(line) {
            index.insert_loaded(rec);
        }
    }
    index.set_version(version);
    Ok(index)
}

/// Temp-file-then-rename, the same pattern `TokenStore::persist_all` already established in
/// `lumen-play`: a crash or power loss mid-write leaves the previous, still-valid index in place
/// rather than a half-written file the next load would have to guess about.
pub fn save(path: &Path, index: &Index) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{HEADER} {}", index.version())?;
        for rec in index.all_including_tombstoned() {
            writeln!(f, "{}", record_to_line(rec))?;
        }
        f.flush()?;
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_a_field_with_every_special_character_round_trips() {
        let original = "a\\weird\ttitle\nwith\rall of them";
        assert_eq!(unescape(&escape(original)), original);
    }

    #[test]
    fn a_field_with_nothing_special_round_trips_unchanged() {
        let original = "Blade Runner 2049";
        assert_eq!(unescape(&escape(original)), original);
    }

    #[test]
    fn a_trailing_lone_backslash_is_kept_rather_than_dropped() {
        assert_eq!(unescape("literally ends in \\"), "literally ends in \\");
    }

    #[test]
    fn field_or_dash_and_read_field_round_trip_the_dash_collision_they_exist_for() {
        // `field_or_dash`'s own doc comment reasons at length about why a field whose entire content
        // is one literal hyphen cannot be written as a bare `-` -- that would read back indistinguish-
        // able from an empty field on the next load. Reasoned about in prose, but never actually
        // exercised by a test: nothing here proved the `\-` stand-in the comment describes really
        // survives a real round trip rather than, say, silently colliding with a field that happens to
        // start with an escaped hyphen for some other reason.
        for original in ["", "-", "\\-", "\\", "--", "-a", "a-", "a-b"] {
            let written = field_or_dash(original);
            let read_back = read_field(&written);
            assert_eq!(read_back, original, "{original:?} wrote as {written:?}");
        }
        // The two reserved encodings must actually mean what the doc comment says they mean, not just
        // round-trip through each other by coincidence.
        assert_eq!(field_or_dash(""), "-", "empty must use the bare dash");
        assert_eq!(
            field_or_dash("-"),
            "\\-",
            "a literal dash must not collide with empty's own bare dash"
        );
        assert_eq!(read_field("-"), "", "a bare dash on disk must read back as empty");
        assert_eq!(
            read_field("\\-"),
            "-",
            "the escaped dash on disk must read back as a literal dash"
        );
    }

    #[test]
    fn save_then_load_reproduces_every_record_field_for_field() {
        let dir = std::env::temp_dir().join(format!("lumen-index-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.idx");

        let mut index = Index::default();
        index.insert_loaded(IndexRecord {
            path: PathBuf::from("/library/Blade Runner 2049 (2017)/movie.mkv"),
            fingerprint: FsFingerprint {
                device_id: 1,
                inode: 2,
                size: 3_000_000_000,
                mtime_ns: -5,
            },
            probe: ProbeResult {
                title: "Blade Runner 2049".into(),
                year: Some(2017),
                sketch: Some(ContentSketch(0xDEAD_BEEF)),
                needs_review: None,
            },
            tombstoned: false,
            last_verified: Some(Verified {
                digest: FileDigest(0x00C0_FFEE),
                at_unix_secs: 1_700_000_000,
            }),
            mismatch_pending: true,
        });
        index.insert_loaded(IndexRecord {
            path: PathBuf::from("/library/broken\tname\n.mkv"),
            fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 0, mtime_ns: 0 },
            probe: ProbeResult {
                title: String::new(),
                year: None,
                sketch: None,
                needs_review: Some("open() failed: permission denied".into()),
            },
            tombstoned: true,
            last_verified: None,
            mismatch_pending: false,
        });
        index.set_version(7);

        save(&db, &index).unwrap();
        let loaded = load(&db).unwrap();

        assert_eq!(loaded.version(), 7);
        let a = loaded
            .all_including_tombstoned()
            .find(|r| r.probe.title == "Blade Runner 2049")
            .unwrap();
        assert_eq!(a.probe.year, Some(2017));
        assert_eq!(a.probe.sketch, Some(ContentSketch(0xDEAD_BEEF)));
        assert_eq!(a.fingerprint.mtime_ns, -5);
        assert_eq!(
            a.last_verified,
            Some(Verified { digest: FileDigest(0x00C0_FFEE), at_unix_secs: 1_700_000_000 })
        );
        assert!(a.mismatch_pending);

        let b = loaded.all_including_tombstoned().find(|r| r.tombstoned).unwrap();
        assert_eq!(b.path, PathBuf::from("/library/broken\tname\n.mkv"));
        assert_eq!(b.probe.needs_review.as_deref(), Some("open() failed: permission denied"));
        assert!(b.last_verified.is_none());
        assert!(!b.mismatch_pending);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v1_line_with_no_verification_columns_still_loads_as_never_verified() {
        let dir =
            std::env::temp_dir().join(format!("lumen-index-v1-compat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.idx");
        // Exactly what an older build (before this format grew last_verified/mismatch_pending)
        // would have written -- ten fields, no trailing columns.
        std::fs::write(
            &db,
            format!("{HEADER} 3\n/library/a.mkv\t1\t2\t500\t9\tabc123\tA Movie\t2020\t-\t0\n"),
        )
        .unwrap();

        let loaded = load(&db).unwrap();
        let rec = loaded.all_including_tombstoned().next().unwrap();
        assert_eq!(rec.probe.title, "A Movie");
        assert!(rec.last_verified.is_none(), "a v1 line predates verification entirely");
        assert!(!rec.mismatch_pending);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_title_that_is_literally_a_single_hyphen_round_trips_rather_than_becoming_empty() {
        // Regression: `-` alone means "empty field"; a real field whose content genuinely is one
        // hyphen used to be written as an indistinguishable bare `-` and read back as `""`.
        let dir =
            std::env::temp_dir().join(format!("lumen-index-hyphen-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.idx");

        let mut index = Index::default();
        index.insert_loaded(IndexRecord {
            path: PathBuf::from("/library/-.mkv"),
            fingerprint: FsFingerprint { device_id: 0, inode: 0, size: 0, mtime_ns: 0 },
            probe: ProbeResult {
                title: "-".into(),
                year: None,
                sketch: None,
                needs_review: Some("-".into()),
            },
            tombstoned: false,
            last_verified: None,
            mismatch_pending: false,
        });
        save(&db, &index).unwrap();

        let loaded = load(&db).unwrap();
        let rec = loaded.all_including_tombstoned().next().unwrap();
        assert_eq!(rec.path, PathBuf::from("/library/-.mkv"));
        assert_eq!(rec.probe.title, "-", "a literal hyphen title must not become empty");
        assert_eq!(rec.probe.needs_review.as_deref(), Some("-"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_with_more_real_tabs_than_any_valid_format_has_is_rejected_not_silently_folded() {
        // Regression: `splitn(FIELDS_CURRENT, '\t')` capped the split at 13 pieces, so a corrupted
        // line with extra real tabs past the 12th silently folded the remainder into the last field
        // instead of being recognised as malformed.
        let dir =
            std::env::temp_dir().join(format!("lumen-index-extra-tabs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.idx");
        // A well-formed 13-field line with one extra bare tab spliced into the last field.
        std::fs::write(
            &db,
            format!(
                "{HEADER} 3\n/ok/path\t1\t2\t3\t4\t-\tTitle\t-\t-\t0\t-\t-\t0\textra\n\
                 /good/path\t1\t2\t3\t4\t-\tGood\t-\t-\t0\n"
            ),
        )
        .unwrap();

        let loaded = load(&db).unwrap();
        assert_eq!(
            loaded.all_including_tombstoned().count(),
            1,
            "the malformed line must be dropped, not misread with the extra tab folded in"
        );
        assert_eq!(loaded.all_including_tombstoned().next().unwrap().probe.title, "Good");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loading_a_missing_file_is_an_empty_index_not_an_error() {
        let path = std::env::temp_dir().join("lumen-index-does-not-exist-9182.idx");
        let index = load(&path).unwrap();
        assert!(index.is_empty());
        assert_eq!(index.version(), 0);
    }

    #[test]
    fn one_corrupt_line_is_dropped_without_losing_the_rest_of_the_file() {
        let dir = std::env::temp_dir().join(format!("lumen-index-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("library.idx");
        std::fs::write(
            &db,
            format!("{HEADER} 3\nnot enough fields\n/ok/path\t1\t2\t3\t4\t-\tTitle\t-\t-\t0\n"),
        )
        .unwrap();

        let loaded = load(&db).unwrap();
        assert_eq!(loaded.all_including_tombstoned().count(), 1);
        assert_eq!(loaded.all_including_tombstoned().next().unwrap().probe.title, "Title");

        std::fs::remove_dir_all(&dir).ok();
    }
}
