//! Colour, transfer, and HDR description.
//!
//! `docs/11` §6.3. Range mishandling is the single most common cause of "washed out" and "crushed
//! blacks" reports, and HDR format detection determines whether tone mapping is needed at all.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ColorPrimaries {
    #[default]
    Unspecified,
    Bt709,
    Bt601_525,
    Bt601_625,
    Bt2020,
    DciP3,
    DisplayP3,
    Smpte240M,
}

impl ColorPrimaries {
    /// Whether `self` (a stream's mastering primaries) sits entirely inside `display`'s gamut, so
    /// every colour the content specifies is reproducible without out-of-gamut clipping.
    ///
    /// This is the standard nesting every HDR display spec sheet documents: BT.2020 is a superset of
    /// DCI-P3, which is a superset of the narrower standard-gamut set (BT.709/BT.601/SMPTE 240M, all
    /// close enough in practice to be interchangeable for this purpose). `Unspecified` on *either*
    /// side resolves to BT.709 before comparing -- the same "untagged is the narrow, common case"
    /// reasoning [`ColorRange::or_default_for_yuv`] already applies to range: the overwhelming
    /// majority of untagged content genuinely is BT.709, and assuming a display's gamut is at least
    /// that wide when unconfirmed is the safe direction to be wrong in, exactly mirroring why
    /// untagged range defaults to limited rather than full.
    pub fn is_covered_by(self, display: Self) -> bool {
        use ColorPrimaries::*;
        let rank = |p: Self| match p {
            Bt2020 => 3,
            DciP3 | DisplayP3 => 2,
            Bt709 | Bt601_525 | Bt601_625 | Smpte240M | Unspecified => 1,
        };
        rank(self) <= rank(display)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ColorTransfer {
    #[default]
    Unspecified,
    Bt709,
    Bt601,
    Smpte170M,
    Smpte240M,
    Linear,
    Srgb,
    Bt2020_10,
    Bt2020_12,
    /// SMPTE ST 2084 — perceptual quantizer, the HDR10/DV transfer.
    Pq,
    /// ARIB STD-B67 — hybrid log-gamma.
    Hlg,
}

impl ColorTransfer {
    pub fn is_hdr(self) -> bool {
        matches!(self, Self::Pq | Self::Hlg)
    }
}

/// The coefficients used to convert YUV to RGB. A distinct dimension from [`ColorPrimaries`]/
/// [`ColorTransfer`] -- getting this wrong produces the same class of visible colour-shift defect
/// range mishandling does (`docs/11` §6.3 groups them together for exactly that reason), but nothing
/// about knowing a stream's primaries or transfer function tells a decoder which matrix to invert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ColorMatrix {
    #[default]
    Unspecified,
    Bt709,
    Bt601,
    /// Non-constant-luminance YCgCo, used by some screen-capture and lossless encodes.
    YCgCo,
    /// BT.2020 non-constant luminance -- the common case for HDR content.
    Bt2020Ncl,
    /// BT.2020 constant luminance -- rare, but a real, distinct matrix from NCL.
    Bt2020Cl,
    /// ICtCp, the matrix Dolby Vision and some HDR mastering pipelines use in place of BT.2020.
    IcTcP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorRange {
    #[default]
    Unspecified,
    /// 16–235 (8-bit) — the default assumption for YUV video when untagged.
    Limited,
    /// 0–255.
    Full,
}

impl ColorRange {
    /// Untagged YUV is limited range far more often than not; guessing full range on untagged
    /// content crushes blacks and blows highlights.
    pub fn or_default_for_yuv(self) -> ColorRange {
        match self {
            Self::Unspecified => Self::Limited,
            other => other,
        }
    }
}

/// SMPTE ST 2086 mastering display metadata plus CTA-861.3 light levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MasteringDisplay {
    pub max_luminance_nits: u32,
    pub min_luminance_millinits: u32,
    pub max_cll: u16,
    pub max_fall: u16,
}

/// The HDR system in use, in ascending order of what a renderer must understand.
///
/// Dolby Vision profiles are distinguished because their handling differs materially: 8.1 and 7 have
/// an HDR10-compatible base layer that can simply be remuxed for a non-DV client, while 5 does not.
/// See `docs/03` §4.1 and `docs/11` §6.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum HdrFormat {
    #[default]
    Sdr,
    Hdr10,
    Hlg,
    /// HDR10 base plus SMPTE ST 2094-40 dynamic metadata.
    Hdr10Plus,
    /// Single-layer, IPTPQc2. No HDR10-compatible base layer.
    DolbyVisionP5,
    /// Single-layer with an HDR10- or HLG-compatible base layer.
    DolbyVisionP8,
    /// Dual-layer, minimal enhancement layer. The EL carries no picture detail, so base-layer
    /// playback is the correct and complete outcome.
    DolbyVisionP7Mel,
    /// Dual-layer, full enhancement layer. The EL carries real additional luma/chroma detail that no
    /// open-source renderer can reconstruct; base-layer HDR10 playback with honest labelling is the
    /// specified behaviour, not a claim of support.
    DolbyVisionP7Fel,
}

impl HdrFormat {
    pub fn is_hdr(self) -> bool {
        !matches!(self, Self::Sdr)
    }

    /// True when a client that understands only HDR10 can play the stream by taking the base layer,
    /// with no transcode — the difference between T1 and T3 for Dolby Vision content.
    pub fn has_hdr10_compatible_base(self) -> bool {
        matches!(
            self,
            Self::Hdr10
                | Self::Hdr10Plus
                | Self::DolbyVisionP8
                | Self::DolbyVisionP7Mel
                | Self::DolbyVisionP7Fel
        )
    }

    /// True when reproduction is necessarily incomplete even on a fully capable HDR display.
    pub fn is_lossy_to_reproduce(self) -> bool {
        matches!(self, Self::DolbyVisionP7Fel)
    }

    pub fn is_dolby_vision(self) -> bool {
        matches!(
            self,
            Self::DolbyVisionP5
                | Self::DolbyVisionP8
                | Self::DolbyVisionP7Mel
                | Self::DolbyVisionP7Fel
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorInfo {
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
    pub matrix: ColorMatrix,
    pub range: ColorRange,
    pub hdr: HdrFormat,
    pub mastering: Option<MasteringDisplay>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_yuv_range_defaults_to_limited() {
        assert_eq!(ColorRange::Unspecified.or_default_for_yuv(), ColorRange::Limited);
        assert_eq!(ColorRange::Full.or_default_for_yuv(), ColorRange::Full);
    }

    #[test]
    fn wider_gamuts_nest_the_narrower_ones() {
        assert!(ColorPrimaries::Bt709.is_covered_by(ColorPrimaries::Bt2020));
        assert!(ColorPrimaries::DciP3.is_covered_by(ColorPrimaries::Bt2020));
        assert!(ColorPrimaries::Bt709.is_covered_by(ColorPrimaries::DciP3));
        assert!(
            !ColorPrimaries::Bt2020.is_covered_by(ColorPrimaries::DciP3),
            "P3 is not wide enough"
        );
        assert!(!ColorPrimaries::Bt2020.is_covered_by(ColorPrimaries::Bt709));
        assert!(
            ColorPrimaries::Bt709.is_covered_by(ColorPrimaries::Bt709),
            "a gamut covers itself"
        );
    }

    #[test]
    fn untagged_primaries_on_either_side_resolve_to_bt709_not_an_extreme() {
        // The overwhelming majority of untagged content genuinely is BT.709 -- treating unknown as
        // "covers nothing" would flag ordinary SDR files with no declared primaries as a gamut
        // mismatch against every display, including one explicitly built wide; treating it as "covers
        // everything" would hide a real BT.2020-on-a-narrow-display mismatch on the display side.
        assert!(ColorPrimaries::Unspecified.is_covered_by(ColorPrimaries::Bt709));
        assert!(ColorPrimaries::Unspecified.is_covered_by(ColorPrimaries::Bt2020));
        assert!(ColorPrimaries::Bt709.is_covered_by(ColorPrimaries::Unspecified));
        assert!(
            !ColorPrimaries::Bt2020.is_covered_by(ColorPrimaries::Unspecified),
            "an unconfirmed display gamut must not silently absorb real BT.2020 content"
        );
    }

    #[test]
    fn p5_is_the_only_dv_profile_without_an_hdr10_base() {
        assert!(!HdrFormat::DolbyVisionP5.has_hdr10_compatible_base());
        for f in
            [HdrFormat::DolbyVisionP8, HdrFormat::DolbyVisionP7Mel, HdrFormat::DolbyVisionP7Fel]
        {
            assert!(f.has_hdr10_compatible_base(), "{f:?}");
            assert!(f.is_dolby_vision());
        }
    }

    #[test]
    fn only_fel_is_flagged_as_lossy_to_reproduce() {
        assert!(HdrFormat::DolbyVisionP7Fel.is_lossy_to_reproduce());
        // MEL's enhancement layer carries no picture detail, so base-layer playback is complete.
        assert!(!HdrFormat::DolbyVisionP7Mel.is_lossy_to_reproduce());
        assert!(!HdrFormat::Hdr10Plus.is_lossy_to_reproduce());
    }

    #[test]
    fn color_matrix_defaults_to_unspecified() {
        assert_eq!(ColorMatrix::default(), ColorMatrix::Unspecified);
        assert_eq!(ColorInfo::default().matrix, ColorMatrix::Unspecified);
    }

    #[test]
    fn sdr_is_the_only_non_hdr_format() {
        assert!(!HdrFormat::Sdr.is_hdr());
        for f in [HdrFormat::Hdr10, HdrFormat::Hlg, HdrFormat::Hdr10Plus, HdrFormat::DolbyVisionP5]
        {
            assert!(f.is_hdr(), "{f:?}");
        }
    }
}
