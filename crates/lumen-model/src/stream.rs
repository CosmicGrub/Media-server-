//! Stream descriptions produced by the Probe stage (`docs/05` §4.3).

use crate::Language;
use crate::codec::{AudioCodec, SubtitleCodec, VideoCodec};
use crate::color::ColorInfo;

/// Exact rational, used for frame rates and time bases.
///
/// `docs/12` §6: frame rates stay rational from demux to render. Rounding 24000/1001 to 23.976
/// accumulates roughly one frame of drift every 40 minutes, which users report as "audio drifts out
/// of sync on long films".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub const NTSC_FILM: Self = Self::new(24_000, 1_001);
    pub const NTSC_VIDEO: Self = Self::new(30_000, 1_001);
    pub const PAL: Self = Self::new(25, 1);

    /// Lossy conversion, for display only. Never use the result for timing arithmetic.
    pub fn as_f64(self) -> f64 {
        if self.den == 0 {
            return 0.0;
        }
        f64::from(self.num) / f64::from(self.den)
    }

    pub fn is_valid(self) -> bool {
        self.den != 0 && self.num != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum FieldOrder {
    #[default]
    Progressive,
    TopFieldFirst,
    BottomFieldFirst,
    /// Interlaced with an order the container did not state — must be detected from the bitstream
    /// rather than assumed TFF (`docs/13` §3).
    UnknownInterlaced,
}

impl FieldOrder {
    pub fn is_interlaced(self) -> bool {
        !matches!(self, Self::Progressive)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum StereoMode {
    #[default]
    Mono,
    SideBySide,
    TopAndBottom,
    FramePacked,
    /// H.264 MVC — Blu-ray 3D. The base view must decode even when the dependent view cannot.
    Mvc,
}

/// Chroma sample density relative to luma, in ascending order of what a decoder must support.
///
/// Ordered deliberately: hardware decoders that handle a given level almost always handle every
/// level below it too (a 4:4:4-capable decoder decodes 4:2:0 trivially), so a single `max_chroma`
/// ceiling on [`crate::VideoDecodeCaps`]-shaped types can be compared with `<=` rather than needing
/// an exhaustive support list. `docs/11` §8 notes 4:2:2/4:4:4 profiles (H.264 High 4:2:2/4:4:4
/// Predictive, HEVC Rext) as the case hardware decoders most often lack entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[non_exhaustive]
pub enum ChromaSubsampling {
    /// Quarter chroma resolution — the overwhelming majority of consumer video.
    #[default]
    Yuv420,
    /// Half-horizontal chroma resolution — broadcast and professional mezzanine formats.
    Yuv422,
    /// Full chroma resolution — screen recordings, professional masters, rarely hardware-decoded.
    Yuv444,
}

/// Pixels to discard from each edge before display, in decoded-frame coordinates. Distinct from
/// [`Rational`] sample-aspect scaling: crop removes rows/columns the encoder padded in (macroblock
/// alignment padding, cropped-for-broadcast masters), while SAR reshapes the pixels that remain.
/// Applying SAR before crop -- or not applying crop at all -- misreports the displayed geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CropRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl CropRect {
    pub fn is_zero(self) -> bool {
        self == Self::default()
    }
}

/// Cadence used to store a different native frame rate inside a fixed-rate container. Detecting
/// this matters because naively deinterlacing or frame-rate-converting pulled-down content
/// re-derives frames that were never independently captured, producing visible judder or ghosting
/// that careful handling of the original cadence would avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum TelecinePattern {
    #[default]
    None,
    /// NTSC 3:2 pulldown -- 24fps film stored as 60 fields/29.97fps video.
    Pulldown32,
    /// PAL speedup -- 24fps film played at 25fps with no field repetition, a matching ~4% audio
    /// pitch shift.
    PalSpeedup,
}

/// Track flags. Matroska carries all of these; MP4 and TS carry a subset.
///
/// `docs/12` §4 — these drive automatic track selection, and getting selection wrong reads to users
/// as a compatibility failure even when every stream decoded correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamFlags {
    pub default: bool,
    pub forced: bool,
    pub enabled_default_true: bool,
    pub hearing_impaired: bool,
    pub visual_impaired: bool,
    pub original: bool,
    pub commentary: bool,
}

impl StreamFlags {
    pub fn enabled() -> Self {
        Self { enabled_default_true: true, ..Default::default() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoStream {
    pub index: u32,
    pub codec: VideoCodec,
    pub profile: Option<String>,
    pub level: Option<u16>,
    pub width: u32,
    pub height: u32,
    /// Sample aspect ratio. Anything other than 1:1 is anamorphic and must be honoured on display.
    pub sample_aspect: Rational,
    pub frame_rate: Option<Rational>,
    pub bit_depth: u8,
    pub color: ColorInfo,
    pub field_order: FieldOrder,
    pub stereo_mode: StereoMode,
    pub bitrate_bps: Option<u64>,
    pub flags: StreamFlags,
    /// Edge pixels to discard before display. Zero (the default) means the full decoded frame is
    /// shown -- most streams carry no crop.
    pub crop: CropRect,
    /// Film cadence hidden inside a fixed-rate container, if detected.
    pub telecine: TelecinePattern,
    pub chroma: ChromaSubsampling,
}

impl VideoStream {
    /// Display dimensions after applying crop and sample aspect, in that order: crop removes
    /// encoder padding from the decoded frame, and only then does SAR reshape what remains.
    /// Ignoring SAR shows anamorphic DVD content at 3:2 instead of 16:9 (`docs/11` §6.1); ignoring
    /// crop shows the padding the encoder never meant to display.
    pub fn display_size(&self) -> (u32, u32) {
        // `saturating_add`, not `+`: crop values are meant to come from a probed container's own
        // crop atoms eventually (`PixelCropLeft`/`PixelCropRight` and friends), the same kind of
        // attacker-controlled numeric field this codebase never trusts to be sane on its own --
        // `left + right` panics outright on overflow in a debug build (`cargo test`'s own default
        // profile) the moment two crop values sum past `u32::MAX`, and silently wraps to a small
        // number in release instead. `saturating_sub` right after this already treats the *width*
        // side defensively; the crop-value addition feeding it deserves the same treatment.
        let cropped_w =
            self.width.saturating_sub(self.crop.left.saturating_add(self.crop.right)).max(1);
        let cropped_h =
            self.height.saturating_sub(self.crop.top.saturating_add(self.crop.bottom)).max(1);
        if !self.sample_aspect.is_valid() || self.sample_aspect.num == self.sample_aspect.den {
            return (cropped_w, cropped_h);
        }
        let w = (u64::from(cropped_w) * u64::from(self.sample_aspect.num))
            / u64::from(self.sample_aspect.den);
        (w.max(1) as u32, cropped_h)
    }

    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Frame rate is absent on true VFR streams (no Matroska `DefaultDuration`, irregular `stts`).
    /// Such streams must play as VFR rather than being "corrected" to CFR (`docs/11` §6.2).
    pub fn is_variable_frame_rate(&self) -> bool {
        self.frame_rate.is_none_or(|r| !r.is_valid())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChannelLayout {
    pub channels: u8,
    /// Low-frequency effects channel present. Tracked separately because downmix matrices treat it
    /// differently from the full-range channels.
    pub lfe: bool,
}

impl ChannelLayout {
    pub const STEREO: Self = Self { channels: 2, lfe: false };
    pub const SURROUND_5_1: Self = Self { channels: 6, lfe: true };
    pub const SURROUND_7_1: Self = Self { channels: 8, lfe: true };

    pub const fn new(channels: u8) -> Self {
        Self { channels, lfe: channels >= 6 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub index: u32,
    pub codec: AudioCodec,
    pub layout: ChannelLayout,
    pub sample_rate: u32,
    pub bit_depth: Option<u8>,
    pub bitrate_bps: Option<u64>,
    pub language: Language,
    pub title: Option<String>,
    pub flags: StreamFlags,
    /// Object-based extension detected in the bitstream: Atmos in TrueHD/E-AC-3 JOC, DTS:X in an MA
    /// substream. Lost when decoded to channel-based LPCM.
    pub has_objects: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleStream {
    pub index: u32,
    pub codec: SubtitleCodec,
    pub language: Language,
    pub title: Option<String>,
    pub flags: StreamFlags,
    /// Subtitle carried outside the media file, discovered by sidecar naming (`docs/11` §5).
    pub external: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(w: u32, h: u32, sar: Rational) -> VideoStream {
        VideoStream {
            index: 0,
            codec: VideoCodec::H264,
            profile: None,
            level: None,
            width: w,
            height: h,
            sample_aspect: sar,
            frame_rate: Some(Rational::NTSC_FILM),
            bit_depth: 8,
            color: ColorInfo::default(),
            field_order: FieldOrder::Progressive,
            stereo_mode: StereoMode::Mono,
            bitrate_bps: None,
            flags: StreamFlags::enabled(),
            crop: CropRect::default(),
            telecine: TelecinePattern::default(),
            chroma: ChromaSubsampling::default(),
        }
    }

    #[test]
    fn anamorphic_dvd_displays_as_16_9() {
        // 720x480 NTSC with 32:27 SAR is the canonical anamorphic case from docs/11 §6.1.
        let s = video(720, 480, Rational::new(32, 27));
        let (w, h) = s.display_size();
        let dar = f64::from(w) / f64::from(h);
        assert!((dar - 16.0 / 9.0).abs() < 0.02, "expected ~1.778, got {dar}");
    }

    #[test]
    fn square_pixels_pass_through_unchanged() {
        let s = video(1920, 1080, Rational::new(1, 1));
        assert_eq!(s.display_size(), (1920, 1080));
    }

    #[test]
    fn invalid_sar_does_not_divide_by_zero() {
        let s = video(1280, 720, Rational::new(1, 0));
        assert_eq!(s.display_size(), (1280, 720));
    }

    #[test]
    fn ntsc_film_rate_stays_exact() {
        let r = Rational::NTSC_FILM;
        assert_eq!((r.num, r.den), (24_000, 1_001));
        // The rounded value must never be used for timing; assert it is genuinely not 23.976 exactly.
        assert!((r.as_f64() - 23.976).abs() > 1e-6);
    }

    #[test]
    fn missing_frame_rate_means_vfr() {
        let mut s = video(1920, 1080, Rational::new(1, 1));
        assert!(!s.is_variable_frame_rate());
        s.frame_rate = None;
        assert!(s.is_variable_frame_rate());
        s.frame_rate = Some(Rational::new(0, 1));
        assert!(s.is_variable_frame_rate());
    }

    #[test]
    fn zero_crop_is_a_no_op() {
        let s = video(1920, 1080, Rational::new(1, 1));
        assert!(s.crop.is_zero());
        assert_eq!(s.display_size(), (1920, 1080));
    }

    #[test]
    fn crop_is_applied_before_sample_aspect() {
        // 1920x1080 with 8px letterboxing top and bottom cropped away, then scaled by a 4:3 SAR --
        // crop must land on the pre-scale width/height, not the display width/height.
        let mut s = video(1920, 1080, Rational::new(4, 3));
        s.crop = CropRect { left: 0, top: 8, right: 0, bottom: 8 };
        let (w, h) = s.display_size();
        assert_eq!(h, 1064, "vertical crop must reduce the cropped height, not the scaled width");
        assert_eq!(w, (1920u64 * 4 / 3) as u32);
    }

    #[test]
    fn crop_wider_than_the_frame_clamps_to_one_pixel_instead_of_underflowing() {
        let mut s = video(100, 100, Rational::new(1, 1));
        s.crop = CropRect { left: 60, top: 0, right: 60, bottom: 0 };
        assert_eq!(s.display_size(), (1, 100));
    }

    #[test]
    fn crop_values_that_would_overflow_u32_clamp_rather_than_panicking() {
        // Crop values are meant to eventually come from a probed container's own crop atoms --
        // exactly the kind of attacker/corruption-controlled numeric field this codebase never
        // trusts to be sane. `left + right` used to be a plain addition, which panicked outright
        // in a debug build (`cargo test`'s own default profile) the moment two crop values summed
        // past `u32::MAX`, rather than degrading the same way an over-wide crop already does above.
        let mut s = video(100, 100, Rational::new(1, 1));
        s.crop = CropRect { left: u32::MAX, top: u32::MAX, right: u32::MAX, bottom: u32::MAX };
        assert_eq!(s.display_size(), (1, 1));
    }

    #[test]
    fn telecine_defaults_to_none() {
        let s = video(1920, 1080, Rational::new(1, 1));
        assert_eq!(s.telecine, TelecinePattern::None);
    }

    #[test]
    fn chroma_subsampling_orders_from_least_to_most_demanding() {
        assert!(ChromaSubsampling::Yuv420 < ChromaSubsampling::Yuv422);
        assert!(ChromaSubsampling::Yuv422 < ChromaSubsampling::Yuv444);
        assert_eq!(ChromaSubsampling::default(), ChromaSubsampling::Yuv420);
    }
}
