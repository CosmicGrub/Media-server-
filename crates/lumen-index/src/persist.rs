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

use lumen_identity::{ContentSketch, FsFingerprint};

use crate::store::{Index, IndexRecord, ProbeResult};

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

fn field_or_dash(s: &str) -> String {
    if s.is_empty() { "-".to_string() } else { escape(s) }
}

fn read_field(s: &str) -> String {
    if s == "-" { String::new() } else { unescape(s) }
}

fn record_to_line(r: &IndexRecord) -> String {
    let fp = r.fingerprint;
    let mut line = String::new();
    let _ = write!(
        line,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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
    );
    line
}

/// `None` for a line that cannot be parsed, rather than an error that would abort loading every
/// other record after it -- the same "one bad entry never takes down the batch" posture used
/// throughout this codebase (`lumen-probe`'s truncation property, `verify_duplicate_group`'s
/// per-file error handling). A corrupt line is dropped and rediscovered on the next reindex.
fn line_to_record(line: &str) -> Option<IndexRecord> {
    let f: Vec<&str> = line.splitn(10, '\t').collect();
    if f.len() != 10 {
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

    Some(IndexRecord {
        path,
        fingerprint: FsFingerprint { device_id, inode, size, mtime_ns },
        probe: ProbeResult { title, year, sketch, needs_review },
        tombstoned,
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

        let b = loaded.all_including_tombstoned().find(|r| r.tombstoned).unwrap();
        assert_eq!(b.path, PathBuf::from("/library/broken\tname\n.mkv"));
        assert_eq!(b.probe.needs_review.as_deref(), Some("open() failed: permission denied"));

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
