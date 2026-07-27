//! Recursive library scan.
//!
//! Walks a directory, decides what each file actually is by reading its first bytes, and parses the
//! name for a title. The output is a playlist and a report — the two things needed to answer "does my
//! collection play?".
//!
//! **Content decides, not the extension.** `lumen_probe::sniff` reads the magic bytes. An extension
//! that disagrees with the content is recorded rather than obeyed: a `.avi` that is really Matroska
//! plays fine and the mismatch is a note, while trusting the extension would have set up the wrong
//! demuxer. The reverse case matters more — a `.mkv` whose bytes are not Matroska is usually a
//! truncated download, and knowing that before playback beats a mystery failure during it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lumen_match::{EpisodeSpec, ParsedName};
use lumen_model::Container;
use lumen_probe::{Candidate, Confidence, sniff};

/// Bytes read from the head of each file for sniffing.
///
/// Enough for every signature in `lumen_probe::magic` plus the `ftyp` brand list, and small enough
/// that scanning a thousand files over a network share stays interactive.
const HEAD_BYTES: usize = 4096;

/// Directories never worth descending into. Skipped by name at any depth.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    "node_modules",
    "$RECYCLE.BIN",
    "System Volume Information",
    ".Trash",
    ".Trashes",
    "@eaDir",
    "lost+found",
    "target",
];

/// Extensions that are sidecar subtitles rather than playable media.
const SUBTITLE_EXTS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt", "sup", "smi"];

/// Extensions for audio-only files. Container sniffing cannot tell an audio-only MP4 from a video
/// one without parsing tracks, so the extension is the cheap first pass and mpv corrects it later.
const AUDIO_EXTS: &[&str] =
    &["mp3", "flac", "wav", "m4a", "aac", "ogg", "opus", "wma", "alac", "aiff", "ape", "dsf", "wv"];

/// Extensions worth opening even when the bytes are unrecognised.
///
/// Sniffing cannot identify a raw elementary stream or a fragmented segment from its head, and
/// refusing those would violate the "no refusal" guarantee (`docs/11` §G2) at the scan stage — before
/// the player ever gets a chance to try.
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "flv", "webm", "ts", "m2ts", "mts", "mpg", "mpeg",
    "vob", "ogv", "3gp", "divx", "rmvb", "asf", "m2v", "mxf", "iso",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaKind {
    Video,
    Audio,
    /// A sidecar subtitle. Not played on its own; attached to its neighbour by mpv's own auto-load.
    Subtitle,
    /// Read as media by neither content nor extension.
    Other,
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: u64,
    pub extension: Option<String>,
    pub kind: MediaKind,
    /// Best container guess from the file's own bytes, if any signature matched.
    pub container: Option<Container>,
    pub confidence: Option<Confidence>,
    /// Why the container was chosen, carried through for the report.
    pub evidence: Option<&'static str>,
    /// The extension claims one container and the bytes say another.
    pub extension_mismatch: bool,
    /// No signature matched at all. Not a refusal — the player still tries.
    pub unidentified: bool,
    pub parsed: ParsedName,
}

impl ScannedFile {
    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    /// A short human label: the parsed title, with episode numbering when present.
    pub fn label(&self) -> String {
        let mut s = self.parsed.title.clone();
        if s.is_empty() {
            return self.file_name();
        }
        if let Some(y) = self.parsed.year {
            s.push_str(&format!(" ({y})"));
        }
        match &self.parsed.episode {
            Some(EpisodeSpec::SeasonEpisode { season, episodes }) => {
                let eps: Vec<String> = episodes.iter().map(|e| format!("E{e:02}")).collect();
                s.push_str(&format!(" S{season:02}{}", eps.join("-")));
            }
            Some(EpisodeSpec::SeasonOnly { season }) => s.push_str(&format!(" S{season:02}")),
            Some(EpisodeSpec::Absolute(n)) => {
                if let Some(first) = n.first() {
                    s.push_str(&format!(" #{first}"));
                }
            }
            Some(EpisodeSpec::Date { year, month, day }) => {
                s.push_str(&format!(" {year}-{month:02}-{day:02}"));
            }
            None => {}
        }
        s
    }

    /// Notes worth showing next to this file in the report.
    pub fn notes(&self) -> Vec<String> {
        let mut n = Vec::new();
        if self.extension_mismatch {
            n.push(format!(
                "extension says .{} but the bytes are {:?}",
                self.extension.as_deref().unwrap_or("?"),
                self.container.expect("a mismatch requires a sniffed container")
            ));
        }
        if self.unidentified && self.kind != MediaKind::Subtitle {
            n.push("no container signature matched; the player will still try it".into());
        }
        if self.parsed.is_sample {
            n.push("looks like a sample clip".into());
        }
        if self.parsed.title.is_empty() {
            n.push("no title could be parsed from the name".into());
        }
        // A "video" file this small is nearly always a stub, a failed download, or a trailer that
        // slipped past the sample check. Worth a note, never a reason to skip.
        if self.kind == MediaKind::Video && self.size > 0 && self.size < 1_000_000 {
            n.push(format!("only {} KiB — likely truncated or a stub", self.size / 1024));
        }
        if self.size == 0 {
            n.push("zero bytes".into());
        }
        n
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Include files that parse as sample clips.
    pub include_samples: bool,
    /// Stop after this many playable files. `None` for no limit.
    pub limit: Option<usize>,
    /// Maximum directory depth. `None` for unlimited.
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub files: Vec<ScannedFile>,
    /// Directories that could not be read, with the reason. Recorded rather than fatal: one
    /// unreadable folder must not abort a scan of a whole library.
    pub unreadable: Vec<(PathBuf, String)>,
    pub skipped_samples: usize,
    /// True when `limit` cut the scan short, so the report can say so instead of implying the
    /// library ends here.
    pub truncated: bool,
}

impl Scan {
    pub fn playable(&self) -> impl Iterator<Item = &ScannedFile> {
        self.files.iter().filter(|f| matches!(f.kind, MediaKind::Video | MediaKind::Audio))
    }

    pub fn subtitles(&self) -> impl Iterator<Item = &ScannedFile> {
        self.files.iter().filter(|f| f.kind == MediaKind::Subtitle)
    }
}

fn lower_ext(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The container an extension implies, for the mismatch check.
///
/// Only unambiguous extensions are listed. `.ts` is deliberately absent — it is MPEG-TS to a media
/// library and TypeScript to a source tree, and a mismatch warning on either would be noise.
fn container_for_ext(ext: &str) -> Option<Container> {
    Some(match ext {
        "mkv" => Container::Matroska,
        "webm" => Container::WebM,
        "mp4" | "m4v" | "mov" => Container::Mp4,
        "avi" => Container::Avi,
        "wmv" | "asf" => Container::Asf,
        "flv" => Container::Flv,
        "ogv" => Container::Ogg,
        _ => return None,
    })
}

/// Do a sniffed container and an extension-implied one describe the same file?
///
/// Deliberately lenient about families. MP4, QuickTime and fragmented MP4 share a byte structure and
/// the same demuxer opens all three, so `.mov` holding a fragmented MP4 is not worth a warning; WebM
/// is a Matroska profile, so a `.mkv` sniffed as WebM is correct rather than mismatched. Warning on
/// those would bury the mismatch that matters — a `.mkv` that is really an HTML error page.
fn same_family(a: Container, b: Container) -> bool {
    use Container as C;
    if a == b {
        return true;
    }
    let isobmff = |c: C| matches!(c, C::Mp4 | C::FragmentedMp4);
    let matroska = |c: C| matches!(c, C::Matroska | C::WebM);
    (isobmff(a) && isobmff(b)) || (matroska(a) && matroska(b))
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn classify(path: &Path, size: u64) -> ScannedFile {
    let ext = lower_ext(path);
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let parsed = lumen_match::parse(&name);

    let is_subtitle = ext.as_deref().is_some_and(|e| SUBTITLE_EXTS.contains(&e));
    // Sniffing a subtitle is pointless — they have no container magic — and reading the head of
    // every .srt in a large library is real time on a network share.
    let candidates: Vec<Candidate> = if is_subtitle {
        Vec::new()
    } else {
        read_head(path).map(|h| sniff(&h)).unwrap_or_default()
    };

    // `sniff` always ends its list with a `Weak` raw-elementary-stream fallback, because the demuxer
    // is entitled to try that on anything. For classification it means the opposite of a match:
    // treating it as identification would make every text file in the tree a video.
    let best =
        candidates.iter().filter(|c| c.confidence > Confidence::Weak).max_by_key(|c| c.confidence);
    let container = best.map(|c| c.container);
    let ext_container = ext.as_deref().and_then(container_for_ext);

    // Only a confident sniff may contradict an extension. A `Weak` candidate is itself derived from
    // guesswork, and letting it raise a mismatch would flag files that are perfectly fine.
    let extension_mismatch = match (container, ext_container, best.map(|c| c.confidence)) {
        (Some(sniffed), Some(claimed), Some(Confidence::Certain | Confidence::Probable)) => {
            !same_family(sniffed, claimed)
        }
        _ => false,
    };

    let kind = if is_subtitle {
        MediaKind::Subtitle
    } else if ext.as_deref().is_some_and(|e| AUDIO_EXTS.contains(&e)) {
        MediaKind::Audio
    } else if container.is_some() || ext.as_deref().is_some_and(|e| VIDEO_EXTS.contains(&e)) {
        MediaKind::Video
    } else {
        MediaKind::Other
    };

    ScannedFile {
        path: path.to_path_buf(),
        size,
        extension: ext,
        kind,
        container,
        confidence: best.map(|c| c.confidence),
        evidence: best.map(|c| c.evidence),
        extension_mismatch,
        unidentified: container.is_none(),
        parsed,
    }
}

/// Walk `roots` and classify everything found.
///
/// A file given directly on the command line is classified whatever it looks like — an explicit
/// argument is an instruction, not a suggestion, and second-guessing it is how a player ends up
/// refusing to open something the user can see is there.
pub fn scan(roots: &[PathBuf], opts: &ScanOptions) -> Scan {
    let mut out = Scan::default();
    for root in roots {
        match std::fs::metadata(root) {
            Ok(m) if m.is_file() => {
                let mut f = classify(root, m.len());
                if f.kind == MediaKind::Other {
                    f.kind = MediaKind::Video;
                }
                out.files.push(f);
            }
            Ok(_) => walk(root, 0, opts, &mut out),
            Err(e) => out.unreadable.push((root.clone(), e.to_string())),
        }
    }

    // Sort so a scan of the same library twice produces the same playlist. Directory iteration order
    // is not defined by any filesystem, and an unstable playlist makes two runs incomparable.
    out.files.sort_by(|a, b| a.path.cmp(&b.path));

    if let Some(limit) = opts.limit {
        let mut kept = 0usize;
        let mut truncated = false;
        out.files.retain(|f| {
            if !matches!(f.kind, MediaKind::Video | MediaKind::Audio) {
                return true;
            }
            if kept < limit {
                kept += 1;
                true
            } else {
                truncated = true;
                false
            }
        });
        out.truncated = truncated;
    }
    out
}

fn walk(dir: &Path, depth: usize, opts: &ScanOptions, out: &mut Scan) {
    if opts.max_depth.is_some_and(|d| depth > d) {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            // An unreadable folder is recorded and stepped over. Aborting the whole scan because one
            // directory denied permission would be the wrong trade every time.
            out.unreadable.push((dir.to_path_buf(), e.to_string()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        // `file_type` does not follow symlinks, which is what stops a link pointing at an ancestor
        // from walking the scan into an infinite loop.
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(&name)) || name.starts_with('.') {
                continue;
            }
            walk(&path, depth + 1, opts, out);
            continue;
        }
        if name.starts_with('.') {
            continue;
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let f = classify(&path, size);
        if f.kind == MediaKind::Other {
            continue;
        }
        if f.parsed.is_sample && !opts.include_samples {
            out.skipped_samples += 1;
            continue;
        }
        out.files.push(f);
    }
}

/// One logical item — a film, or one season of a show — with the files that make it up.
#[derive(Debug, Clone)]
pub struct Item {
    pub title: String,
    pub year: Option<u16>,
    pub season: Option<u16>,
    pub files: Vec<usize>,
}

/// Group scanned files into logical items.
///
/// This is what turns a pile of paths into a collection view. Films key on title and year; episodes
/// key on title and season so a season lists as one item rather than twenty-four.
pub fn group(scan: &Scan) -> Vec<Item> {
    let mut map: BTreeMap<(String, Option<u16>, Option<u16>), Item> = BTreeMap::new();
    for (i, f) in scan.files.iter().enumerate() {
        if !matches!(f.kind, MediaKind::Video | MediaKind::Audio) {
            continue;
        }
        let season = match &f.parsed.episode {
            Some(
                EpisodeSpec::SeasonEpisode { season, .. } | EpisodeSpec::SeasonOnly { season },
            ) => Some(*season),
            _ => None,
        };
        let title = if f.parsed.title.is_empty() { f.file_name() } else { f.parsed.title.clone() };
        // A series keys on title and season alone. Episode filenames often carry the *episode's* air
        // year, so including the year would scatter one season across several items.
        let year = if season.is_some() { None } else { f.parsed.year };
        let key = (title.to_ascii_lowercase(), year, season);
        map.entry(key)
            .or_insert_with(|| Item { title, year, season, files: Vec::new() })
            .files
            .push(i);
    }
    map.into_values().collect()
}

/// Playlist order: series by season and episode, films by title, parts in order.
///
/// Returns indices into `scan.files`. Sorting by path alone would interleave a show's specials with
/// its episodes and play `Part 10` before `Part 2`.
pub fn playlist_order(scan: &Scan) -> Vec<usize> {
    let mut idx: Vec<usize> = scan
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f.kind, MediaKind::Video | MediaKind::Audio))
        .map(|(i, _)| i)
        .collect();

    idx.sort_by(|&a, &b| {
        let (fa, fb) = (&scan.files[a], &scan.files[b]);
        let key = |f: &ScannedFile| {
            let (season, episode) = match &f.parsed.episode {
                Some(EpisodeSpec::SeasonEpisode { season, episodes }) => {
                    (i64::from(*season), episodes.first().map_or(0, |e| i64::from(*e)))
                }
                Some(EpisodeSpec::SeasonOnly { season }) => (i64::from(*season), -1),
                Some(EpisodeSpec::Absolute(n)) => {
                    (0, n.first().map_or(0, |v| i64::from(*v).min(i64::from(u32::MAX))))
                }
                // A dated episode sorts chronologically among its siblings.
                Some(EpisodeSpec::Date { year, month, day }) => {
                    (i64::from(*year), i64::from(*month) * 100 + i64::from(*day))
                }
                None => (-1, -1),
            };
            (
                f.parsed.title.to_ascii_lowercase(),
                season,
                episode,
                i64::from(f.parsed.part.unwrap_or(0)),
            )
        };
        key(fa).cmp(&key(fb)).then_with(|| fa.path.cmp(&fb.path))
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A directory that deletes itself. No tempfile crate, and a test that leaks directories into
    /// the user's tree is its own bug.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // The address of a local is unique among concurrently-running tests in one process, and
            // the pid separates test binaries. No clock needed, so the name is reproducible enough
            // to debug and unique enough not to collide.
            let anchor = 0u8;
            let dir = std::env::temp_dir().join(format!(
                "lumen-scan-{tag}-{}-{:x}",
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
            let mut f = std::fs::File::create(&p).unwrap();
            f.write_all(bytes).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Bytes that sniff as Matroska: the EBML magic, padded so size heuristics do not fire.
    fn mkv_bytes() -> Vec<u8> {
        let mut v = vec![0x1A, 0x45, 0xDF, 0xA3];
        v.extend(std::iter::repeat_n(0u8, 2_000_000));
        v
    }

    /// Bytes that sniff as ISOBMFF: `ftyp` at offset 4 with an `isom` brand.
    fn mp4_bytes() -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x00, 0x18];
        v.extend_from_slice(b"ftypisom\x00\x00\x02\x00isomiso2");
        v.extend(std::iter::repeat_n(0u8, 2_000_000));
        v
    }

    #[test]
    fn a_library_tree_is_walked_and_classified() {
        let d = TempDir::new("tree");
        d.file("Movies/Arrival (2016) 2160p BluRay x265.mkv", &mkv_bytes());
        d.file("Shows/Chernobyl/Season 01/Chernobyl.S01E03.1080p.mkv", &mkv_bytes());
        d.file("Shows/Chernobyl/Season 01/Chernobyl.S01E03.eng.srt", b"1\n00:00:01,000 --> x\n");
        d.file("Movies/notes.txt", b"not media");

        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        assert_eq!(s.playable().count(), 2, "{:?}", s.files);
        assert_eq!(s.subtitles().count(), 1);
        assert!(!s.files.iter().any(|f| f.file_name() == "notes.txt"), "a .txt is not media");

        let arrival = s.files.iter().find(|f| f.parsed.title.contains("Arrival")).unwrap();
        assert_eq!(arrival.parsed.year, Some(2016));
        assert_eq!(arrival.container, Some(Container::Matroska));
        assert_eq!(arrival.kind, MediaKind::Video);
    }

    #[test]
    fn content_decides_when_the_extension_disagrees() {
        // The case that matters: a `.mkv` whose bytes are not Matroska is usually a failed download,
        // and finding that at scan time beats a mystery failure mid-playback.
        let d = TempDir::new("mismatch");
        d.file("Film.mkv", &mp4_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let f = &s.files[0];
        assert_eq!(f.container, Some(Container::Mp4));
        assert!(f.extension_mismatch);
        assert!(f.notes().iter().any(|n| n.contains("extension says")), "{:?}", f.notes());
    }

    #[test]
    fn related_container_families_are_not_reported_as_mismatches() {
        // A `.mov` holding a fragmented MP4 opens with the same demuxer, and warning about it would
        // bury the mismatch that actually means something.
        assert!(same_family(Container::Mp4, Container::FragmentedMp4));
        assert!(same_family(Container::Matroska, Container::WebM));
        assert!(!same_family(Container::Matroska, Container::Mp4));
        assert!(!same_family(Container::Avi, Container::Mp4));
    }

    #[test]
    fn an_unidentifiable_file_with_a_media_extension_is_still_offered() {
        // Refusing at scan time would break the "no refusal" guarantee before the player ever tried.
        // Plenty of real media has no signature: raw elementary streams, and any file whose header
        // was lost. A media extension is enough reason to hand it to the demuxer.
        let d = TempDir::new("unknown");
        d.file("recovered-stream.mkv", &[0x5Au8; 4096]);
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let f = &s.files[0];
        assert!(f.unidentified, "nothing should have matched these bytes");
        assert_eq!(f.kind, MediaKind::Video, "a media extension is enough to try");
        assert!(f.notes().iter().any(|n| n.contains("will still try")), "{:?}", f.notes());
    }

    #[test]
    fn a_transport_stream_is_recognised_from_its_sync_bytes() {
        // MPEG-TS has no magic sequence — it is the 0x47 sync byte recurring every 188 bytes. Broadcast
        // captures and Blu-ray rips are full of these, so failing to recognise one would be a large
        // hole in a library scan.
        let d = TempDir::new("ts");
        let mut bytes = vec![0u8; 188 * 12];
        for i in 0..12 {
            bytes[i * 188] = 0x47;
        }
        d.file("capture.ts", &bytes);
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        assert_eq!(s.files[0].container, Some(Container::MpegTs), "{:?}", s.files[0]);
    }

    #[test]
    fn a_text_file_is_not_media_even_though_the_demuxer_would_try_it() {
        // `sniff` ends every list with a weak "try it as a raw stream" fallback, because the demuxer
        // is entitled to attempt that on anything. Reading it as identification would put every
        // README in the library into the playlist.
        let d = TempDir::new("text");
        d.file("notes.txt", b"not media at all");
        d.file("readme", b"also not media");
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        assert_eq!(s.playable().count(), 0, "{:?}", s.files);
    }

    #[test]
    fn hidden_and_junk_directories_are_skipped() {
        let d = TempDir::new("junk");
        d.file(".hidden/Film.mkv", &mkv_bytes());
        d.file("node_modules/Film.mkv", &mkv_bytes());
        d.file("@eaDir/Film.mkv", &mkv_bytes());
        d.file("Real/Film.mkv", &mkv_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        assert_eq!(
            s.playable().count(),
            1,
            "{:?}",
            s.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn samples_are_skipped_by_default_and_counted() {
        let d = TempDir::new("samples");
        d.file("Film-sample.mkv", &mkv_bytes());
        d.file("Film.mkv", &mkv_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        assert_eq!(s.playable().count(), 1);
        assert_eq!(s.skipped_samples, 1);

        let with = scan(
            std::slice::from_ref(&d.0),
            &ScanOptions { include_samples: true, ..Default::default() },
        );
        assert_eq!(with.playable().count(), 2);
    }

    #[test]
    fn an_explicit_file_argument_is_played_whatever_it_looks_like() {
        // An argument is an instruction. Second-guessing it is how a player ends up refusing to open
        // something the user can plainly see.
        let d = TempDir::new("explicit");
        let p = d.file("no-extension-at-all", &mkv_bytes());
        let s = scan(&[p], &ScanOptions::default());
        assert_eq!(s.playable().count(), 1);
    }

    #[test]
    fn an_unreadable_root_is_recorded_rather_than_fatal() {
        let s = scan(&[PathBuf::from("/definitely/not/here")], &ScanOptions::default());
        assert!(s.files.is_empty());
        assert_eq!(s.unreadable.len(), 1);
    }

    #[test]
    fn scanning_twice_produces_the_same_order() {
        // Directory iteration order is undefined, and an unstable playlist makes two runs
        // incomparable — which defeats the point of a test run.
        let d = TempDir::new("stable");
        for n in ["c.mkv", "a.mkv", "b.mkv"] {
            d.file(n, &mkv_bytes());
        }
        let first = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let second = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let paths = |s: &Scan| s.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&first), paths(&second));
    }

    #[test]
    fn a_limit_truncates_and_says_so() {
        let d = TempDir::new("limit");
        for n in ["a.mkv", "b.mkv", "c.mkv"] {
            d.file(n, &mkv_bytes());
        }
        let s =
            scan(std::slice::from_ref(&d.0), &ScanOptions { limit: Some(2), ..Default::default() });
        assert_eq!(s.playable().count(), 2);
        assert!(s.truncated, "a truncated scan must not read as the whole library");
    }

    #[test]
    fn a_season_groups_as_one_item() {
        let d = TempDir::new("group");
        for e in 1..=3 {
            d.file(&format!("Show.S02E{e:02}.1080p.mkv"), &mkv_bytes());
        }
        d.file("Some Film (1999) 1080p.mkv", &mkv_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let items = group(&s);
        assert_eq!(items.len(), 2, "{items:#?}");
        let season = items.iter().find(|i| i.season == Some(2)).expect("the season");
        assert_eq!(season.files.len(), 3);
    }

    #[test]
    fn episodes_play_in_order_rather_than_alphabetically() {
        // E10 sorts before E2 by path, which is exactly the wrong order to watch a season in.
        let d = TempDir::new("order");
        for e in [10u32, 2, 1] {
            d.file(&format!("Show.S01E{e:02}.mkv"), &mkv_bytes());
        }
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let order = playlist_order(&s);
        let eps: Vec<String> = order.iter().map(|&i| s.files[i].file_name()).collect();
        assert_eq!(eps, vec!["Show.S01E01.mkv", "Show.S01E02.mkv", "Show.S01E10.mkv"], "{eps:?}");
    }

    #[test]
    fn multi_part_films_play_in_part_order() {
        let d = TempDir::new("parts");
        d.file("Long Film (1962) cd2.mkv", &mkv_bytes());
        d.file("Long Film (1962) cd1.mkv", &mkv_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let order = playlist_order(&s);
        let names: Vec<String> = order.iter().map(|&i| s.files[i].file_name()).collect();
        assert!(names[0].contains("cd1"), "{names:?}");
    }

    #[test]
    fn a_zero_byte_file_is_noted_and_still_listed() {
        let d = TempDir::new("empty");
        d.file("Broken.mkv", b"");
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let f = &s.files[0];
        assert!(f.notes().iter().any(|n| n.contains("zero bytes")), "{:?}", f.notes());
        assert_eq!(f.kind, MediaKind::Video, "still offered; the player reports the real error");
    }

    #[test]
    fn labels_read_the_way_a_person_would_write_them() {
        let d = TempDir::new("label");
        d.file("Chernobyl.S01E03.1080p.mkv", &mkv_bytes());
        d.file("Arrival (2016) 2160p.mkv", &mkv_bytes());
        let s = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let labels: Vec<String> = s.files.iter().map(ScannedFile::label).collect();
        assert!(labels.iter().any(|l| l.contains("S01E03")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("(2016)")), "{labels:?}");
    }
}
