//! Move-surviving file identity — `docs/05` §3 and decision **D5** in `docs/00`.
//!
//! The universal complaint about every product in this category is "I moved my files and lost all my
//! watch state." It happens because they key user data on path. This crate is the fix: a
//! content-derived sketch that survives rename, move, and remount, used as a stable secondary key
//! alongside path.
//!
//! The sketch reads a fixed ~3 MiB regardless of file size — head, middle, and tail — mixed with the
//! exact byte length. It is computed during the Probe stage, which already opens the file, so it
//! costs nothing extra in wall-clock terms.
//!
//! It is **not** a cryptographic hash and must never be used as one. It answers "is this the same
//! file I saw before, possibly at a different path?" — a question where an adversary is not trying to
//! force a collision.

#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};

use xxhash_rust::xxh3::Xxh3;

/// Bytes read from each of the three regions.
pub const CHUNK: u64 = 1024 * 1024;

/// Files at or below this size are sketched in full — the three regions would overlap anyway, and
/// reading a small file entirely is cheaper than seeking around it.
pub const FULL_READ_THRESHOLD: u64 = 3 * CHUNK;

/// Filesystem fast path. Cheap to obtain and stable while a file stays put, but every field changes
/// when it moves across filesystems, which is exactly why the sketch exists alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FsFingerprint {
    pub device_id: u64,
    /// Inode on Unix, file index on Windows, file ID on APFS.
    pub inode: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

/// Content-derived identity. Equal sketches mean "almost certainly the same bytes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentSketch(pub u128);

impl ContentSketch {
    /// Lowercase hex, for storage and logs.
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        u128::from_str_radix(s.trim(), 16).ok().map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub fs: FsFingerprint,
    pub sketch: ContentSketch,
}

/// What the scanner concluded about a path on this pass — `docs/05` §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanVerdict {
    /// Path and (size, mtime) unchanged. Roughly 99.9% of files on a rescan; skip all further work.
    Unchanged,
    /// Same path, contents changed. Re-probe.
    Modified,
    /// A new path carries a sketch we have seen before: the file moved. Update the path and keep
    /// every scrap of user data.
    Moved { from_sketch: ContentSketch },
    /// Not seen before.
    New,
}

/// Classify a path against what the library already knows.
///
/// `known_by_sketch` answers "have I seen these contents anywhere before?" and is what turns a
/// move into a path update instead of a delete-plus-insert.
pub fn classify(
    current: &FileIdentity,
    known_at_path: Option<&FileIdentity>,
    known_by_sketch: impl FnOnce(ContentSketch) -> bool,
) -> ScanVerdict {
    if let Some(known) = known_at_path {
        if known.fs.size == current.fs.size && known.fs.mtime_ns == current.fs.mtime_ns {
            return ScanVerdict::Unchanged;
        }
        return ScanVerdict::Modified;
    }
    if known_by_sketch(current.sketch) {
        return ScanVerdict::Moved { from_sketch: current.sketch };
    }
    ScanVerdict::New
}

/// Compute the sketch over an arbitrary seekable reader.
///
/// Deliberately generic so it is testable without touching a filesystem, and reusable over network
/// sources where a `File` is not available.
pub fn sketch_reader<R: Read + Seek>(reader: &mut R, size: u64) -> std::io::Result<ContentSketch> {
    let mut hasher = Xxh3::new();
    // Length is mixed in first so two files that share their sampled regions but differ in size can
    // never collide — the common case for truncated copies and for padded container rewrites.
    hasher.update(&size.to_le_bytes());

    let mut buf = vec![0u8; CHUNK as usize];

    if size <= FULL_READ_THRESHOLD {
        reader.seek(SeekFrom::Start(0))?;
        let mut remaining = size;
        while remaining > 0 {
            let want = remaining.min(CHUNK) as usize;
            let n = read_up_to(reader, &mut buf[..want])?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        return Ok(ContentSketch(hasher.digest128()));
    }

    // Head captures container headers, middle captures payload, tail captures the index/moov that
    // many muxers write last. Sampling all three makes a same-length different-content collision
    // implausible in practice.
    let offsets = [0, size / 2 - CHUNK / 2, size - CHUNK];
    for off in offsets {
        reader.seek(SeekFrom::Start(off))?;
        let n = read_up_to(reader, &mut buf)?;
        hasher.update(&buf[..n]);
    }
    Ok(ContentSketch(hasher.digest128()))
}

/// `Read::read` may return fewer bytes than requested for reasons other than EOF, especially over a
/// network transport. Looping is required for a deterministic sketch.
fn read_up_to<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Bytes read to sketch a file of `size`. Constant above the threshold — this is the property that
/// makes the sketch affordable on a 500 GB remux.
pub fn sketch_read_cost(size: u64) -> u64 {
    if size <= FULL_READ_THRESHOLD { size } else { 3 * CHUNK }
}

/// A whole-file content digest — unlike [`ContentSketch`], which samples three ~1 MiB regions plus
/// length (fast, "is this probably the same file"), this hashes every byte. It answers a stronger,
/// different question: "have these exact bytes changed since I last checked?" — the question the
/// Integrity Engine (`docs/15` §B) needs a real answer to, not an implausible-collision guess. A
/// corrupted byte anywhere, including the vast majority of a large file the sketch never reads at
/// all, changes this digest; only a byte inside one of the sketch's three sampled regions is
/// guaranteed to change that one.
///
/// Not a cryptographic hash, for the same reason `ContentSketch` is not one: this defends against
/// ordinary corruption — bit rot, a failed write, a bad sector — not against an adversary
/// constructing a collision on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileDigest(pub u128);

impl FileDigest {
    /// Lowercase hex, for storage and logs.
    pub fn to_hex(self) -> String {
        format!("{:032x}", self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        u128::from_str_radix(s.trim(), 16).ok().map(Self)
    }
}

/// Hash every byte of `reader`, start to end, in bounded chunks so this never needs the whole file
/// in memory at once. Unlike [`sketch_reader`], there is no size-based shortcut and no seeking — that
/// full read is the entire point of this function existing alongside the sampled one, so it takes a
/// plain `Read` rather than `Read + Seek`, and a size the caller does not even need to know up front.
pub fn digest_reader<R: Read>(reader: &mut R) -> std::io::Result<FileDigest> {
    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; CHUNK as usize];
    loop {
        let n = read_up_to(reader, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(FileDigest(hasher.digest128()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Deterministic pseudo-random bytes; avoids a dev-dependency and keeps failures reproducible.
    fn bytes(len: usize, seed: u64) -> Vec<u8> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect()
    }

    fn sketch_of(data: &[u8]) -> ContentSketch {
        let mut c = Cursor::new(data);
        sketch_reader(&mut c, data.len() as u64).expect("in-memory read cannot fail")
    }

    #[test]
    fn identical_bytes_sketch_identically_regardless_of_reader_position() {
        let data = bytes(10 * CHUNK as usize, 7);
        let a = sketch_of(&data);
        let mut c = Cursor::new(&data);
        c.seek(SeekFrom::Start(1234)).unwrap();
        let b = sketch_reader(&mut c, data.len() as u64).unwrap();
        assert_eq!(a, b, "sketch must not depend on incoming stream position");
    }

    #[test]
    fn a_moved_file_keeps_its_sketch() {
        // The whole point: nothing about the sketch derives from the path or from filesystem
        // metadata, so a move cannot change it.
        let data = bytes(8 * CHUNK as usize, 99);
        let before = sketch_of(&data);
        let after = sketch_of(&data);
        assert_eq!(before, after);
    }

    #[test]
    fn same_length_different_content_sketches_differently() {
        let len = 12 * CHUNK as usize;
        assert_ne!(sketch_of(&bytes(len, 1)), sketch_of(&bytes(len, 2)));
    }

    #[test]
    fn a_change_in_any_sampled_region_changes_the_sketch() {
        let len = 12 * CHUNK as usize;
        let base = bytes(len, 42);
        let original = sketch_of(&base);
        for pos in [0usize, len / 2, len - 1] {
            let mut mutated = base.clone();
            mutated[pos] ^= 0xff;
            assert_ne!(sketch_of(&mutated), original, "flip at {pos} not detected");
        }
    }

    #[test]
    fn truncation_is_always_detected_via_the_length_mix() {
        let data = bytes(12 * CHUNK as usize, 5);
        let truncated = &data[..data.len() - 1];
        assert_ne!(sketch_of(&data), sketch_of(truncated));
    }

    #[test]
    fn small_files_are_read_in_full_and_still_distinguished() {
        let a = bytes(1024, 1);
        let mut b = a.clone();
        b[500] ^= 1;
        assert_ne!(sketch_of(&a), sketch_of(&b));
        assert_eq!(sketch_of(&[]), sketch_of(&[]));
    }

    #[test]
    fn read_cost_is_constant_above_the_threshold() {
        assert_eq!(sketch_read_cost(1024), 1024);
        assert_eq!(sketch_read_cost(FULL_READ_THRESHOLD), FULL_READ_THRESHOLD);
        // A 500 GB remux costs the same 3 MiB as a 4 MiB clip.
        assert_eq!(sketch_read_cost(500 * 1024 * 1024 * 1024), 3 * CHUNK);
        assert_eq!(sketch_read_cost(u64::MAX / 2), 3 * CHUNK);
    }

    #[test]
    fn the_smallest_file_that_enters_sampling_does_not_underflow_its_offsets() {
        // sketch_reader's sampling branch computes `size / 2 - CHUNK / 2` and `size - CHUNK` as plain
        // u64 subtraction -- either going negative would underflow-panic in a debug build and wrap to
        // a huge, wrong seek offset in a release one (overflow checks are off there by construction).
        // Both stay safely positive only because `FULL_READ_THRESHOLD` (the smallest size that takes
        // this branch at all, at `threshold + 1`) is fixed at exactly `3 * CHUNK`: worked through by
        // hand, `size > 3*CHUNK` guarantees `size - CHUNK > 2*CHUNK` and `size/2 - CHUNK/2 > CHUNK`,
        // both comfortably positive -- but that guarantee lives entirely in the relationship between
        // two constants, nothing enforces it if either one is ever tuned independently later. Proven
        // here at the tightest possible case (`threshold + 1`, the smallest input that ever reaches
        // this code) rather than trusted from the constants' current values alone.
        let size = FULL_READ_THRESHOLD + 1;
        let data = bytes(size as usize, 42);
        let sketch = sketch_of(&data); // must not panic
        // And it must still be a real sketch, not an artifact of a seek gone to the wrong place:
        // changing a byte in the tail sample (the region `size - CHUNK` reads from) must move it.
        let mut tail_flipped = data.clone();
        let tail_start = (size - CHUNK) as usize;
        tail_flipped[tail_start] ^= 1;
        assert_ne!(sketch_of(&tail_flipped), sketch, "a change in the tail sample went undetected");
    }

    #[test]
    fn hex_round_trips() {
        let s = ContentSketch(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210);
        assert_eq!(s.to_hex().len(), 32);
        assert_eq!(ContentSketch::from_hex(&s.to_hex()), Some(s));
        assert_eq!(ContentSketch::from_hex("not hex"), None);
        // Leading zeros must survive, or short sketches fail to match on reload.
        assert_eq!(ContentSketch::from_hex(&ContentSketch(1).to_hex()), Some(ContentSketch(1)));
    }

    fn digest_of(data: &[u8]) -> FileDigest {
        let mut c = Cursor::new(data);
        digest_reader(&mut c).expect("in-memory read cannot fail")
    }

    #[test]
    fn digest_is_stable_for_the_same_bytes_across_chunk_boundaries() {
        // A length that is not a whole multiple of CHUNK, so the loop's last iteration is a short
        // read — exactly the boundary condition most likely to silently drop or duplicate bytes.
        let data = bytes(5 * CHUNK as usize + 137, 3);
        assert_eq!(digest_of(&data), digest_of(&data));
    }

    #[test]
    fn digest_catches_a_flip_anywhere_the_sampled_sketch_would_miss() {
        // Nine megabytes: comfortably past FULL_READ_THRESHOLD, so the sketch only samples three
        // 1 MiB regions out of nine — this picks positions in the six MiB the sketch never reads.
        let len = 9 * CHUNK as usize;
        let base = bytes(len, 11);
        let baseline_digest = digest_of(&base);
        let baseline_sketch = sketch_of(&base);

        // Roughly 25% and 75% through the file: past the head sample, short of the middle sample,
        // and short of the tail sample -- squarely in territory the sketch never touches.
        for pos in [len / 4, 3 * len / 4] {
            let mut mutated = base.clone();
            mutated[pos] ^= 0xff;
            assert_eq!(
                sketch_of(&mutated),
                baseline_sketch,
                "test setup invariant broken: position {pos} was supposed to be outside every sampled region"
            );
            assert_ne!(
                digest_of(&mutated),
                baseline_digest,
                "a full-file digest must catch a flip anywhere, unlike the sampled sketch"
            );
        }
    }

    #[test]
    fn digest_of_an_empty_reader_is_deterministic() {
        assert_eq!(digest_of(&[]), digest_of(&[]));
    }

    #[test]
    fn digest_hex_round_trips() {
        let d = FileDigest(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210);
        assert_eq!(d.to_hex().len(), 32);
        assert_eq!(FileDigest::from_hex(&d.to_hex()), Some(d));
        assert_eq!(FileDigest::from_hex("not hex"), None);
        assert_eq!(FileDigest::from_hex(&FileDigest(1).to_hex()), Some(FileDigest(1)));
    }

    #[test]
    fn digest_and_sketch_are_distinct_types_that_do_not_coincidentally_agree() {
        // Not a security property, just a sanity check that two different algorithms over the same
        // small (fully-sampled) input do not somehow collide in a way that would make a future
        // refactor accidentally conflate them.
        let data = bytes(1024, 4);
        let d = digest_of(&data);
        let s = sketch_of(&data);
        assert_ne!(d.0, s.0);
    }

    fn ident(size: u64, mtime: i128, sketch: u128) -> FileIdentity {
        FileIdentity {
            fs: FsFingerprint { device_id: 1, inode: 2, size, mtime_ns: mtime },
            sketch: ContentSketch(sketch),
        }
    }

    #[test]
    fn unchanged_is_the_fast_path() {
        let known = ident(100, 500, 0xaa);
        let verdict =
            classify(&known, Some(&known), |_| panic!("must not consult the sketch index"));
        assert_eq!(verdict, ScanVerdict::Unchanged);
    }

    #[test]
    fn same_path_changed_stat_is_modified() {
        let known = ident(100, 500, 0xaa);
        let now = ident(120, 600, 0xbb);
        assert_eq!(classify(&now, Some(&known), |_| false), ScanVerdict::Modified);
    }

    #[test]
    fn new_path_with_a_known_sketch_is_a_move_not_a_new_item() {
        // This is the assertion that protects watch state across a library reorganisation.
        let now = ident(100, 500, 0xaa);
        assert_eq!(
            classify(&now, None, |s| s == ContentSketch(0xaa)),
            ScanVerdict::Moved { from_sketch: ContentSketch(0xaa) }
        );
    }

    #[test]
    fn genuinely_new_content_is_new() {
        let now = ident(100, 500, 0xaa);
        assert_eq!(classify(&now, None, |_| false), ScanVerdict::New);
    }
}
