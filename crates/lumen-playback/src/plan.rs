//! The shape of a playback decision: what happens to each stream, the tier achieved, and every
//! better outcome that was rejected with its reason.

use lumen_model::{AudioCodec, ChannelLayout, Container, Integrity, SubtitleCodec, VideoCodec};

use crate::reason::RejectReason;

/// Fidelity achieved, per `docs/11` §1.1. Lower is better; a session resolves to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Every byte of every selected stream reaches the decoder or sink untouched.
    T0BitExact,
    /// Streams untouched; container rewritten and/or lossless audio decoded at native rate and
    /// channel count. Indistinguishable from the source.
    T1FullFidelity,
    /// Video untouched; audio adapted without dropping below source channel count. HDR preserved.
    T2Preserved,
    /// Video and/or audio transcoded, or subtitles burned in.
    T3Adapted,
    /// Source was damaged or non-conformant and had to be reconstructed with content loss.
    T4Recovered,
    /// Cannot play. Only the causes in `docs/11` §7 may produce this.
    T5Blocked,
}

impl Tier {
    /// Conformance asserts `achieved <= expected`, so an improvement never fails the build.
    pub fn is_at_least_as_good_as(self, expected: Tier) -> bool {
        self <= expected
    }

    pub fn as_key(self) -> &'static str {
        match self {
            Self::T0BitExact => "T0",
            Self::T1FullFidelity => "T1",
            Self::T2Preserved => "T2",
            Self::T3Adapted => "T3",
            Self::T4Recovered => "T4",
            Self::T5Blocked => "T5",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerPlan {
    /// Direct Play: the client opens the source bytes as they are.
    Original,
    /// Container rewritten, elementary streams untouched.
    Remux(Container),
    /// No container the client accepts can carry any producible codec combination. Always converted
    /// into a blocked plan before it escapes the ladder; never observable on a returned plan.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoTranscodeSpec {
    pub codec: VideoCodec,
    pub max_width: u32,
    pub max_height: u32,
    pub max_bitrate_bps: Option<u64>,
    pub tone_map_to_sdr: bool,
    pub deinterlace: bool,
    pub burn_in_subtitles: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VideoPath {
    /// Elementary stream copied byte-for-byte.
    Copy,
    Transcode(VideoTranscodeSpec),
    /// Video dropped, audio kept — recovery rung 9 (`docs/12` §5). When no codec the client can
    /// decode is reachable, audio-only playback is far better than nothing, and it is the outcome a
    /// user would choose if asked.
    Drop,
}

impl VideoPath {
    pub fn is_copy(&self) -> bool {
        matches!(self, Self::Copy)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioPath {
    /// No audio stream selected (video-only source, or the user muted the track).
    None,
    /// Compressed bitstream sent to the sink untouched, IEC 61937 encapsulated. Preserves Atmos and
    /// DTS:X objects.
    Passthrough,
    /// Decoded to LPCM with no resampling, mixing, or volume scaling, on an exclusive device.
    ExclusiveBitPerfect,
    /// Decoded to LPCM. Lossless in the sample domain, but object-based Atmos and DTS:X
    /// positioning is flattened into the channel bed — which is why `objects_lost` caps the tier.
    DecodeToLpcm { channels: u8, sample_rate: u32, resampled: bool, objects_lost: bool },
    /// The source's embedded lossy core, extracted as an original bitstream — never re-encoded.
    CoreExtraction { core: AudioCodec },
    /// Re-encoded, possibly with a channel-count reduction.
    Transcode { codec: AudioCodec, channels: u8 },
}

impl AudioPath {
    pub fn reencodes(&self) -> bool {
        matches!(self, Self::Transcode { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubtitleDelivery {
    None,
    /// Carried inside the media stream, either as a container track or as in-video captions.
    InBand,
    /// Delivered separately for the client to render. The correct answer roughly 95% of the time
    /// (`docs/13` §5) and the reason burn-in stays rare.
    OutOfBand {
        as_format: SubtitleCodec,
    },
    /// Drawn into the picture. Forces a video transcode and is always tier T3.
    BurnedIn,
}

/// A better tier that was attempted and why it did not apply.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejection {
    pub tier: Tier,
    pub reason: RejectReason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackPlan {
    pub container: ContainerPlan,
    pub video: VideoPath,
    pub audio: AudioPath,
    pub subtitle: SubtitleDelivery,
    pub tier: Tier,
    /// Every higher-fidelity outcome that was ruled out, in the order it was considered. This is
    /// what the Playback Report renders, and what guarantee G1 is built on.
    pub rejections: Vec<Rejection>,
}

impl PlaybackPlan {
    /// Cannot play. Carries the specific cause — `docs/11` §7 permits no unexplained T5.
    pub fn blocked(reason: RejectReason) -> Self {
        Self {
            container: ContainerPlan::Original,
            video: VideoPath::Copy,
            audio: AudioPath::None,
            subtitle: SubtitleDelivery::None,
            tier: Tier::T5Blocked,
            rejections: vec![Rejection { tier: Tier::T0BitExact, reason }],
        }
    }

    pub fn is_blocked(&self) -> bool {
        self.tier == Tier::T5Blocked
    }

    /// True when no re-encoding of any kind occurs. The metric published per release in
    /// `docs/13` §8 counts sessions where this holds.
    pub fn is_direct(&self) -> bool {
        self.video.is_copy()
            && !self.audio.reencodes()
            && self.subtitle != SubtitleDelivery::BurnedIn
    }

    pub fn reason_keys(&self) -> Vec<&'static str> {
        self.rejections.iter().map(|r| r.reason.key()).collect()
    }

    /// The ordered, human-readable explanation shown in the Playback Report.
    pub fn explain(&self) -> Vec<String> {
        self.rejections.iter().map(|r| r.reason.explain()).collect()
    }

    /// Derive the tier from the chosen paths. Kept separate from planning so the mapping can be
    /// asserted independently of the decision logic that produces the paths.
    pub(crate) fn derive_tier(
        container: ContainerPlan,
        video: &VideoPath,
        audio: &AudioPath,
        subtitle: &SubtitleDelivery,
        integrity: Integrity,
        source_channels: ChannelLayout,
    ) -> Tier {
        // Content loss during recovery dominates: however cleanly it plays, the picture is
        // incomplete and the user is owed that fact.
        if integrity == Integrity::RecoveredLossy {
            return Tier::T4Recovered;
        }
        if !video.is_copy() || *subtitle == SubtitleDelivery::BurnedIn {
            return Tier::T3Adapted;
        }
        match audio {
            AudioPath::Transcode { channels, .. } => {
                if *channels < source_channels.channels {
                    Tier::T3Adapted
                } else {
                    Tier::T2Preserved
                }
            }
            AudioPath::CoreExtraction { .. } => Tier::T2Preserved,
            AudioPath::DecodeToLpcm { channels, resampled, objects_lost, .. } => {
                if *channels < source_channels.channels {
                    // Below source channel count is a downmix: T3 per docs/11 §1.1.
                    Tier::T3Adapted
                } else if *resampled || *objects_lost {
                    Tier::T2Preserved
                } else {
                    Tier::T1FullFidelity
                }
            }
            AudioPath::None | AudioPath::Passthrough | AudioPath::ExclusiveBitPerfect => {
                if container == ContainerPlan::Original {
                    Tier::T0BitExact
                } else {
                    Tier::T1FullFidelity
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier_of(container: ContainerPlan, video: VideoPath, audio: AudioPath) -> Tier {
        PlaybackPlan::derive_tier(
            container,
            &video,
            &audio,
            &SubtitleDelivery::None,
            Integrity::Intact,
            ChannelLayout::SURROUND_7_1,
        )
    }

    #[test]
    fn t0_requires_original_container_and_an_untouched_bitstream() {
        assert_eq!(
            tier_of(ContainerPlan::Original, VideoPath::Copy, AudioPath::Passthrough),
            Tier::T0BitExact
        );
        // A container rewrite is fidelity-preserving but not bit-exact end to end.
        assert_eq!(
            tier_of(
                ContainerPlan::Remux(Container::Matroska),
                VideoPath::Copy,
                AudioPath::Passthrough
            ),
            Tier::T1FullFidelity
        );
    }

    #[test]
    fn lpcm_at_full_channels_and_native_rate_is_full_fidelity() {
        assert_eq!(
            tier_of(
                ContainerPlan::Original,
                VideoPath::Copy,
                AudioPath::DecodeToLpcm {
                    channels: 8,
                    sample_rate: 48_000,
                    resampled: false,
                    objects_lost: false
                }
            ),
            Tier::T1FullFidelity
        );
        // Resampling or losing channels drops to preserved.
        assert_eq!(
            tier_of(
                ContainerPlan::Original,
                VideoPath::Copy,
                AudioPath::DecodeToLpcm {
                    channels: 8,
                    sample_rate: 48_000,
                    resampled: true,
                    objects_lost: false
                }
            ),
            Tier::T2Preserved
        );
        // Losing channels is a downmix, which is a bigger compromise than resampling.
        assert_eq!(
            tier_of(
                ContainerPlan::Original,
                VideoPath::Copy,
                AudioPath::DecodeToLpcm {
                    channels: 6,
                    sample_rate: 48_000,
                    resampled: false,
                    objects_lost: false
                }
            ),
            Tier::T3Adapted
        );
        // Flattening Atmos objects into the bed is sample-lossless but not artistically identical.
        assert_eq!(
            tier_of(
                ContainerPlan::Original,
                VideoPath::Copy,
                AudioPath::DecodeToLpcm {
                    channels: 8,
                    sample_rate: 48_000,
                    resampled: false,
                    objects_lost: true
                }
            ),
            Tier::T2Preserved
        );
    }

    #[test]
    fn core_extraction_is_preserved_not_adapted() {
        // docs/13 §4: extracting the DTS core is the original bitstream, not a re-encode, so it
        // must not be scored as harshly as a transcode.
        assert_eq!(
            tier_of(
                ContainerPlan::Original,
                VideoPath::Copy,
                AudioPath::CoreExtraction { core: AudioCodec::Dts }
            ),
            Tier::T2Preserved
        );
    }

    #[test]
    fn any_video_transcode_or_burn_in_is_t3() {
        let spec = VideoTranscodeSpec {
            codec: VideoCodec::H264,
            max_width: 1920,
            max_height: 1080,
            max_bitrate_bps: None,
            tone_map_to_sdr: false,
            deinterlace: false,
            burn_in_subtitles: false,
        };
        assert_eq!(
            tier_of(ContainerPlan::Original, VideoPath::Transcode(spec), AudioPath::Passthrough),
            Tier::T3Adapted
        );
        assert_eq!(
            PlaybackPlan::derive_tier(
                ContainerPlan::Original,
                &VideoPath::Copy,
                &AudioPath::Passthrough,
                &SubtitleDelivery::BurnedIn,
                Integrity::Intact,
                ChannelLayout::STEREO,
            ),
            Tier::T3Adapted
        );
    }

    #[test]
    fn lossy_recovery_dominates_every_other_outcome() {
        // A truncated file that direct-plays perfectly is still T4 — the user is owed the fact that
        // content is missing.
        assert_eq!(
            PlaybackPlan::derive_tier(
                ContainerPlan::Original,
                &VideoPath::Copy,
                &AudioPath::Passthrough,
                &SubtitleDelivery::None,
                Integrity::RecoveredLossy,
                ChannelLayout::STEREO,
            ),
            Tier::T4Recovered
        );
        // Recovery that preserved all content does not cap the tier: a Cues-less MKV still reaches
        // T0. This is why `Integrity` has three states rather than a bool.
        assert_eq!(
            PlaybackPlan::derive_tier(
                ContainerPlan::Original,
                &VideoPath::Copy,
                &AudioPath::Passthrough,
                &SubtitleDelivery::None,
                Integrity::RecoveredComplete,
                ChannelLayout::STEREO,
            ),
            Tier::T0BitExact
        );
    }

    #[test]
    fn tier_ordering_lets_conformance_accept_improvements() {
        assert!(Tier::T0BitExact.is_at_least_as_good_as(Tier::T1FullFidelity));
        assert!(Tier::T1FullFidelity.is_at_least_as_good_as(Tier::T1FullFidelity));
        assert!(!Tier::T3Adapted.is_at_least_as_good_as(Tier::T1FullFidelity));
    }

    #[test]
    fn blocked_plans_always_carry_a_cause() {
        let p = PlaybackPlan::blocked(RejectReason::UserPolicy { policy: "Bit-perfect" });
        assert!(p.is_blocked());
        assert_eq!(p.rejections.len(), 1);
        assert!(!p.explain()[0].is_empty());
    }
}
