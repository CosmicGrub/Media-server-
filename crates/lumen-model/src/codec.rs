//! Codec identity.
//!
//! Every enum carries an `Other(String)` arm. Per `docs/12` §1 Rule 2, an unrecognised codec on a
//! track you did not select must never fail the file — so unknown codecs are representable, not
//! parse errors.

/// Any codec, used where a stream's kind is not statically known.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Codec {
    Video(VideoCodec),
    Audio(AudioCodec),
    Subtitle(SubtitleCodec),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoCodec {
    H264,
    Hevc,
    Vvc,
    Av1,
    Vp8,
    Vp9,
    Mpeg1,
    Mpeg2,
    Mpeg4Part2,
    Vc1,
    Theora,
    ProRes,
    ProResRaw,
    DnxHd,
    Apv,
    Ffv1,
    Mjpeg,
    Dv,
    Uncompressed,
    /// ITU-T H.263 — pre-AVC videoconferencing and early mobile video (3GP).
    H263,
    /// Cinepak — early-90s CD-ROM video, still turns up in archival AVI/MOV rips.
    Cinepak,
    /// Indeo (3/4/5) — legacy AVI codec, same era and use case as Cinepak.
    Indeo,
    /// Sorenson Video 3 — the codec early iPod-era QuickTime `.mov` files were commonly encoded
    /// with, distinct from the Sorenson Spark (`flv1`) used in early Flash video.
    Svq3,
    /// QuickTime Animation / RLE — ubiquitous in screen-recording `.mov` files, not a mastering
    /// format despite being lossless.
    QtRle,
    /// Ut Video — a lossless intermediate codec, the open-source alternative to the commercial
    /// mastering formats already modelled here.
    UtVideo,
    Other(String),
}

impl VideoCodec {
    /// Codecs with essentially no hardware decoder coverage in the profiles that matter, or none at
    /// all. Informs the "expect software decode" path in `docs/11` §8 — not a correctness gate.
    pub fn is_typically_software_only(&self) -> bool {
        matches!(
            self,
            Self::Vc1
                | Self::Theora
                | Self::ProRes
                | Self::ProResRaw
                | Self::DnxHd
                | Self::Apv
                | Self::Ffv1
                | Self::Mjpeg
                | Self::Dv
                | Self::Uncompressed
                | Self::Mpeg1
                | Self::Mpeg4Part2
                | Self::H263
                | Self::Cinepak
                | Self::Indeo
                | Self::Svq3
                | Self::QtRle
                | Self::UtVideo
                | Self::Other(_)
        )
    }

    /// Intra-only / mastering-grade codecs. These reach hundreds of Mbps and are I/O-bound rather
    /// than decode-bound (`docs/11` §6.4).
    pub fn is_intermediate(&self) -> bool {
        matches!(
            self,
            Self::ProRes
                | Self::ProResRaw
                | Self::DnxHd
                | Self::Apv
                | Self::Ffv1
                | Self::Uncompressed
                | Self::UtVideo
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioCodec {
    // Lossless / HD — the remux-critical set (docs/11 §4.1)
    TrueHd,
    DtsHdMa,
    DtsX,
    DtsHdHra,
    Flac,
    Alac,
    WavPack,
    MonkeysAudio,
    Pcm,
    Dsd,
    /// Windows Media Audio 9 Lossless — bit-exact, unlike the lossy WMA family it is otherwise
    /// grouped with by name.
    WmaLossless,
    // Lossy
    Ac3,
    EAc3,
    Ac4,
    Dts,
    Aac,
    Opus,
    Vorbis,
    Mp3,
    Mp2,
    Wma,
    /// Adaptive Differential PCM — the near-universal audio codec in legacy game FMVs and camcorder
    /// AVI files (`ms-adpcm`, `ima-adpcm`, and the format's many other container-specific variants).
    Adpcm,
    Other(String),
}

impl AudioCodec {
    /// True when the codec reproduces the source bit-exactly on decode.
    pub fn is_lossless(&self) -> bool {
        matches!(
            self,
            Self::TrueHd
                | Self::DtsHdMa
                | Self::Flac
                | Self::Alac
                | Self::WavPack
                | Self::MonkeysAudio
                | Self::Pcm
                | Self::Dsd
                | Self::WmaLossless
        )
    }

    /// Codecs that require IEC 61937 HBR encapsulation to bitstream, and so only pass through to a
    /// sink that explicitly advertises them (`docs/03` §5.4).
    pub fn requires_hbr_passthrough(&self) -> bool {
        matches!(self, Self::TrueHd | Self::DtsHdMa | Self::DtsX | Self::DtsHdHra)
    }

    /// Carries object-based spatial audio that is lost on decode to channel-based LPCM.
    pub fn may_carry_objects(&self) -> bool {
        matches!(self, Self::TrueHd | Self::DtsHdMa | Self::DtsX | Self::EAc3 | Self::Ac4)
    }

    /// The embedded lossy core that can be *extracted* — original bitstream, no re-encode — when the
    /// sink cannot take the full stream. See `docs/13` §4.
    ///
    /// TrueHD's AC-3 core is only present in Blu-ray-authored streams; callers must confirm against
    /// the actual bitstream before relying on it, which is why this is advisory.
    pub fn extractable_core(&self) -> Option<AudioCodec> {
        match self {
            Self::DtsHdMa | Self::DtsX | Self::DtsHdHra => Some(Self::Dts),
            Self::TrueHd => Some(Self::Ac3),
            _ => None,
        }
    }
}

/// A still image, distinct from [`VideoCodec`] even where the underlying compression coincides
/// (`Mjpeg` is a video codec because it is a sequence of frames; a JPEG cover embedded as a
/// Matroska attachment, an MP4 `covr` atom, or an ID3 `APIC` frame is one image, never decoded
/// through the video pipeline at all). Conflating the two would make an embedded cover show up as
/// a spurious video track.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageCodec {
    Jpeg,
    Png,
    Gif,
    Bmp,
    WebP,
    Other(String),
}

impl ImageCodec {
    /// Guess from a filename extension, the same "never trust the declared MIME type" stance
    /// `docs/12` §2.7 already takes for font attachments — muxers get MIME wrong constantly, but an
    /// extension is what the person who embedded the file actually chose.
    pub fn from_extension(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let ext = lower.rsplit('.').next()?;
        Some(match ext {
            "jpg" | "jpeg" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "gif" => Self::Gif,
            "bmp" => Self::Bmp,
            "webp" => Self::WebP,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SubtitleCodec {
    // Text
    SubRip,
    Ass,
    WebVtt,
    Ttml,
    MicroDvd,
    SubViewer,
    /// MP4 timed text (`tx3g`) — plain text with basic styling, distinct from SRT/ASS.
    MovText,
    // Bitmap
    Pgs,
    VobSub,
    DvbSub,
    // Caption
    Cea608,
    Cea708,
    Teletext,
    Other(String),
}

impl SubtitleCodec {
    pub fn is_bitmap(&self) -> bool {
        matches!(self, Self::Pgs | Self::VobSub | Self::DvbSub)
    }

    /// Captions embedded in the video elementary stream or as a side channel; they travel with the
    /// video and need no separate delivery (`docs/13` §5).
    pub fn is_in_band_caption(&self) -> bool {
        matches!(self, Self::Cea608 | Self::Cea708 | Self::Teletext)
    }

    /// Carries styling/positioning that is lost on conversion to a plain text format.
    pub fn is_styled(&self) -> bool {
        matches!(self, Self::Ass | Self::Ttml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_set_is_coherent() {
        assert!(AudioCodec::TrueHd.is_lossless());
        assert!(AudioCodec::DtsHdMa.is_lossless());
        assert!(!AudioCodec::Ac3.is_lossless());
        assert!(!AudioCodec::Aac.is_lossless());
    }

    #[test]
    fn wma_lossless_is_lossless_unlike_the_rest_of_the_wma_family() {
        // Grouping WMA Lossless under the lossy `Wma` variant would misreport a bit-exact track as
        // one that must be treated like a lossy source (e.g. never trusted as a remux's "safe" audio).
        assert!(AudioCodec::WmaLossless.is_lossless());
        assert!(!AudioCodec::Wma.is_lossless());
    }

    #[test]
    fn legacy_video_codecs_are_recognised_as_software_only() {
        for c in [
            VideoCodec::H263,
            VideoCodec::Cinepak,
            VideoCodec::Indeo,
            VideoCodec::Svq3,
            VideoCodec::QtRle,
            VideoCodec::UtVideo,
        ] {
            assert!(c.is_typically_software_only(), "{c:?}");
        }
    }

    #[test]
    fn ut_video_is_intermediate_but_qtrle_is_not() {
        // Both are lossless, but Ut Video is a mastering-grade intermediate codec while QuickTime
        // Animation/RLE is a legacy screen-capture format -- treating the latter as intermediate
        // would misclassify what is usually a small, low-bitrate file.
        assert!(VideoCodec::UtVideo.is_intermediate());
        assert!(!VideoCodec::QtRle.is_intermediate());
    }

    #[test]
    fn hbr_codecs_are_exactly_the_hd_bitstream_formats() {
        for c in [AudioCodec::TrueHd, AudioCodec::DtsHdMa, AudioCodec::DtsX, AudioCodec::DtsHdHra] {
            assert!(c.requires_hbr_passthrough(), "{c:?}");
        }
        // E-AC-3 JOC carries Atmos but fits in standard IEC 61937 — it must NOT be gated on HBR,
        // or Atmos-over-E-AC3 (the only Atmos path on tvOS) is wrongly rejected. docs/13 §4.
        assert!(!AudioCodec::EAc3.requires_hbr_passthrough());
        assert!(AudioCodec::EAc3.may_carry_objects());
    }

    #[test]
    fn core_extraction_targets_are_lossy_and_not_self_referential() {
        for c in [AudioCodec::DtsHdMa, AudioCodec::DtsX, AudioCodec::TrueHd] {
            let core = c.extractable_core().expect("has a core");
            assert!(!core.is_lossless(), "{c:?} core {core:?} must be lossy");
            assert_ne!(core, c);
            assert_eq!(core.extractable_core(), None, "cores must not chain");
        }
        assert_eq!(AudioCodec::Flac.extractable_core(), None);
    }

    #[test]
    fn image_codec_is_guessed_from_the_extension_case_insensitively() {
        assert_eq!(ImageCodec::from_extension("cover.jpg"), Some(ImageCodec::Jpeg));
        assert_eq!(ImageCodec::from_extension("COVER.JPEG"), Some(ImageCodec::Jpeg));
        assert_eq!(ImageCodec::from_extension("folder.png"), Some(ImageCodec::Png));
        assert_eq!(ImageCodec::from_extension("art.webp"), Some(ImageCodec::WebP));
        assert_eq!(ImageCodec::from_extension("font.ttf"), None, "not an image extension");
        assert_eq!(ImageCodec::from_extension("no-extension"), None);
    }

    #[test]
    fn unknown_codecs_are_representable_not_errors() {
        // docs/12 §1 Rule 2: unknown is not fatal.
        let v = VideoCodec::Other("V_PRIVATE/EXPERIMENT".into());
        assert!(v.is_typically_software_only());
        let s = SubtitleCodec::Other("S_WEIRD".into());
        assert!(!s.is_bitmap() && !s.is_in_band_caption());
    }
}
