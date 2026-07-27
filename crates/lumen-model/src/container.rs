//! Container identity and the codec-carriage rules that drive remux legality.
//!
//! The matrix here is the executable form of `docs/13` §1. Getting a cell wrong produces either a
//! failed remux at playback time or a needless transcode, so it is tested directly.

use crate::codec::{AudioCodec, SubtitleCodec, VideoCodec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Container {
    Matroska,
    WebM,
    Mp4,
    /// Fragmented MP4 / CMAF — the segment format for both LL-HLS and DASH (`docs/13` §6).
    FragmentedMp4,
    MpegTs,
    MpegPs,
    Avi,
    Asf,
    Flv,
    Ogg,
    /// Headerless elementary stream — the rung-5 recovery target (`docs/12` §5).
    RawElementaryStream,
    /// Disc structure: BDMV, VIDEO_TS, or an ISO image.
    DiscStructure,
}

impl Container {
    /// Matroska accepts every codec Lumen supports. This is why MKV-capable clients reach T0/T1 far
    /// more often than browser-based ones (`docs/13` §1.1) and why it is the preferred remux target.
    pub fn is_universal(self) -> bool {
        matches!(self, Self::Matroska)
    }

    pub fn accepts_video(self, codec: &VideoCodec) -> bool {
        use VideoCodec as V;
        match self {
            Self::Matroska => true,
            Self::WebM => matches!(codec, V::Vp8 | V::Vp9 | V::Av1),
            Self::Mp4 | Self::FragmentedMp4 => {
                !matches!(codec, V::Vc1 | V::Uncompressed | V::Other(_))
            }
            Self::MpegTs => {
                matches!(codec, V::H264 | V::Hevc | V::Mpeg1 | V::Mpeg2 | V::Vc1 | V::Vvc)
            }
            Self::MpegPs => matches!(codec, V::Mpeg1 | V::Mpeg2 | V::H264),
            Self::Avi => !matches!(codec, V::Hevc | V::Av1 | V::Vvc | V::ProResRaw),
            Self::Asf => matches!(codec, V::Mpeg4Part2 | V::Vc1 | V::Other(_)),
            Self::Flv => matches!(codec, V::H264),
            Self::Ogg => matches!(codec, V::Theora | V::Vp8),
            Self::RawElementaryStream | Self::DiscStructure => false,
        }
    }

    pub fn accepts_audio(self, codec: &AudioCodec) -> bool {
        use AudioCodec as A;
        match self {
            Self::Matroska => true,
            Self::WebM => matches!(codec, A::Opus | A::Vorbis),
            // TrueHD (`mlpa`) and the DTS family are legal in MP4 but poorly supported by third
            // party clients — legality is modelled here, client support is modelled in lumen-caps.
            Self::Mp4 | Self::FragmentedMp4 => {
                !matches!(codec, A::Dsd | A::MonkeysAudio | A::WavPack | A::Other(_))
            }
            Self::MpegTs => matches!(
                codec,
                A::Aac
                    | A::Ac3
                    | A::EAc3
                    | A::Ac4
                    | A::Dts
                    | A::DtsHdMa
                    | A::Mp2
                    | A::Mp3
                    | A::TrueHd
            ),
            Self::MpegPs => matches!(codec, A::Ac3 | A::Dts | A::Mp2 | A::Pcm),
            Self::Avi => matches!(codec, A::Mp3 | A::Ac3 | A::Dts | A::Pcm | A::Aac),
            Self::Asf => matches!(codec, A::Wma | A::Mp3 | A::Other(_)),
            Self::Flv => matches!(codec, A::Aac | A::Mp3),
            Self::Ogg => matches!(codec, A::Vorbis | A::Opus | A::Flac),
            Self::RawElementaryStream | Self::DiscStructure => false,
        }
    }

    pub fn accepts_subtitle(self, codec: &SubtitleCodec) -> bool {
        use SubtitleCodec as S;
        // In-band captions ride inside the video stream, so container rules do not apply.
        if codec.is_in_band_caption() {
            return self.accepts_video(&VideoCodec::H264);
        }
        match self {
            Self::Matroska => true,
            Self::WebM => matches!(codec, S::WebVtt),
            // ASS and PGS cannot enter MP4. This single fact drives the entire out-of-band subtitle
            // ladder in `docs/13` §5 — burning in is the failure mode it exists to avoid.
            Self::Mp4 | Self::FragmentedMp4 => matches!(codec, S::WebVtt | S::Ttml | S::SubRip),
            Self::MpegTs => matches!(codec, S::DvbSub | S::Pgs),
            Self::MpegPs => matches!(codec, S::VobSub),
            Self::Avi | Self::Asf | Self::Flv | Self::Ogg => false,
            Self::RawElementaryStream | Self::DiscStructure => false,
        }
    }

    /// Remux preference order from `docs/13` §2: fewest carriage constraints first.
    pub fn remux_preference(self) -> u8 {
        match self {
            Self::Matroska => 0,
            Self::FragmentedMp4 => 1,
            Self::Mp4 => 2,
            Self::MpegTs => 3,
            Self::WebM => 4,
            _ => 250,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matroska_accepts_everything_we_support() {
        for v in [
            VideoCodec::Hevc,
            VideoCodec::Vc1,
            VideoCodec::ProResRaw,
            VideoCodec::Other("x".into()),
        ] {
            assert!(Container::Matroska.accepts_video(&v), "{v:?}");
        }
        for a in
            [AudioCodec::TrueHd, AudioCodec::DtsX, AudioCodec::Dsd, AudioCodec::Other("y".into())]
        {
            assert!(Container::Matroska.accepts_audio(&a), "{a:?}");
        }
        for s in [SubtitleCodec::Ass, SubtitleCodec::Pgs, SubtitleCodec::Other("z".into())] {
            assert!(Container::Matroska.accepts_subtitle(&s), "{s:?}");
        }
        assert!(Container::Matroska.is_universal());
    }

    #[test]
    fn mp4_rejects_ass_and_pgs() {
        // The constraint that makes out-of-band subtitle delivery mandatory rather than optional.
        assert!(!Container::Mp4.accepts_subtitle(&SubtitleCodec::Ass));
        assert!(!Container::Mp4.accepts_subtitle(&SubtitleCodec::Pgs));
        assert!(!Container::FragmentedMp4.accepts_subtitle(&SubtitleCodec::VobSub));
        assert!(Container::Mp4.accepts_subtitle(&SubtitleCodec::WebVtt));
    }

    #[test]
    fn webm_is_a_narrow_subset() {
        assert!(Container::WebM.accepts_video(&VideoCodec::Av1));
        assert!(!Container::WebM.accepts_video(&VideoCodec::H264));
        assert!(Container::WebM.accepts_audio(&AudioCodec::Opus));
        assert!(!Container::WebM.accepts_audio(&AudioCodec::Aac));
    }

    #[test]
    fn matroska_is_the_top_remux_preference() {
        let mut all = [
            Container::Mp4,
            Container::MpegTs,
            Container::Matroska,
            Container::WebM,
            Container::FragmentedMp4,
        ];
        all.sort_by_key(|c| c.remux_preference());
        assert_eq!(all[0], Container::Matroska);
    }
}
