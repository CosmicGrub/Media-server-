//! Structured reasons why a higher-fidelity playback path was rejected.
//!
//! This is not a log type. Guarantee **G1** (`docs/11` §1) requires that every departure from
//! bit-exact reproduction is shown to the user in plain language, and gap **G1** in `docs/01` makes
//! that the product's main wedge against Plex and Jellyfin, whose transcode decisions are opaque.
//!
//! Consequently `explain()` is a product surface with the same status as a UI string, and the
//! conformance corpus asserts on these variants directly via `reasons_absent` / `reasons_present`.

use lumen_model::{
    AudioCodec, ChromaSubsampling, ColorPrimaries, Container, HdrFormat, SubtitleCodec, VideoCodec,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateCause {
    ClientCeiling,
    MeasuredNetwork,
    DecoderLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnInCause {
    /// No target container in the client's list can carry the format, and the client cannot fetch a
    /// separately-delivered subtitle.
    NoCarriageAndNoOutOfBand,
    /// The client cannot render the format at all, and it cannot be converted without losing the
    /// content entirely (bitmap subtitles to a text-only renderer).
    ClientCannotRender,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RejectReason {
    ContainerUnsupported {
        container: Container,
        client: String,
    },
    VideoCodecUnsupported {
        codec: VideoCodec,
        profile: Option<String>,
        level: Option<u16>,
    },
    VideoTooLarge {
        have: (u32, u32),
        max: (u32, u32),
    },
    BitDepthUnsupported {
        have: u8,
        max: u8,
    },
    /// The decoder cannot handle this chroma layout — 4:2:2/4:4:4 profiles hardware decoders most
    /// often lack entirely (`docs/11` §8), distinct from bit depth or profile.
    ChromaSubsamplingUnsupported {
        have: ChromaSubsampling,
        max: ChromaSubsampling,
    },
    BitrateCeiling {
        have_bps: u64,
        max_bps: u64,
        cause: BitrateCause,
    },
    NoHardwareDecoder {
        codec: VideoCodec,
        fallback_viable: bool,
    },
    /// The audio sink will not accept this codec as a bitstream. Carries the sink's actual encoding
    /// list so the explanation can name what the sink *does* accept.
    SinkLacksEncoding {
        codec: AudioCodec,
        sink: String,
        sink_encodings: Vec<AudioCodec>,
    },
    ChannelCountUnsupported {
        have: u8,
        max: u8,
    },
    SampleRateUnsupported {
        have: u32,
        sink: String,
    },
    HdrUnsupportedByDisplay {
        format: HdrFormat,
    },
    /// The display's physical gamut does not cover the stream's mastering primaries -- a separate
    /// question from `HdrUnsupportedByDisplay`, since a display can support the HDR10 *format*
    /// outright while still being unable to show every colour BT.2020 content specifies.
    GamutUnsupportedByDisplay {
        primaries: ColorPrimaries,
    },
    /// Reproduction is incomplete even on fully capable hardware — currently only Dolby Vision
    /// Profile 7 FEL, whose enhancement layer no open-source renderer can reconstruct.
    EnhancementLayerUnsupported {
        format: HdrFormat,
    },
    SubtitleBurnInRequired {
        format: SubtitleCodec,
        why: BurnInCause,
    },
    NetworkHeadroom {
        measured_bps: u64,
        required_bps: u64,
    },
    /// The source was damaged and reconstruction could not recover all of it. Playback proceeds on
    /// what survived; the user is owed the fact that something is missing (`docs/12` §5).
    SourceIncomplete,
    /// The user forbade this adaptation. Distinct from a capability limit: the chain *could* do it.
    UserPolicy {
        policy: &'static str,
    },
}

impl RejectReason {
    /// The user-facing sentence. Guarantee G1 is only real if this is genuinely readable, so it
    /// names concrete devices, formats, and numbers rather than codes.
    pub fn explain(&self) -> String {
        match self {
            Self::ContainerUnsupported { container, client } => {
                format!("{client} cannot open {container:?} files directly.")
            }
            Self::VideoCodecUnsupported { codec, profile, level } => {
                let detail = match (profile, level) {
                    (Some(p), Some(l)) => format!(" ({p}, level {l})"),
                    (Some(p), None) => format!(" ({p})"),
                    _ => String::new(),
                };
                format!("This device has no decoder for {codec:?}{detail}.")
            }
            Self::VideoTooLarge { have, max } => format!(
                "The video is {}x{}, above this device's {}x{} decode limit.",
                have.0, have.1, max.0, max.1
            ),
            Self::BitDepthUnsupported { have, max } => {
                format!("The video is {have}-bit; this device decodes up to {max}-bit.")
            }
            Self::ChromaSubsamplingUnsupported { have, max } => {
                format!("The video is {have:?}; this device decodes up to {max:?}.",)
            }
            Self::BitrateCeiling { have_bps, max_bps, cause } => {
                let why = match cause {
                    BitrateCause::ClientCeiling => "the playback quality limit you set",
                    BitrateCause::MeasuredNetwork => "the measured speed of this connection",
                    BitrateCause::DecoderLimit => "this device's decoder limit",
                };
                format!(
                    "The source runs at {:.1} Mbps, above {why} of {:.1} Mbps.",
                    *have_bps as f64 / 1e6,
                    *max_bps as f64 / 1e6
                )
            }
            Self::NoHardwareDecoder { codec, fallback_viable } => {
                if *fallback_viable {
                    format!("No hardware decoder for {codec:?}; using software decoding.")
                } else {
                    format!(
                        "No hardware decoder for {codec:?}, and software decoding is too slow here."
                    )
                }
            }
            Self::SinkLacksEncoding { codec, sink, sink_encodings } => {
                let accepts = if sink_encodings.is_empty() {
                    "only uncompressed audio".to_string()
                } else {
                    let names: Vec<String> =
                        sink_encodings.iter().map(|c| format!("{c:?}")).collect();
                    names.join(", ")
                };
                format!("\"{sink}\" does not accept {codec:?}. It accepts: {accepts}.")
            }
            Self::ChannelCountUnsupported { have, max } => {
                format!("The track has {have} channels; this output supports {max}.")
            }
            Self::SampleRateUnsupported { have, sink } => {
                format!("\"{sink}\" does not support {} kHz.", *have as f64 / 1000.0)
            }
            Self::HdrUnsupportedByDisplay { format } => {
                format!("This display does not support {format:?}, so the picture was tone mapped.")
            }
            Self::GamutUnsupportedByDisplay { primaries } => format!(
                "This display's colour gamut does not cover the source's {primaries:?} primaries, \
                 so the picture was gamut mapped."
            ),
            Self::EnhancementLayerUnsupported { format } => format!(
                "{format:?} carries an enhancement layer that cannot be reconstructed. \
                 Playing the HDR10 base layer, which is the full picture minus the extra \
                 Dolby Vision detail."
            ),
            Self::SubtitleBurnInRequired { format, why } => {
                let why = match why {
                    BurnInCause::NoCarriageAndNoOutOfBand => {
                        "this client cannot receive subtitles separately"
                    }
                    BurnInCause::ClientCannotRender => {
                        "this client cannot draw this subtitle format"
                    }
                };
                format!("{format:?} subtitles had to be drawn into the picture because {why}.")
            }
            Self::NetworkHeadroom { measured_bps, required_bps } => format!(
                "This connection is delivering {:.0} Mbps but the source needs {:.0} Mbps.",
                *measured_bps as f64 / 1e6,
                *required_bps as f64 / 1e6
            ),
            Self::SourceIncomplete => "This file is damaged. Playing everything that could be \
                 recovered — some content is missing or could not be verified."
                .to_string(),
            Self::UserPolicy { policy } => {
                format!("Blocked by your \"{policy}\" setting, which forbids changing the stream.")
            }
        }
    }

    /// Stable machine name, matching the `reasons_absent` / `reasons_present` keys used in
    /// `conformance/corpus.yaml`.
    pub fn key(&self) -> &'static str {
        match self {
            Self::ContainerUnsupported { .. } => "ContainerUnsupported",
            Self::VideoCodecUnsupported { .. } => "VideoCodecUnsupported",
            Self::VideoTooLarge { .. } => "VideoTooLarge",
            Self::BitDepthUnsupported { .. } => "BitDepthUnsupported",
            Self::ChromaSubsamplingUnsupported { .. } => "ChromaSubsamplingUnsupported",
            Self::BitrateCeiling { .. } => "BitrateCeiling",
            Self::NoHardwareDecoder { .. } => "NoHardwareDecoder",
            Self::SinkLacksEncoding { .. } => "SinkLacksEncoding",
            Self::ChannelCountUnsupported { .. } => "ChannelCountUnsupported",
            Self::SampleRateUnsupported { .. } => "SampleRateUnsupported",
            Self::HdrUnsupportedByDisplay { .. } => "HdrUnsupportedByDisplay",
            Self::GamutUnsupportedByDisplay { .. } => "GamutUnsupportedByDisplay",
            Self::EnhancementLayerUnsupported { .. } => "EnhancementLayerUnsupported",
            Self::SubtitleBurnInRequired { .. } => "SubtitleBurnInRequired",
            Self::NetworkHeadroom { .. } => "NetworkHeadroom",
            Self::SourceIncomplete => "SourceIncomplete",
            Self::UserPolicy { .. } => "UserPolicy",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_explanation_names_the_device_and_what_it_does_accept() {
        // The whole point of gap G1: tell the user what their AVR actually supports, not "error".
        let r = RejectReason::SinkLacksEncoding {
            codec: AudioCodec::DtsHdMa,
            sink: "HDMI (Sony BRAVIA)".into(),
            sink_encodings: vec![AudioCodec::Ac3, AudioCodec::EAc3],
        };
        let msg = r.explain();
        assert!(msg.contains("Sony BRAVIA"), "{msg}");
        assert!(msg.contains("DtsHdMa"), "{msg}");
        assert!(msg.contains("Ac3"), "{msg}");
    }

    #[test]
    fn pcm_only_sink_explains_itself_without_an_empty_list() {
        let r = RejectReason::SinkLacksEncoding {
            codec: AudioCodec::TrueHd,
            sink: "MacBook Pro Speakers".into(),
            sink_encodings: vec![],
        };
        assert!(r.explain().contains("only uncompressed audio"), "{}", r.explain());
    }

    #[test]
    fn fel_explanation_is_honest_about_what_is_lost() {
        // docs/11 §7: we do not claim FEL support, we state the base-layer outcome plainly.
        let msg = RejectReason::EnhancementLayerUnsupported { format: HdrFormat::DolbyVisionP7Fel }
            .explain();
        assert!(msg.contains("base layer"), "{msg}");
    }

    #[test]
    fn every_reason_produces_a_non_empty_explanation_and_a_stable_key() {
        let all = [
            RejectReason::ContainerUnsupported {
                container: Container::Matroska,
                client: "web".into(),
            },
            RejectReason::VideoCodecUnsupported {
                codec: VideoCodec::Vc1,
                profile: None,
                level: None,
            },
            RejectReason::VideoTooLarge { have: (7680, 4320), max: (3840, 2160) },
            RejectReason::BitDepthUnsupported { have: 10, max: 8 },
            RejectReason::ChromaSubsamplingUnsupported {
                have: ChromaSubsampling::Yuv444,
                max: ChromaSubsampling::Yuv420,
            },
            RejectReason::BitrateCeiling {
                have_bps: 92_000_000,
                max_bps: 20_000_000,
                cause: BitrateCause::ClientCeiling,
            },
            RejectReason::NoHardwareDecoder { codec: VideoCodec::H264, fallback_viable: true },
            RejectReason::SinkLacksEncoding {
                codec: AudioCodec::TrueHd,
                sink: "x".into(),
                sink_encodings: vec![],
            },
            RejectReason::ChannelCountUnsupported { have: 8, max: 2 },
            RejectReason::SampleRateUnsupported { have: 192_000, sink: "x".into() },
            RejectReason::HdrUnsupportedByDisplay { format: HdrFormat::Hdr10 },
            RejectReason::GamutUnsupportedByDisplay { primaries: ColorPrimaries::Bt2020 },
            RejectReason::EnhancementLayerUnsupported { format: HdrFormat::DolbyVisionP7Fel },
            RejectReason::SubtitleBurnInRequired {
                format: SubtitleCodec::Pgs,
                why: BurnInCause::ClientCannotRender,
            },
            RejectReason::NetworkHeadroom { measured_bps: 24_000_000, required_bps: 92_000_000 },
            RejectReason::SourceIncomplete,
            RejectReason::UserPolicy { policy: "Bit-perfect" },
        ];
        let mut keys = Vec::new();
        for r in &all {
            let msg = r.explain();
            assert!(msg.len() > 20, "explanation too terse for {r:?}: {msg}");
            assert!(msg.ends_with('.'), "explanation should be a sentence: {msg}");
            keys.push(r.key());
        }
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(before, keys.len(), "reason keys must be unique");
    }
}
