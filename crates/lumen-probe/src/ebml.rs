//! Minimal EBML/Matroska structural reader — `docs/12` §2.
//!
//! Not a demuxer. It answers the structural questions that determine *how* a Matroska file must be
//! opened, each of which maps to a documented failure mode in other players:
//!
//! | Question | Why it matters |
//! |---|---|
//! | Unknown-size `Segment`/`Cluster`? | Live/streamed MKV; a player requiring known sizes cannot play it at all (§2.1) |
//! | Non-default `TimestampScale`? | Hard-coding the 1 ms default breaks timing entirely (§2.2) |
//! | `Cues` present, and before or after the clusters? | Absent ⇒ seek by scanning; at the tail ⇒ range-fetch over HTTP (§2.3) |
//! | `ContentCompression` header stripping? | Unhandled ⇒ every frame is corrupt while still "playing" (§2.7) |
//! | `ContentEncryption`? | Must produce a specific message, never a mysterious decode failure (§2.7) |
//! | Attached fonts? | Without them ASS renders in the wrong font — a top "broken subtitles" report (§2.7) |
//! | Segment linking? | Ordered chapters and linked segments need resolving *before* playback (§2.4) |
//! | Declared video codec, geometry, colour? | Answerable from `TrackEntry` alone -- no bitstream parsing, no launching a decoder just to ask "how big is this?" |
//!
//! Every parse is bounded and total: malformed input yields partial findings, never a panic and
//! never an error that stops playback. Per §1 Rule 2, unknown is not fatal.

// Element IDs, stored with their length marker so they compare directly against what is read.
const ID_EBML: u64 = 0x1A45_DFA3;
const ID_DOCTYPE: u64 = 0x4282;
const ID_DOCTYPE_VERSION: u64 = 0x4287;
const ID_SEGMENT: u64 = 0x1853_8067;
const ID_SEEK_HEAD: u64 = 0x114D_9B74;
const ID_INFO: u64 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u64 = 0x2A_D7B1;
const ID_DURATION: u64 = 0x4489;
const ID_PREV_UID: u64 = 0x3C_B923;
const ID_NEXT_UID: u64 = 0x3E_B923;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u64 = 0xAE;
const ID_TRACK_TYPE: u64 = 0x83;
/// Matroska value for a video track. Audio is 2, subtitle is 17 -- neither is relevant here.
const TRACK_TYPE_VIDEO: u64 = 1;
const ID_CODEC_ID: u64 = 0x86;
const ID_VIDEO: u64 = 0xE0;
const ID_PIXEL_WIDTH: u64 = 0xB0;
const ID_PIXEL_HEIGHT: u64 = 0xBA;
const ID_COLOUR: u64 = 0x55B0;
const ID_MATRIX_COEFFICIENTS: u64 = 0x55B1;
const ID_RANGE: u64 = 0x55B9;
const ID_TRANSFER_CHARACTERISTICS: u64 = 0x55BA;
const ID_PRIMARIES: u64 = 0x55BB;
const ID_CONTENT_ENCODINGS: u64 = 0x6D80;
const ID_CONTENT_ENCODING: u64 = 0x6240;
const ID_CONTENT_COMPRESSION: u64 = 0x5034;
const ID_CONTENT_COMP_ALGO: u64 = 0x4254;
const ID_CONTENT_ENCRYPTION: u64 = 0x5035;
const ID_CLUSTER: u64 = 0x1F43_B675;
const ID_CUES: u64 = 0x1C53_BB6B;
const ID_CHAPTERS: u64 = 0x1043_A770;
const ID_EDITION_ENTRY: u64 = 0x45B9;
const ID_EDITION_FLAG_ORDERED: u64 = 0x45DD;
const ID_CHAPTER_ATOM: u64 = 0xB6;
const ID_CHAPTER_SEGMENT_UUID: u64 = 0x6E67;
const ID_CHAPTER_SEGMENT_EDITION_UID: u64 = 0x6EBC;
const ID_ATTACHMENTS: u64 = 0x1941_A469;
const ID_ATTACHED_FILE: u64 = 0x61A7;
const ID_FILE_NAME: u64 = 0x466E;
/// Deliberately parsed but never used as a discriminator: muxers emit wrong MIME types constantly,
/// so font detection goes by filename and magic bytes instead (§2.7). Kept for the diagnostics
/// bundle and for the test that proves wrong MIME types are ignored.
#[allow(dead_code)]
const ID_FILE_MIME_TYPE: u64 = 0x4660;

/// Matroska's default `TimestampScale`: 1 ms in nanoseconds.
pub const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

/// `ContentCompAlgo` values. Algo 3 — header stripping — is the dangerous one: the muxer removes a
/// constant prefix from every frame, so a player that ignores it decodes garbage while appearing to
/// work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgo {
    Zlib,
    Bzlib,
    Lzo1x,
    HeaderStripping,
    Unknown(u64),
}

impl CompressionAlgo {
    fn from_value(v: u64) -> Self {
        match v {
            0 => Self::Zlib,
            1 => Self::Bzlib,
            2 => Self::Lzo1x,
            3 => Self::HeaderStripping,
            other => Self::Unknown(other),
        }
    }
}

/// Where `Cues` sits relative to the first `Cluster`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CuesPlacement {
    /// No `Cues` element. Seeking must fall back to scanning clusters — recovery rung 2, but
    /// seeking must still work (§2.3). Also the correct default for a buffer with no findings.
    #[default]
    Absent,
    /// Before the clusters: streamable, no extra fetch needed.
    Front,
    /// After the clusters. Over HTTP this needs a tail range request before playback, or seeking is
    /// unavailable until the whole file is fetched.
    Tail,
}

/// Codec, geometry and colour read directly from one video `TrackEntry`'s declared properties.
///
/// No bitstream parsing: everything here is what the container itself states, the same "structural,
/// not a demuxer" boundary the rest of this module keeps. In particular `codec` names the container
/// codec (from `CodecID`), not a profile/level -- those live inside the bitstream this reader never
/// touches.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTrackInfo {
    pub codec: lumen_model::VideoCodec,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Populated from the track's `Colour` element when present, `hdr` derived from `transfer` the
    /// same way `fidelity::color_info` derives it from mpv's live properties. `Unspecified`/`Sdr`
    /// fields are the honest default for the (common) case where a muxer wrote no `Colour` element at
    /// all -- absence here is "not stated", never a claim of SDR BT.709.
    pub color: lumen_model::ColorInfo,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatroskaLayout {
    pub doctype: Option<String>,
    pub doctype_version: Option<u64>,
    /// `Segment` declared with an unknown size — live or streamed output (§2.1).
    pub unknown_size_segment: bool,
    pub unknown_size_cluster: bool,
    /// `None` means the element was absent and the 1 ms default applies.
    pub timestamp_scale: Option<u64>,
    pub duration_present: bool,
    pub has_seek_head: bool,
    pub cues: CuesPlacement,
    pub cluster_count_seen: usize,
    pub track_count: usize,
    /// One entry per video `TrackEntry` found, in file order.
    pub video_tracks: Vec<VideoTrackInfo>,
    /// Compression declared on any track. Header stripping *must* be handled.
    pub compression: Vec<CompressionAlgo>,
    /// Any track declares `ContentEncryption`. Playback is T5 with a specific message (§2.7).
    pub encrypted_tracks: bool,
    /// Attachment filenames, used to find fonts for libass. Detection is by extension *and* magic
    /// bytes, never by MIME, because muxers emit wrong MIME types constantly.
    pub attachments: Vec<String>,
    pub font_attachment_count: usize,
    /// Attachments recognised as images by extension -- a Matroska cover is carried exactly like a
    /// font attachment, just under `cover.jpg`/`folder.png` instead of a font filename. Same "never
    /// trust the declared MIME" stance as fonts (§2.7).
    pub image_attachment_count: usize,
    /// The codec of the first recognised image attachment, if any -- enough to know a cover exists
    /// and what format it is without reading `FileData`, which this structural reader never does.
    pub cover_art_codec: Option<lumen_model::ImageCodec>,
    /// Ordered-chapter edition present: a virtual timeline must be built before playback (§2.4).
    pub has_ordered_edition: bool,
    /// Hard linking via `ChapterSegmentUUID`, or an entire linked edition via
    /// `ChapterSegmentEditionUID`. Both need sibling-file resolution and cycle detection.
    pub has_segment_linking: bool,
    pub has_soft_linking: bool,
    /// The parse stopped early because the data ran out. Findings so far are still valid.
    pub truncated: bool,
}

impl MatroskaLayout {
    /// The scale actually in force, applying the default when the element is absent.
    pub fn effective_timestamp_scale(&self) -> u64 {
        self.timestamp_scale.filter(|s| *s > 0).unwrap_or(DEFAULT_TIMESTAMP_SCALE)
    }

    /// Header stripping is present and must be undone before decoding.
    pub fn needs_header_stripping(&self) -> bool {
        self.compression.contains(&CompressionAlgo::HeaderStripping)
    }

    /// Linking structures need resolving before the first frame is shown, including a visited-UID
    /// set to break the endless-loop cases that exist as published test files (§2.4).
    pub fn needs_link_resolution(&self) -> bool {
        self.has_ordered_edition || self.has_segment_linking || self.has_soft_linking
    }

    /// Over HTTP, seeking requires the index up front; a tail-placed or missing `Cues` means either a
    /// range request or a cluster scan.
    pub fn needs_tail_fetch_for_seeking(&self) -> bool {
        matches!(self.cues, CuesPlacement::Tail)
    }

    pub fn is_webm_doctype(&self) -> bool {
        self.doctype.as_deref() == Some("webm")
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// A size that the file declared as unknown (all VINT_DATA bits set).
const UNKNOWN_SIZE: u64 = u64::MAX;

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn byte(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    /// Element ID: leading zeros in the first byte give the length; the marker bit is retained so
    /// the value compares directly against the `ID_*` constants.
    fn read_id(&mut self) -> Option<u64> {
        let first = self.byte()?;
        if first == 0 {
            return None; // not a valid ID; caller resyncs
        }
        let len = first.leading_zeros() as usize + 1;
        if len > 4 || self.remaining() < len - 1 {
            return None;
        }
        let mut id = u64::from(first);
        for _ in 1..len {
            id = (id << 8) | u64::from(self.byte()?);
        }
        Some(id)
    }

    /// Element size: leading zeros give the length; the marker bit is stripped. All data bits set
    /// means "unknown size", which is legal for `Segment` and `Cluster` and is how live Matroska is
    /// written (§2.1).
    fn read_size(&mut self) -> Option<u64> {
        let first = self.byte()?;
        if first == 0 {
            return None;
        }
        let len = first.leading_zeros() as usize + 1;
        if len > 8 || self.remaining() < len - 1 {
            return None;
        }
        // `0xFFu8 >> 8` overflows. A length of 8 means the first byte carries the marker and no data
        // bits — which is precisely how an unknown size is written, so this path is load-bearing.
        let mask = if len >= 8 { 0 } else { 0xFFu8 >> len };
        let mut value = u64::from(first & mask);
        let mut all_ones = (first & mask) == mask;
        for _ in 1..len {
            let b = self.byte()?;
            value = (value << 8) | u64::from(b);
            all_ones &= b == 0xFF;
        }
        if all_ones { Some(UNKNOWN_SIZE) } else { Some(value) }
    }

    fn read_uint(&mut self, len: usize) -> Option<u64> {
        if len == 0 || len > 8 || self.remaining() < len {
            self.pos = (self.pos + len).min(self.buf.len());
            return None;
        }
        let mut v = 0u64;
        for _ in 0..len {
            v = (v << 8) | u64::from(self.byte()?);
        }
        Some(v)
    }

    fn read_string(&mut self, len: usize) -> Option<String> {
        let len = len.min(self.remaining());
        let end = self.pos + len;
        let s =
            String::from_utf8_lossy(&self.buf[self.pos..end]).trim_end_matches('\0').to_string();
        self.pos = end;
        Some(s)
    }

    fn skip(&mut self, len: u64) {
        self.pos = self.buf.len().min(self.pos.saturating_add(len as usize));
    }
}

/// Font attachments, detected by filename extension rather than declared MIME type.
///
/// Muxers emit `application/octet-stream`, `application/x-truetype-font`, `font/ttf`,
/// `application/vnd.ms-opentype`, and outright wrong values interchangeably, so MIME is unusable as
/// a discriminator (§2.7).
fn looks_like_font(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".ttf", ".otf", ".ttc", ".otc", ".woff", ".woff2", ".pfb", ".fon"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The Matroska `CodecID` string to a [`lumen_model::VideoCodec`]. Values are the standardised
/// Matroska codec IDs (matroska.org/technical/codec_specs.html), a fixed, distinct vocabulary from
/// FFmpeg's short names that `fidelity::video_codec` maps elsewhere.
fn video_codec_from_matroska_id(id: &str) -> lumen_model::VideoCodec {
    use lumen_model::VideoCodec;
    match id {
        "V_MPEG4/ISO/AVC" => VideoCodec::H264,
        "V_MPEGH/ISO/HEVC" => VideoCodec::Hevc,
        // The VVC codec ID has not settled on one vendor prefix across muxers yet; both are seen.
        "V_MPEGI/ISO/VVC" | "V_MPEGH/ISO/VVC" => VideoCodec::Vvc,
        "V_AV1" => VideoCodec::Av1,
        "V_VP8" => VideoCodec::Vp8,
        "V_VP9" => VideoCodec::Vp9,
        "V_MPEG1" => VideoCodec::Mpeg1,
        "V_MPEG2" => VideoCodec::Mpeg2,
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/MS/V3" => VideoCodec::Mpeg4Part2,
        "V_THEORA" => VideoCodec::Theora,
        "V_UNCOMPRESSED" => VideoCodec::Uncompressed,
        "V_PRORES" => VideoCodec::ProRes,
        "V_MJPEG" => VideoCodec::Mjpeg,
        other => VideoCodec::Other(other.to_string()),
    }
}

/// ITU-T H.273 (CICP) colour primaries code points -- the same numbering Matroska's `Primaries`
/// element, ISOBMFF's `colr` box, and H.264/HEVC VUI all share.
fn primaries_from_cicp(v: u64) -> lumen_model::ColorPrimaries {
    use lumen_model::ColorPrimaries as P;
    match v {
        1 => P::Bt709,
        5 => P::Bt601_625,
        6 => P::Bt601_525,
        7 => P::Smpte240M,
        9 => P::Bt2020,
        11 => P::DciP3,
        12 => P::DisplayP3,
        _ => P::Unspecified,
    }
}

/// ITU-T H.273 transfer characteristics code points.
fn transfer_from_cicp(v: u64) -> lumen_model::ColorTransfer {
    use lumen_model::ColorTransfer as T;
    match v {
        1 => T::Bt709,
        6 => T::Smpte170M,
        7 => T::Smpte240M,
        8 => T::Linear,
        13 => T::Srgb,
        14 => T::Bt2020_10,
        15 => T::Bt2020_12,
        16 => T::Pq,
        18 => T::Hlg,
        _ => T::Unspecified,
    }
}

/// ITU-T H.273 matrix coefficients code points.
fn matrix_from_cicp(v: u64) -> lumen_model::ColorMatrix {
    use lumen_model::ColorMatrix as M;
    match v {
        1 => M::Bt709,
        5 | 6 => M::Bt601,
        8 => M::YCgCo,
        9 => M::Bt2020Ncl,
        10 => M::Bt2020Cl,
        14 => M::IcTcP,
        _ => M::Unspecified,
    }
}

/// Matroska's own `Range` element values -- distinct from the CICP tables above, and not the same
/// numbering.
fn range_from_matroska(v: u64) -> lumen_model::ColorRange {
    use lumen_model::ColorRange as R;
    match v {
        1 => R::Limited,
        2 => R::Full,
        // 0 = unspecified, 3 = "defined by MatrixCoefficients/TransferCharacteristics" -- resolving
        // that would need logic this structural reader does not have; both are honestly Unspecified.
        _ => R::Unspecified,
    }
}

/// Parse one `TrackEntry`'s body in isolation, returning its declared codec/geometry/colour if (and
/// only if) it is a video track. A separate, bounded pass from [`walk`]'s shared accumulator, since a
/// `TrackEntry`'s several colour/geometry sub-elements need correlating to *one* track and `walk`'s
/// flat structure has no notion of "the track currently being visited".
fn parse_track_entry(buf: &[u8]) -> Option<VideoTrackInfo> {
    #[derive(Default)]
    struct Acc {
        is_video: bool,
        codec_id: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
        primaries: lumen_model::ColorPrimaries,
        transfer: lumen_model::ColorTransfer,
        matrix: lumen_model::ColorMatrix,
        range: lumen_model::ColorRange,
    }

    fn walk_track(r: &mut Reader<'_>, acc: &mut Acc, depth: u8) {
        if depth > MAX_DEPTH {
            return;
        }
        while r.remaining() > 0 {
            let start = r.pos;
            let Some(id) = r.read_id() else {
                r.pos = start + 1;
                continue;
            };
            let Some(size) = r.read_size() else { return };
            let body_len = if size == UNKNOWN_SIZE {
                r.remaining()
            } else {
                (size as usize).min(r.remaining())
            };
            let body_start = r.pos;

            match id {
                ID_TRACK_TYPE => {
                    if r.read_uint(body_len) == Some(TRACK_TYPE_VIDEO) {
                        acc.is_video = true;
                    }
                }
                ID_CODEC_ID => acc.codec_id = r.read_string(body_len),
                ID_PIXEL_WIDTH => acc.width = r.read_uint(body_len).map(|v| v as u32),
                ID_PIXEL_HEIGHT => acc.height = r.read_uint(body_len).map(|v| v as u32),
                ID_MATRIX_COEFFICIENTS => {
                    if let Some(v) = r.read_uint(body_len) {
                        acc.matrix = matrix_from_cicp(v);
                    }
                }
                ID_RANGE => {
                    if let Some(v) = r.read_uint(body_len) {
                        acc.range = range_from_matroska(v);
                    }
                }
                ID_TRANSFER_CHARACTERISTICS => {
                    if let Some(v) = r.read_uint(body_len) {
                        acc.transfer = transfer_from_cicp(v);
                    }
                }
                ID_PRIMARIES => {
                    if let Some(v) = r.read_uint(body_len) {
                        acc.primaries = primaries_from_cicp(v);
                    }
                }
                ID_VIDEO | ID_COLOUR => {
                    let end = (body_start + body_len).min(r.buf.len());
                    walk_track(&mut Reader::new(&r.buf[body_start..end]), acc, depth + 1);
                }
                _ => {}
            }

            r.pos = (body_start + body_len).min(r.buf.len());
            if r.pos <= start {
                return;
            }
        }
    }

    let mut acc = Acc::default();
    walk_track(&mut Reader::new(buf), &mut acc, 0);
    if !acc.is_video {
        return None;
    }
    let hdr = match acc.transfer {
        lumen_model::ColorTransfer::Pq => lumen_model::HdrFormat::Hdr10,
        lumen_model::ColorTransfer::Hlg => lumen_model::HdrFormat::Hlg,
        _ => lumen_model::HdrFormat::Sdr,
    };
    Some(VideoTrackInfo {
        codec: video_codec_from_matroska_id(acc.codec_id.as_deref().unwrap_or("")),
        width: acc.width,
        height: acc.height,
        color: lumen_model::ColorInfo {
            primaries: acc.primaries,
            transfer: acc.transfer,
            matrix: acc.matrix,
            range: acc.range,
            hdr,
            mastering: None,
        },
    })
}

/// Maximum nesting depth. Bounds work on hostile or corrupt input.
const MAX_DEPTH: u8 = 12;

/// Analyse the structure of a Matroska buffer.
///
/// `buf` is normally the leading few MiB. Findings degrade gracefully: a truncated buffer sets
/// [`MatroskaLayout::truncated`] and reports everything it did see.
pub fn analyze(buf: &[u8]) -> Option<MatroskaLayout> {
    let mut r = Reader::new(buf);
    let mut layout = MatroskaLayout { cues: CuesPlacement::Absent, ..Default::default() };

    // The EBML header must come first. Its absence is not fatal — a damaged header is recovery
    // rung 4, where we scan forward for the first `Segment` or `Cluster` (§2.8) — but it does mean
    // this is not a well-formed Matroska file.
    let first_id = r.read_id()?;
    if first_id != ID_EBML {
        return None;
    }
    let header_size = r.read_size()?;
    let header_end = if header_size == UNKNOWN_SIZE {
        buf.len()
    } else {
        (r.pos + header_size as usize).min(buf.len())
    };
    walk(&mut Reader::new(&buf[r.pos..header_end]), &mut layout, 1);
    r.pos = header_end;

    while r.remaining() > 0 {
        let Some(id) = r.read_id() else {
            // Resync: a single junk byte must not abandon the rest of the file (§2.8).
            r.pos += 1;
            continue;
        };
        let Some(size) = r.read_size() else {
            layout.truncated = true;
            break;
        };
        if id == ID_SEGMENT {
            if size == UNKNOWN_SIZE {
                layout.unknown_size_segment = true;
            }
            let end = if size == UNKNOWN_SIZE {
                buf.len()
            } else {
                (r.pos + size as usize).min(buf.len())
            };
            if size != UNKNOWN_SIZE && r.pos + size as usize > buf.len() {
                layout.truncated = true;
            }
            let mut inner = Reader::new(&buf[r.pos..end]);
            walk(&mut inner, &mut layout, 1);
            r.pos = end;
        } else {
            r.skip(size);
        }
    }
    Some(layout)
}

fn walk(r: &mut Reader<'_>, layout: &mut MatroskaLayout, depth: u8) {
    if depth > MAX_DEPTH {
        return;
    }
    while r.remaining() > 0 {
        let start = r.pos;
        let Some(id) = r.read_id() else {
            r.pos = start + 1;
            continue;
        };
        let Some(size) = r.read_size() else {
            layout.truncated = true;
            return;
        };
        let unknown = size == UNKNOWN_SIZE;
        let body_len = if unknown { r.remaining() } else { (size as usize).min(r.remaining()) };
        if !unknown && (size as usize) > r.remaining() {
            layout.truncated = true;
        }
        let body_start = r.pos;

        match id {
            ID_DOCTYPE => layout.doctype = r.read_string(body_len),
            ID_DOCTYPE_VERSION => layout.doctype_version = r.read_uint(body_len),
            ID_TIMESTAMP_SCALE => layout.timestamp_scale = r.read_uint(body_len),
            ID_DURATION => layout.duration_present = true,
            ID_SEEK_HEAD => layout.has_seek_head = true,
            ID_PREV_UID | ID_NEXT_UID => layout.has_soft_linking = true,
            ID_CONTENT_ENCRYPTION => layout.encrypted_tracks = true,
            ID_CONTENT_COMP_ALGO => {
                if let Some(v) = r.read_uint(body_len) {
                    let algo = CompressionAlgo::from_value(v);
                    if !layout.compression.contains(&algo) {
                        layout.compression.push(algo);
                    }
                }
            }
            ID_EDITION_FLAG_ORDERED => {
                if r.read_uint(body_len).is_some_and(|v| v != 0) {
                    layout.has_ordered_edition = true;
                }
            }
            ID_CHAPTER_SEGMENT_UUID | ID_CHAPTER_SEGMENT_EDITION_UID => {
                layout.has_segment_linking = true;
            }
            ID_TRACK_ENTRY => {
                layout.track_count += 1;
                let end = (body_start + body_len).min(r.buf.len());
                let entry_buf = &r.buf[body_start..end];
                if let Some(v) = parse_track_entry(entry_buf) {
                    layout.video_tracks.push(v);
                }
                walk(&mut Reader::new(entry_buf), layout, depth + 1);
            }
            ID_FILE_NAME => {
                if let Some(name) = r.read_string(body_len) {
                    if looks_like_font(&name) {
                        layout.font_attachment_count += 1;
                    }
                    if let Some(codec) = lumen_model::ImageCodec::from_extension(&name) {
                        layout.image_attachment_count += 1;
                        layout.cover_art_codec.get_or_insert(codec);
                    }
                    layout.attachments.push(name);
                }
            }
            ID_CLUSTER => {
                layout.cluster_count_seen += 1;
                if unknown {
                    layout.unknown_size_cluster = true;
                }
                // Clusters carry the payload; nothing structural inside is needed here.
            }
            ID_CUES => {
                // Placement relative to the clusters is what decides whether an HTTP client needs a
                // tail range request before it can seek.
                layout.cues = if layout.cluster_count_seen == 0 {
                    CuesPlacement::Front
                } else {
                    CuesPlacement::Tail
                };
            }
            // Containers worth descending into.
            ID_INFO
            | ID_TRACKS
            | ID_CONTENT_ENCODINGS
            | ID_CONTENT_ENCODING
            | ID_CONTENT_COMPRESSION
            | ID_CHAPTERS
            | ID_EDITION_ENTRY
            | ID_CHAPTER_ATOM
            | ID_ATTACHMENTS
            | ID_ATTACHED_FILE
            | ID_SEGMENT => {
                let end = (body_start + body_len).min(r.buf.len());
                walk(&mut Reader::new(&r.buf[body_start..end]), layout, depth + 1);
            }
            _ => {}
        }

        // Always advance past the declared body, whatever the arm above consumed. Per §1 Rule 2 an
        // unknown element is skipped by size, never treated as an error.
        r.pos = (body_start + body_len).min(r.buf.len());
        if r.pos <= start {
            return; // no forward progress: bail rather than spin
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Builders ────────────────────────────────────────────────────────────────────────────────

    fn id_bytes(id: u64) -> Vec<u8> {
        let bytes = id.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
        bytes[first..].to_vec()
    }

    /// Encode a size as a 4-byte vint: marker `0b0001_0000` in the first byte, then 28 data bits.
    /// Fixed-width keeps the builders trivial to reason about.
    fn size_bytes(size: u64) -> Vec<u8> {
        assert!(size < (1 << 28), "test sizes must fit a 4-byte vint");
        vec![0x10 | ((size >> 24) as u8 & 0x0F), (size >> 16) as u8, (size >> 8) as u8, size as u8]
    }

    fn unknown_size_bytes() -> Vec<u8> {
        vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    }

    fn elem(id: u64, body: &[u8]) -> Vec<u8> {
        let mut v = id_bytes(id);
        v.extend(size_bytes(body.len() as u64));
        v.extend_from_slice(body);
        v
    }

    fn uint_elem(id: u64, value: u64) -> Vec<u8> {
        let mut body = value.to_be_bytes().to_vec();
        while body.len() > 1 && body[0] == 0 {
            body.remove(0);
        }
        elem(id, &body)
    }

    fn str_elem(id: u64, s: &str) -> Vec<u8> {
        elem(id, s.as_bytes())
    }

    fn file(header: &[u8], segment_body: &[u8]) -> Vec<u8> {
        let mut v = elem(ID_EBML, header);
        v.extend(elem(ID_SEGMENT, segment_body));
        v
    }

    fn matroska_header() -> Vec<u8> {
        let mut v = str_elem(ID_DOCTYPE, "matroska");
        v.extend(uint_elem(ID_DOCTYPE_VERSION, 4));
        v
    }

    // ── Tests ───────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn reads_doctype_and_version() {
        let f = file(&matroska_header(), &[]);
        let l = analyze(&f).expect("valid EBML");
        assert_eq!(l.doctype.as_deref(), Some("matroska"));
        assert_eq!(l.doctype_version, Some(4));
        assert!(!l.is_webm_doctype());
    }

    #[test]
    fn webm_doctype_recognised() {
        let f = file(&str_elem(ID_DOCTYPE, "webm"), &[]);
        assert!(analyze(&f).unwrap().is_webm_doctype());
    }

    #[test]
    fn non_default_timestamp_scale_is_read_not_assumed() {
        // docs/12 §2.2: hard-coding the 1 ms default breaks timing entirely on these files.
        let info = elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 100));
        let l = analyze(&file(&matroska_header(), &info)).unwrap();
        assert_eq!(l.timestamp_scale, Some(100));
        assert_eq!(l.effective_timestamp_scale(), 100);
    }

    #[test]
    fn absent_timestamp_scale_falls_back_to_the_1ms_default() {
        let l = analyze(&file(&matroska_header(), &[])).unwrap();
        assert_eq!(l.timestamp_scale, None);
        assert_eq!(l.effective_timestamp_scale(), DEFAULT_TIMESTAMP_SCALE);
    }

    #[test]
    fn zero_timestamp_scale_does_not_produce_a_divide_by_zero() {
        let info = elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 0));
        let l = analyze(&file(&matroska_header(), &info)).unwrap();
        assert_eq!(l.effective_timestamp_scale(), DEFAULT_TIMESTAMP_SCALE);
    }

    #[test]
    fn unknown_size_segment_is_detected() {
        // docs/12 §2.1: live and streamed Matroska. A player requiring known sizes cannot play it.
        let mut f = elem(ID_EBML, &matroska_header());
        f.extend(id_bytes(ID_SEGMENT));
        f.extend(unknown_size_bytes());
        f.extend(elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 1_000_000)));
        let l = analyze(&f).unwrap();
        assert!(l.unknown_size_segment);
        assert_eq!(
            l.timestamp_scale,
            Some(1_000_000),
            "content after an unknown size is still read"
        );
    }

    #[test]
    fn header_stripping_is_detected() {
        // docs/12 §2.7: unhandled, every frame decodes to garbage while appearing to play.
        let comp = elem(ID_CONTENT_COMPRESSION, &uint_elem(ID_CONTENT_COMP_ALGO, 3));
        let enc = elem(ID_CONTENT_ENCODING, &comp);
        let encs = elem(ID_CONTENT_ENCODINGS, &enc);
        let track = elem(ID_TRACK_ENTRY, &encs);
        let tracks = elem(ID_TRACKS, &track);
        let l = analyze(&file(&matroska_header(), &tracks)).unwrap();
        assert!(l.needs_header_stripping());
        assert_eq!(l.compression, vec![CompressionAlgo::HeaderStripping]);
        assert_eq!(l.track_count, 1);
    }

    #[test]
    fn zlib_compression_is_distinguished_from_header_stripping() {
        let comp = elem(ID_CONTENT_COMPRESSION, &uint_elem(ID_CONTENT_COMP_ALGO, 0));
        let track =
            elem(ID_TRACK_ENTRY, &elem(ID_CONTENT_ENCODINGS, &elem(ID_CONTENT_ENCODING, &comp)));
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();
        assert_eq!(l.compression, vec![CompressionAlgo::Zlib]);
        assert!(!l.needs_header_stripping());
    }

    #[test]
    fn encryption_is_detected_so_it_can_be_reported_specifically() {
        let enc = elem(ID_CONTENT_ENCODING, &elem(ID_CONTENT_ENCRYPTION, &[0u8; 4]));
        let track = elem(ID_TRACK_ENTRY, &elem(ID_CONTENT_ENCODINGS, &enc));
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();
        assert!(l.encrypted_tracks, "must be detected, not left as a mystery decode failure");
    }

    #[test]
    fn font_attachments_are_found_by_extension_not_mime() {
        // docs/12 §2.7: muxers emit wrong MIME types constantly. Detection must not rely on them.
        let mut attachments = Vec::new();
        for (name, mime) in [
            ("Roboto-Bold.ttf", "application/octet-stream"),
            ("NotoSans.OTF", "text/plain"),
            ("cover.jpg", "image/jpeg"),
            ("readme.txt", "text/plain"),
        ] {
            let mut body = str_elem(ID_FILE_NAME, name);
            body.extend(str_elem(ID_FILE_MIME_TYPE, mime));
            attachments.extend(elem(ID_ATTACHED_FILE, &body));
        }
        let l = analyze(&file(&matroska_header(), &elem(ID_ATTACHMENTS, &attachments))).unwrap();
        assert_eq!(l.attachments.len(), 4);
        assert_eq!(l.font_attachment_count, 2, "both fonts found despite wrong MIME types");
        assert_eq!(l.image_attachment_count, 1, "the cover, despite the readme and two fonts");
        assert_eq!(l.cover_art_codec, Some(lumen_model::ImageCodec::Jpeg));
    }

    #[test]
    fn a_wrongly_mimed_cover_is_still_found_by_extension() {
        // Same principle as fonts: a muxer's declared MIME is not trusted.
        let mut body = str_elem(ID_FILE_NAME, "folder.png");
        body.extend(str_elem(ID_FILE_MIME_TYPE, "application/octet-stream"));
        let attachments = elem(ID_ATTACHED_FILE, &body);
        let l = analyze(&file(&matroska_header(), &elem(ID_ATTACHMENTS, &attachments))).unwrap();
        assert_eq!(l.image_attachment_count, 1);
        assert_eq!(l.cover_art_codec, Some(lumen_model::ImageCodec::Png));
    }

    #[test]
    fn no_image_attachments_means_no_cover_art_claim() {
        let body = str_elem(ID_FILE_NAME, "Roboto-Bold.ttf");
        let attachments = elem(ID_ATTACHED_FILE, &body);
        let l = analyze(&file(&matroska_header(), &elem(ID_ATTACHMENTS, &attachments))).unwrap();
        assert_eq!(l.image_attachment_count, 0);
        assert_eq!(l.cover_art_codec, None);
    }

    #[test]
    fn cues_placement_distinguishes_front_from_tail() {
        // Front: streamable.
        let mut front = elem(ID_CUES, &[0u8; 8]);
        front.extend(elem(ID_CLUSTER, &[0u8; 16]));
        let l = analyze(&file(&matroska_header(), &front)).unwrap();
        assert_eq!(l.cues, CuesPlacement::Front);
        assert!(!l.needs_tail_fetch_for_seeking());

        // Tail: an HTTP client must range-fetch the end before it can seek.
        let mut tail = elem(ID_CLUSTER, &[0u8; 16]);
        tail.extend(elem(ID_CUES, &[0u8; 8]));
        let l = analyze(&file(&matroska_header(), &tail)).unwrap();
        assert_eq!(l.cues, CuesPlacement::Tail);
        assert!(l.needs_tail_fetch_for_seeking());
    }

    #[test]
    fn absent_cues_is_reported_without_disabling_seeking() {
        // docs/12 §2.3: seeking must still work by scanning clusters. Absent is a fact, not a veto.
        let l = analyze(&file(&matroska_header(), &elem(ID_CLUSTER, &[0u8; 16]))).unwrap();
        assert_eq!(l.cues, CuesPlacement::Absent);
        assert_eq!(l.cluster_count_seen, 1);
    }

    #[test]
    fn ordered_editions_and_segment_linking_are_flagged_for_pre_resolution() {
        // docs/12 §2.4: linking must be evaluated before playback, with cycle detection.
        let atom = elem(ID_CHAPTER_ATOM, &elem(ID_CHAPTER_SEGMENT_UUID, &[0xAB; 16]));
        let mut edition = uint_elem(ID_EDITION_FLAG_ORDERED, 1);
        edition.extend(atom);
        let chapters = elem(ID_CHAPTERS, &elem(ID_EDITION_ENTRY, &edition));
        let l = analyze(&file(&matroska_header(), &chapters)).unwrap();
        assert!(l.has_ordered_edition);
        assert!(l.has_segment_linking);
        assert!(l.needs_link_resolution());
    }

    #[test]
    fn ordered_flag_set_to_zero_is_not_an_ordered_edition() {
        let edition = uint_elem(ID_EDITION_FLAG_ORDERED, 0);
        let chapters = elem(ID_CHAPTERS, &elem(ID_EDITION_ENTRY, &edition));
        let l = analyze(&file(&matroska_header(), &chapters)).unwrap();
        assert!(!l.has_ordered_edition);
    }

    #[test]
    fn soft_linking_via_prev_next_uid_is_detected() {
        let info = elem(ID_INFO, &elem(ID_NEXT_UID, &[0x11; 16]));
        assert!(analyze(&file(&matroska_header(), &info)).unwrap().has_soft_linking);
    }

    #[test]
    fn unknown_elements_are_skipped_not_fatal() {
        // docs/12 §1 Rule 2. A private extension element must not stop the parse.
        let mut body = elem(0x3F_1234, &[0xDE, 0xAD, 0xBE, 0xEF]); // invented ID
        body.extend(elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 500_000)));
        let l = analyze(&file(&matroska_header(), &body)).unwrap();
        assert_eq!(l.timestamp_scale, Some(500_000), "parse continued past the unknown element");
    }

    #[test]
    fn truncation_at_every_offset_never_panics() {
        // The parser runs on hostile and partial input; a panic here is a denial of service.
        let mut body = elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 100));
        body.extend(elem(ID_TRACKS, &elem(ID_TRACK_ENTRY, &[0u8; 4])));
        body.extend(elem(ID_CLUSTER, &[0u8; 32]));
        body.extend(elem(ID_CUES, &[0u8; 8]));
        let full = file(&matroska_header(), &body);
        for cut in 0..full.len() {
            let _ = analyze(&full[..cut]);
        }
    }

    #[test]
    fn garbage_between_elements_is_resynced() {
        // docs/12 §2.8: scan forward rather than abandoning the file.
        let mut body = vec![0x00, 0x00, 0x00];
        body.extend(elem(ID_INFO, &uint_elem(ID_TIMESTAMP_SCALE, 250_000)));
        let l = analyze(&file(&matroska_header(), &body)).unwrap();
        assert_eq!(l.timestamp_scale, Some(250_000));
    }

    #[test]
    fn a_non_matroska_buffer_is_rejected_cleanly() {
        assert!(analyze(b"not matroska at all").is_none());
        assert!(analyze(&[]).is_none());
        // An ISOBMFF file must not be mistaken for Matroska.
        let mut mp4 = vec![0, 0, 0, 0x18];
        mp4.extend_from_slice(b"ftypisom");
        assert!(analyze(&mp4).is_none());
    }

    #[test]
    fn deeply_nested_input_terminates() {
        // Bound the work an adversarial file can demand.
        let mut body = vec![0u8; 4];
        for _ in 0..64 {
            body = elem(ID_TRACKS, &body);
        }
        let l = analyze(&file(&matroska_header(), &body));
        assert!(l.is_some(), "must return rather than recurse without bound");
    }

    fn colour_elem(matrix: u64, range: u64, transfer: u64, primaries: u64) -> Vec<u8> {
        let mut body = uint_elem(ID_MATRIX_COEFFICIENTS, matrix);
        body.extend(uint_elem(ID_RANGE, range));
        body.extend(uint_elem(ID_TRANSFER_CHARACTERISTICS, transfer));
        body.extend(uint_elem(ID_PRIMARIES, primaries));
        elem(ID_COLOUR, &body)
    }

    fn video_track(codec_id: &str, width: u64, height: u64, colour: Option<Vec<u8>>) -> Vec<u8> {
        let mut video_body = uint_elem(ID_PIXEL_WIDTH, width);
        video_body.extend(uint_elem(ID_PIXEL_HEIGHT, height));
        if let Some(c) = colour {
            video_body.extend(c);
        }
        let mut body = uint_elem(ID_TRACK_TYPE, TRACK_TYPE_VIDEO);
        body.extend(str_elem(ID_CODEC_ID, codec_id));
        body.extend(elem(ID_VIDEO, &video_body));
        elem(ID_TRACK_ENTRY, &body)
    }

    #[test]
    fn a_video_tracks_codec_geometry_and_hdr_colour_are_read_from_the_container() {
        // The UHD HDR10 remux case: HEVC Main 10, BT.2020 NCL, PQ transfer, PQ signals HDR10.
        let colour = colour_elem(9, 1, 16, 9);
        let track = video_track("V_MPEGH/ISO/HEVC", 3840, 2160, Some(colour));
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();

        assert_eq!(l.video_tracks.len(), 1);
        let v = &l.video_tracks[0];
        assert_eq!(v.codec, lumen_model::VideoCodec::Hevc);
        assert_eq!(v.width, Some(3840));
        assert_eq!(v.height, Some(2160));
        assert_eq!(v.color.matrix, lumen_model::ColorMatrix::Bt2020Ncl);
        assert_eq!(v.color.range, lumen_model::ColorRange::Limited);
        assert_eq!(v.color.transfer, lumen_model::ColorTransfer::Pq);
        assert_eq!(v.color.primaries, lumen_model::ColorPrimaries::Bt2020);
        assert_eq!(v.color.hdr, lumen_model::HdrFormat::Hdr10, "PQ transfer implies HDR10");
    }

    #[test]
    fn a_video_track_with_no_colour_element_is_reported_as_unspecified_not_sdr_bt709() {
        let track = video_track("V_MPEG4/ISO/AVC", 1920, 1080, None);
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();

        let v = &l.video_tracks[0];
        assert_eq!(v.codec, lumen_model::VideoCodec::H264);
        assert_eq!(v.color.primaries, lumen_model::ColorPrimaries::Unspecified);
        assert_eq!(v.color.hdr, lumen_model::HdrFormat::Sdr, "no PQ/HLG transfer means SDR");
    }

    #[test]
    fn an_audio_track_never_produces_a_video_track_entry() {
        let body = uint_elem(ID_TRACK_TYPE, 2); // audio
        let track = elem(ID_TRACK_ENTRY, &body);
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();
        assert!(l.video_tracks.is_empty());
        assert_eq!(l.track_count, 1, "still counted as a track, just not a video one");
    }

    #[test]
    fn multiple_video_tracks_are_all_reported_in_order() {
        let mut tracks = video_track("V_VP9", 1280, 720, None);
        tracks.extend(video_track("V_AV1", 3840, 2160, None));
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &tracks))).unwrap();
        assert_eq!(l.video_tracks.len(), 2);
        assert_eq!(l.video_tracks[0].codec, lumen_model::VideoCodec::Vp9);
        assert_eq!(l.video_tracks[1].codec, lumen_model::VideoCodec::Av1);
    }

    #[test]
    fn an_unrecognised_codec_id_is_representable_not_an_error() {
        let track = video_track("V_SOME_FUTURE_CODEC", 640, 480, None);
        let l = analyze(&file(&matroska_header(), &elem(ID_TRACKS, &track))).unwrap();
        assert_eq!(
            l.video_tracks[0].codec,
            lumen_model::VideoCodec::Other("V_SOME_FUTURE_CODEC".into())
        );
    }
}
