//! Capability model for a playback endpoint.
//!
//! The load-bearing idea, from `docs/01` gap G2 and `docs/05` §7: audio capability belongs to the
//! **current output sink**, not to the device and certainly not to the app. An Android TV box's
//! capabilities change the moment the user switches from TV speakers to an AVR, or plugs in
//! headphones. A static device profile — Jellyfin's model — is therefore wrong by construction.
//!
//! `AudioSinkCaps` is consequently a snapshot with a generation counter, refreshed on every device
//! change, and any cached playback decision keyed on it is invalidated when the generation moves.

#![forbid(unsafe_code)]

use lumen_model::{
    AudioCodec, ChannelLayout, Container, HdrFormat, Rational, SubtitleCodec, VideoCodec,
};

/// What a decoder can actually do for one codec.
///
/// Probed per `docs/11` §8: query the real profile/level/bit-depth/chroma support rather than
/// assuming, because a device that advertises "HEVC" usually means "HEVC Main 8-bit 4:2:0 only".
#[derive(Debug, Clone, PartialEq)]
pub struct VideoDecodeCaps {
    pub codec: VideoCodec,
    pub profiles: Vec<String>,
    pub max_level: Option<u16>,
    pub max_bit_depth: u8,
    pub max_width: u32,
    pub max_height: u32,
    pub max_bitrate_bps: Option<u64>,
    pub hardware: bool,
}

impl VideoDecodeCaps {
    /// Convenience constructor for a broadly capable hardware decoder.
    pub fn hardware(codec: VideoCodec, max_bit_depth: u8, max_width: u32, max_height: u32) -> Self {
        Self {
            codec,
            profiles: Vec::new(),
            max_level: None,
            max_bit_depth,
            max_width,
            max_height,
            max_bitrate_bps: None,
            hardware: true,
        }
    }

    /// An empty profile list means "no profile restriction known" rather than "no profiles
    /// supported" — probes on several platforms cannot enumerate profiles.
    pub fn accepts_profile(&self, profile: Option<&str>) -> bool {
        match (self.profiles.is_empty(), profile) {
            (true, _) | (_, None) => true,
            (false, Some(p)) => self.profiles.iter().any(|x| x.eq_ignore_ascii_case(p)),
        }
    }
}

/// Compressed formats a sink will accept as an IEC 61937 bitstream, plus its PCM limits.
///
/// Populated from a live probe of the *current* device: `AudioDeviceInfo.getEncodings()` on Android,
/// `IAudioClient::IsFormatSupported` on WASAPI, ELD parsing on ALSA, stream enumeration on
/// CoreAudio. See `docs/10` R12.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSinkCaps {
    pub device_name: String,
    /// Bumped on every device change. Any decision cached against this sink is invalid once it moves.
    pub generation: u64,
    /// Codecs accepted as a compressed bitstream. Empty means PCM-only.
    pub passthrough_encodings: Vec<AudioCodec>,
    pub max_pcm_channels: u8,
    pub pcm_sample_rates: Vec<u32>,
    pub max_pcm_bit_depth: u8,
    /// Exclusive/hog-mode access is obtainable, enabling bit-perfect output.
    pub exclusive_available: bool,
}

impl AudioSinkCaps {
    /// A plain stereo PCM sink: laptop speakers, Bluetooth, most browsers.
    pub fn stereo_pcm(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
            generation: 0,
            passthrough_encodings: Vec::new(),
            max_pcm_channels: 2,
            pcm_sample_rates: vec![44_100, 48_000],
            max_pcm_bit_depth: 16,
            exclusive_available: false,
        }
    }

    /// An HDMI sink that decodes the full HD bitstream set — the AVR case that makes T0 reachable.
    pub fn hd_avr(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
            generation: 0,
            passthrough_encodings: vec![
                AudioCodec::Ac3,
                AudioCodec::EAc3,
                AudioCodec::Dts,
                AudioCodec::DtsHdMa,
                AudioCodec::DtsX,
                AudioCodec::TrueHd,
            ],
            max_pcm_channels: 8,
            pcm_sample_rates: vec![44_100, 48_000, 96_000, 192_000],
            max_pcm_bit_depth: 24,
            exclusive_available: true,
        }
    }

    pub fn can_passthrough(&self, codec: &AudioCodec) -> bool {
        self.passthrough_encodings.contains(codec)
    }

    pub fn supports_sample_rate(&self, hz: u32) -> bool {
        self.pcm_sample_rates.contains(&hz)
    }

    /// Channels deliverable as PCM, capped by the sink.
    pub fn deliverable_channels(&self, layout: ChannelLayout) -> u8 {
        layout.channels.min(self.max_pcm_channels)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisplayCaps {
    pub width: u32,
    pub height: u32,
    pub hdr_formats: Vec<HdrFormat>,
    pub refresh_modes: Vec<Rational>,
    /// Display-mode switching for frame-rate matching (`docs/03` §4.2).
    pub can_switch_mode: bool,
}

impl DisplayCaps {
    pub fn sdr_1080p() -> Self {
        Self {
            width: 1920,
            height: 1080,
            hdr_formats: vec![HdrFormat::Sdr],
            refresh_modes: vec![Rational::new(60, 1)],
            can_switch_mode: false,
        }
    }

    pub fn hdr_4k() -> Self {
        Self {
            width: 3840,
            height: 2160,
            hdr_formats: vec![
                HdrFormat::Sdr,
                HdrFormat::Hdr10,
                HdrFormat::Hlg,
                HdrFormat::Hdr10Plus,
            ],
            refresh_modes: vec![
                Rational::NTSC_FILM,
                Rational::new(24, 1),
                Rational::PAL,
                Rational::new(60, 1),
            ],
            can_switch_mode: true,
        }
    }

    /// A display handles a stream's HDR without tone mapping either by supporting the format
    /// outright, or — for Dolby Vision with an HDR10-compatible base layer — by supporting HDR10.
    pub fn handles_hdr(&self, format: HdrFormat) -> bool {
        if self.hdr_formats.contains(&format) {
            return true;
        }
        match format {
            HdrFormat::Sdr => true,
            f if f.has_hdr10_compatible_base() => self.hdr_formats.contains(&HdrFormat::Hdr10),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleCaps {
    pub renderable: Vec<SubtitleCodec>,
    /// The client can fetch and render a subtitle delivered separately from the media stream. True
    /// for every Lumen native client; this is what makes burn-in avoidable (`docs/13` §5).
    pub accepts_out_of_band: bool,
}

impl SubtitleCaps {
    pub fn full() -> Self {
        Self {
            renderable: vec![
                SubtitleCodec::Ass,
                SubtitleCodec::SubRip,
                SubtitleCodec::WebVtt,
                SubtitleCodec::Pgs,
                SubtitleCodec::VobSub,
                SubtitleCodec::Cea608,
                SubtitleCodec::Cea708,
            ],
            accepts_out_of_band: true,
        }
    }

    pub fn text_only() -> Self {
        Self {
            renderable: vec![SubtitleCodec::WebVtt, SubtitleCodec::SubRip],
            accepts_out_of_band: true,
        }
    }

    pub fn can_render(&self, codec: &SubtitleCodec) -> bool {
        self.renderable.contains(codec)
    }
}

/// How much fidelity loss the user has authorised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscodePolicy {
    /// Any adaptation is acceptable.
    #[default]
    Allowed,
    /// Audio may be adapted; video must be copied. Protects remuxes from the expensive, visible loss
    /// while still letting a sink-incompatible audio track be handled.
    AudioOnly,
    /// Nothing may be adapted. If Direct Play is impossible, fail loudly with an explanation rather
    /// than degrade silently — `docs/03` §6 rule 3.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserPolicy {
    /// No resampling, no mixing, no volume scaling; exclusive device access.
    pub bit_perfect: bool,
    pub transcode: TranscodePolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientCapabilities {
    pub id: String,
    pub containers: Vec<Container>,
    pub video: Vec<VideoDecodeCaps>,
    pub audio_sink: AudioSinkCaps,
    pub display: DisplayCaps,
    pub subtitles: SubtitleCaps,
    /// The renderer can tone map HDR to the display's capability locally, leaving the video
    /// bitstream untouched. True for every libplacebo-backed client; false for most browsers.
    /// When false, an HDR/display mismatch forces a server-side transcode instead.
    pub can_tone_map: bool,
    /// Measured throughput. `None` when not yet known — the ladder must not reject on an unmeasured
    /// link, or first playback on every client would needlessly transcode.
    pub network_bps: Option<u64>,
    pub policy: UserPolicy,
}

impl ClientCapabilities {
    pub fn video_caps_for(&self, codec: &VideoCodec) -> Option<&VideoDecodeCaps> {
        self.video.iter().find(|c| &c.codec == codec)
    }

    pub fn accepts_container(&self, container: Container) -> bool {
        self.containers.contains(&container)
    }

    /// A fully capable native client on a LAN with an HD AVR and an HDR display: the reference
    /// endpoint against which conformance vectors assert their best-case tier.
    pub fn reference_native() -> Self {
        Self {
            id: "reference-native".into(),
            containers: vec![
                Container::Matroska,
                Container::WebM,
                Container::Mp4,
                Container::FragmentedMp4,
                Container::MpegTs,
                Container::DiscStructure,
            ],
            video: vec![
                VideoDecodeCaps::hardware(VideoCodec::H264, 10, 4096, 2160),
                VideoDecodeCaps::hardware(VideoCodec::Hevc, 12, 7680, 4320),
                VideoDecodeCaps::hardware(VideoCodec::Av1, 12, 7680, 4320),
                VideoDecodeCaps::hardware(VideoCodec::Vp9, 12, 7680, 4320),
                VideoDecodeCaps::hardware(VideoCodec::Mpeg2, 8, 1920, 1080),
                VideoDecodeCaps {
                    hardware: false,
                    ..VideoDecodeCaps::hardware(VideoCodec::Vc1, 8, 1920, 1080)
                },
            ],
            audio_sink: AudioSinkCaps::hd_avr("HDMI (Reference AVR)"),
            display: DisplayCaps::hdr_4k(),
            subtitles: SubtitleCaps::full(),
            can_tone_map: true,
            network_bps: Some(940_000_000),
            policy: UserPolicy::default(),
        }
    }

    /// A browser: narrow containers, PCM-only stereo audio, text subtitles, SDR. The honest floor
    /// from `docs/04` §7 — the web tier is a convenience tier and the model says so.
    pub fn reference_browser() -> Self {
        Self {
            id: "reference-browser".into(),
            containers: vec![Container::FragmentedMp4, Container::WebM],
            video: vec![
                VideoDecodeCaps::hardware(VideoCodec::H264, 8, 3840, 2160),
                VideoDecodeCaps::hardware(VideoCodec::Vp9, 10, 3840, 2160),
                VideoDecodeCaps::hardware(VideoCodec::Av1, 10, 3840, 2160),
            ],
            audio_sink: AudioSinkCaps::stereo_pcm("Browser output"),
            display: DisplayCaps::sdr_1080p(),
            subtitles: SubtitleCaps::text_only(),
            can_tone_map: false,
            network_bps: Some(50_000_000),
            policy: UserPolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hd_avr_takes_the_lossless_bitstreams_stereo_sink_does_not() {
        let avr = AudioSinkCaps::hd_avr("avr");
        assert!(avr.can_passthrough(&AudioCodec::TrueHd));
        assert!(avr.can_passthrough(&AudioCodec::DtsHdMa));

        let stereo = AudioSinkCaps::stereo_pcm("laptop");
        assert!(!stereo.can_passthrough(&AudioCodec::TrueHd));
        assert!(!stereo.can_passthrough(&AudioCodec::Ac3));
        assert_eq!(stereo.deliverable_channels(ChannelLayout::SURROUND_7_1), 2);
    }

    #[test]
    fn hdr10_display_handles_dolby_vision_with_a_compatible_base() {
        let d = DisplayCaps::hdr_4k();
        assert!(d.handles_hdr(HdrFormat::Hdr10));
        // P8/P7 carry an HDR10-compatible base layer, so an HDR10 display needs no tone mapping.
        assert!(d.handles_hdr(HdrFormat::DolbyVisionP8));
        assert!(d.handles_hdr(HdrFormat::DolbyVisionP7Fel));
        // P5 has no such base and must be converted or tone mapped.
        assert!(!d.handles_hdr(HdrFormat::DolbyVisionP5));
    }

    #[test]
    fn sdr_display_handles_only_sdr() {
        let d = DisplayCaps::sdr_1080p();
        assert!(d.handles_hdr(HdrFormat::Sdr));
        assert!(!d.handles_hdr(HdrFormat::Hdr10));
        assert!(!d.handles_hdr(HdrFormat::Hlg));
    }

    #[test]
    fn empty_profile_list_means_unrestricted_not_unsupported() {
        let c = VideoDecodeCaps::hardware(VideoCodec::Hevc, 10, 3840, 2160);
        assert!(c.accepts_profile(Some("Main 10")));
        assert!(c.accepts_profile(None));

        let restricted = VideoDecodeCaps { profiles: vec!["Main".into(), "Main 10".into()], ..c };
        assert!(restricted.accepts_profile(Some("main 10")), "case-insensitive");
        assert!(!restricted.accepts_profile(Some("Main 4:4:4 12")));
    }

    #[test]
    fn sink_generation_distinguishes_snapshots() {
        // Swapping TV speakers for an AVR must invalidate any cached decision, which is what the
        // generation counter exists for.
        let a = AudioSinkCaps::stereo_pcm("TV speakers");
        let b = AudioSinkCaps { generation: 1, ..AudioSinkCaps::hd_avr("Denon") };
        assert_ne!(a.generation, b.generation);
        assert_ne!(a, b);
    }
}
