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
    fn unknown_codecs_are_representable_not_errors() {
        // docs/12 §1 Rule 2: unknown is not fatal.
        let v = VideoCodec::Other("V_PRIVATE/EXPERIMENT".into());
        assert!(v.is_typically_software_only());
        let s = SubtitleCodec::Other("S_WEIRD".into());
        assert!(!s.is_bitmap() && !s.is_in_band_caption());
    }
}
