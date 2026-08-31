//! The playback decision ladder — `docs/03` §6 and `docs/13` §2–4.
//!
//! One implementation, compiled into every client *and* the server. Per ADR-0004 this is the piece
//! that must never diverge: if six clients each had their own version they would disagree, and users
//! would experience that as random transcoding.
//!
//! Invariants, asserted by the property tests in `tests/ladder_props.rs`:
//!
//! 1. The emitted plan is always playable by the capabilities it was planned against.
//! 2. The plan reaches the best tier those capabilities allow.
//! 3. Every rung rejected on the way down is recorded with a structured reason (guarantee G1).
//! 4. `TranscodePolicy::None` never silently degrades — it blocks, with an explanation.

use lumen_caps::{ClientCapabilities, TranscodePolicy};
use lumen_model::{
    AudioCodec, AudioStream, ChannelLayout, Container, Integrity, MediaSource, SubtitleCodec,
    SubtitleStream, VideoCodec, VideoStream,
};

use crate::plan::{
    AudioPath, ContainerPlan, PlaybackPlan, Rejection, SubtitleDelivery, Tier, VideoPath,
    VideoTranscodeSpec,
};
use crate::reason::{BitrateCause, BurnInCause, RejectReason};

/// Which streams to play. Produced by [`crate::select`] or by explicit user choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub video: Option<u32>,
    pub audio: Option<u32>,
    pub subtitle: Option<u32>,
}

struct Ctx {
    rejections: Vec<Rejection>,
}

impl Ctx {
    fn reject(&mut self, tier: Tier, reason: RejectReason) {
        self.rejections.push(Rejection { tier, reason });
    }
}

/// Plan playback of `selection` from `source` on `caps`.
///
/// Never panics and never returns a plan the client cannot execute; when nothing works it returns a
/// [`Tier::T5Blocked`] plan carrying the specific cause.
pub fn plan(source: &MediaSource, selection: Selection, caps: &ClientCapabilities) -> PlaybackPlan {
    let mut ctx = Ctx { rejections: Vec::new() };

    let video = selection.video.and_then(|i| source.video.iter().find(|s| s.index == i));
    let audio = selection.audio.and_then(|i| source.audio.iter().find(|s| s.index == i));
    let subtitle = selection.subtitle.and_then(|i| source.subtitles.iter().find(|s| s.index == i));

    if video.is_none() && audio.is_none() {
        return PlaybackPlan::blocked(RejectReason::ContainerUnsupported {
            container: source.container,
            client: caps.id.clone(),
        });
    }

    let video_path = match video {
        Some(v) => decide_video(v, source, caps, &mut ctx),
        None => VideoPath::Copy,
    };
    let audio_path = match audio {
        Some(a) => decide_audio(a, caps, &mut ctx),
        None => AudioPath::None,
    };

    let (container, video_path, audio_path) =
        decide_container(source, video, audio, video_path, audio_path, caps, &mut ctx);

    // The *delivered* container, not the plan variant: burn-in re-encodes into whatever the client
    // will actually open, which for Direct Play is the source's own container.
    let delivered = match container {
        ContainerPlan::Original => Some(source.container),
        ContainerPlan::Remux(c) => Some(c),
        ContainerPlan::Unavailable => None,
    };
    let (subtitle_delivery, video_path) =
        decide_subtitle(subtitle, container, delivered, video, video_path, caps, &mut ctx);

    if source.integrity == Integrity::RecoveredLossy {
        // G1 applies to damage as much as to transcoding: T4 without an explanation is silent
        // degradation. Recorded before tier derivation so the reason is present whatever else ran.
        ctx.reject(Tier::T0BitExact, RejectReason::SourceIncomplete);
    }

    let source_channels = audio.map_or(ChannelLayout::default(), |a| a.layout);
    let tier = PlaybackPlan::derive_tier(
        container,
        &video_path,
        &audio_path,
        &subtitle_delivery,
        source.integrity,
        source_channels,
    );

    if container == ContainerPlan::Unavailable {
        let mut blocked = PlaybackPlan::blocked(RejectReason::ContainerUnsupported {
            container: source.container,
            client: caps.id.clone(),
        });
        blocked.rejections = ctx.rejections;
        return blocked;
    }

    let candidate = PlaybackPlan {
        container,
        video: video_path,
        audio: audio_path,
        subtitle: subtitle_delivery,
        tier,
        rejections: ctx.rejections,
    };

    enforce_policy(candidate, caps)
}

/// `TranscodePolicy::None` must fail loudly rather than degrade silently — `docs/03` §6 rule 3.
/// The user asked for bit-exact; handing them a transcode is the failure mode the setting exists to
/// prevent.
fn enforce_policy(plan: PlaybackPlan, caps: &ClientCapabilities) -> PlaybackPlan {
    let policy = caps.policy;
    let violates = match policy.transcode {
        TranscodePolicy::Allowed => false,
        TranscodePolicy::AudioOnly => !plan.video.is_copy(),
        TranscodePolicy::None => !plan.is_direct() || plan.container != ContainerPlan::Original,
    };
    if !violates || plan.is_blocked() {
        return plan;
    }
    let policy_name = if policy.bit_perfect {
        "Bit-perfect"
    } else {
        match policy.transcode {
            TranscodePolicy::None => "Never transcode",
            TranscodePolicy::AudioOnly => "Never transcode video",
            TranscodePolicy::Allowed => unreachable!(),
        }
    };
    let mut blocked = PlaybackPlan::blocked(RejectReason::UserPolicy { policy: policy_name });
    // Keep everything already learned: the user needs to see *why* Direct Play was impossible, not
    // merely that their policy stopped the fallback.
    let mut rejections = plan.rejections;
    rejections.append(&mut blocked.rejections);
    blocked.rejections = rejections;
    blocked
}

fn decide_video(
    v: &VideoStream,
    source: &MediaSource,
    caps: &ClientCapabilities,
    ctx: &mut Ctx,
) -> VideoPath {
    let mut must_transcode = false;

    match caps.video_caps_for(&v.codec) {
        None => {
            ctx.reject(
                Tier::T1FullFidelity,
                RejectReason::VideoCodecUnsupported {
                    codec: v.codec.clone(),
                    profile: v.profile.clone(),
                    level: v.level,
                },
            );
            must_transcode = true;
        }
        Some(dc) => {
            if !dc.accepts_profile(v.profile.as_deref()) {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::VideoCodecUnsupported {
                        codec: v.codec.clone(),
                        profile: v.profile.clone(),
                        level: v.level,
                    },
                );
                must_transcode = true;
            }
            if !dc.accepts_level(v.level) {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::VideoCodecUnsupported {
                        codec: v.codec.clone(),
                        profile: v.profile.clone(),
                        level: v.level,
                    },
                );
                must_transcode = true;
            }
            if v.bit_depth > dc.max_bit_depth {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::BitDepthUnsupported { have: v.bit_depth, max: dc.max_bit_depth },
                );
                must_transcode = true;
            }
            if !dc.accepts_chroma(v.chroma) {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::ChromaSubsamplingUnsupported {
                        have: v.chroma,
                        max: dc.max_chroma,
                    },
                );
                must_transcode = true;
            }
            if v.width > dc.max_width || v.height > dc.max_height {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::VideoTooLarge {
                        have: (v.width, v.height),
                        max: (dc.max_width, dc.max_height),
                    },
                );
                must_transcode = true;
            }
            if let (Some(have), Some(max)) =
                (v.bitrate_bps.or(source.bitrate_bps), dc.max_bitrate_bps)
                && have > max
            {
                ctx.reject(
                    Tier::T1FullFidelity,
                    RejectReason::BitrateCeiling {
                        have_bps: have,
                        max_bps: max,
                        cause: BitrateCause::DecoderLimit,
                    },
                );
                must_transcode = true;
            }
            if !dc.hardware && v.codec.is_typically_software_only() {
                // Informational only: hardware decode is an optimisation, never a correctness gate
                // (`docs/11` §8). Recorded so the Playback Report can explain a high CPU reading.
                ctx.reject(
                    Tier::T0BitExact,
                    RejectReason::NoHardwareDecoder {
                        codec: v.codec.clone(),
                        fallback_viable: true,
                    },
                );
            }
        }
    }

    // Network headroom. Unmeasured links must not trigger rejection, or first playback on every new
    // client would needlessly transcode.
    if let (Some(measured), Some(required)) = (caps.network_bps, source.bitrate_bps)
        && required > measured
    {
        ctx.reject(
            Tier::T1FullFidelity,
            RejectReason::NetworkHeadroom { measured_bps: measured, required_bps: required },
        );
        must_transcode = true;
    }

    // HDR handling is a *render* decision, not a stream decision. A client that can tone map keeps
    // the bitstream untouched, so the tier is unaffected — but the user is still told the picture
    // was adapted for their display.
    let hdr = v.color.hdr;
    if hdr.is_hdr() && !caps.display.handles_hdr(hdr) {
        ctx.reject(Tier::T1FullFidelity, RejectReason::HdrUnsupportedByDisplay { format: hdr });
        if !caps.can_tone_map {
            must_transcode = true;
        }
    }
    if hdr.is_lossy_to_reproduce() {
        // Dolby Vision Profile 7 FEL: honest labelling, never a support claim (`docs/11` §7).
        ctx.reject(Tier::T0BitExact, RejectReason::EnhancementLayerUnsupported { format: hdr });
    }

    if !must_transcode {
        return VideoPath::Copy;
    }

    let Some(target) = pick_video_target(caps) else {
        // Nothing the client can decode is reachable. Rung 9: drop the video and keep the audio
        // rather than blocking the session outright.
        ctx.reject(
            Tier::T3Adapted,
            RejectReason::VideoCodecUnsupported {
                codec: v.codec.clone(),
                profile: v.profile.clone(),
                level: v.level,
            },
        );
        return VideoPath::Drop;
    };
    let dc = caps.video_caps_for(&target);
    VideoPath::Transcode(VideoTranscodeSpec {
        codec: target,
        max_width: dc.map_or(1920, |d| d.max_width.min(v.width.max(1))),
        max_height: dc.map_or(1080, |d| d.max_height.min(v.height.max(1))),
        max_bitrate_bps: bitrate_ceiling(caps, source.bitrate_bps),
        tone_map_to_sdr: hdr.is_hdr() && !caps.display.handles_hdr(hdr),
        deinterlace: v.field_order.is_interlaced(),
        burn_in_subtitles: false,
    })
}

/// Prefer the most efficient codec the client can actually decode, so a forced transcode costs the
/// least bitrate. Never upscale and never exceed source bitrate (`docs/13` §3.1).
///
/// Returns `None` when the client declared no decoder at all — transcoding to a codec it cannot
/// decode would produce a plan that fails on the user's device rather than at planning time.
fn pick_video_target(caps: &ClientCapabilities) -> Option<VideoCodec> {
    pick_video_target_for(caps, None)
}

/// As [`pick_video_target`], additionally constrained to what an already-chosen container can carry.
///
/// Burn-in is decided after the container is fixed, so a target picked without this constraint can
/// produce e.g. AV1-in-MPEG-TS, which no muxer will accept.
fn pick_video_target_for(
    caps: &ClientCapabilities,
    container: Option<Container>,
) -> Option<VideoCodec> {
    [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264]
        .into_iter()
        .filter(|c| caps.video_caps_for(c).is_some())
        .find(|c| container.is_none_or(|k| k.accepts_video(c)))
}

fn bitrate_ceiling(caps: &ClientCapabilities, source_bps: Option<u64>) -> Option<u64> {
    let net = caps.network_bps.map(|n| n * 8 / 10); // leave headroom for jitter
    match (net, source_bps) {
        (Some(n), Some(s)) => Some(n.min(s)), // never exceed source
        (Some(n), None) => Some(n),
        (None, s) => s,
    }
}

/// The audio ladder from `docs/13` §4.
///
/// Ordered so that the cheapest, highest-fidelity option wins: passthrough preserves objects,
/// LPCM preserves every sample, core extraction reuses an original bitstream, and re-encoding is
/// last. Audio adaptation costs roughly 2% of a CPU core against a GPU for video, so the ladder
/// always spends here before it spends on video.
fn decide_audio(a: &AudioStream, caps: &ClientCapabilities, ctx: &mut Ctx) -> AudioPath {
    let sink = &caps.audio_sink;

    if sink.can_passthrough(&a.codec) {
        return AudioPath::Passthrough;
    }
    ctx.reject(
        Tier::T0BitExact,
        RejectReason::SinkLacksEncoding {
            codec: a.codec.clone(),
            sink: sink.device_name.clone(),
            sink_encodings: sink.passthrough_encodings.clone(),
        },
    );

    let channels_fit = a.layout.channels <= sink.max_pcm_channels;
    let rate_ok = sink.supports_sample_rate(a.sample_rate);

    if !channels_fit {
        ctx.reject(
            Tier::T1FullFidelity,
            RejectReason::ChannelCountUnsupported {
                have: a.layout.channels,
                max: sink.max_pcm_channels,
            },
        );
    }
    if !rate_ok {
        ctx.reject(
            Tier::T1FullFidelity,
            RejectReason::SampleRateUnsupported {
                have: a.sample_rate,
                sink: sink.device_name.clone(),
            },
        );
    }

    if channels_fit
        && rate_ok
        && a.codec.is_lossless()
        && !a.has_objects
        && sink.exclusive_available
    {
        return AudioPath::ExclusiveBitPerfect;
    }

    if channels_fit {
        return AudioPath::DecodeToLpcm {
            channels: a.layout.channels,
            sample_rate: if rate_ok { a.sample_rate } else { preferred_rate(caps, a.sample_rate) },
            resampled: !rate_ok,
            objects_lost: a.has_objects,
        };
    }

    // The sink cannot take the full channel count as PCM. Before downmixing, try the source's own
    // embedded lossy core — it is an original bitstream, so extracting it is not a re-encode.
    if let Some(core) = a.codec.extractable_core()
        && sink.can_passthrough(&core)
    {
        return AudioPath::CoreExtraction { core };
    }

    let channels = sink.deliverable_channels(a.layout);
    AudioPath::Transcode { codec: pick_audio_target(caps, channels), channels }
}

fn preferred_rate(caps: &ClientCapabilities, source_rate: u32) -> u32 {
    // Prefer a rate that is an integer relative of the source, so the resampler has the easiest job.
    let sink = &caps.audio_sink;
    let mut best = *sink.pcm_sample_rates.first().unwrap_or(&48_000);
    for &r in &sink.pcm_sample_rates {
        let related = source_rate % r == 0 || r % source_rate == 0;
        let better_related = related && (best % source_rate != 0 && source_rate % best != 0);
        if better_related || (r > best && r <= source_rate) {
            best = r;
        }
    }
    best
}

/// Never jump straight to stereo AAC (`docs/13` §4.1). E-AC-3 keeps the channel count and is
/// accepted almost everywhere; AAC is the floor, not the default.
fn pick_audio_target(caps: &ClientCapabilities, channels: u8) -> AudioCodec {
    if channels > 2 {
        for candidate in [AudioCodec::EAc3, AudioCodec::Ac3] {
            if caps.audio_sink.can_passthrough(&candidate) {
                return candidate;
            }
        }
    }
    AudioCodec::Aac
}

type ContainerDecision = (ContainerPlan, VideoPath, AudioPath);

fn decide_container(
    source: &MediaSource,
    video: Option<&VideoStream>,
    audio: Option<&AudioStream>,
    video_path: VideoPath,
    audio_path: AudioPath,
    caps: &ClientCapabilities,
    ctx: &mut Ctx,
) -> ContainerDecision {
    // Work the client performs locally — decoding to LPCM, tone mapping — leaves the file untouched,
    // so Direct Play remains available. Only a server-side stream rewrite forces a container change.
    let stream_rewritten = !video_path.is_copy()
        || matches!(audio_path, AudioPath::Transcode { .. } | AudioPath::CoreExtraction { .. });

    if !stream_rewritten && caps.accepts_container(source.container) {
        return (ContainerPlan::Original, video_path, audio_path);
    }
    if !caps.accepts_container(source.container) {
        ctx.reject(
            Tier::T0BitExact,
            RejectReason::ContainerUnsupported {
                container: source.container,
                client: caps.id.clone(),
            },
        );
    }

    let out_video = match &video_path {
        VideoPath::Copy => video.map(|v| v.codec.clone()),
        VideoPath::Transcode(spec) => Some(spec.codec.clone()),
        VideoPath::Drop => None,
    };
    let out_audio = match &audio_path {
        AudioPath::Transcode { codec, .. } => Some(codec.clone()),
        AudioPath::CoreExtraction { core } => Some(core.clone()),
        AudioPath::None => None,
        // Client-side decode still ships the original compressed track inside the container.
        _ => audio.map(|a| a.codec.clone()),
    };

    // Search (video target x audio target) in preference order. Index 0 of each list is "unchanged",
    // and video is the outer loop, so the first hit is always the option that changes the least —
    // and always adapts audio before video, which is the whole point of `docs/13` §4: audio costs
    // ~2% of a core, video costs a GPU.
    let video_targets: Vec<Option<VideoCodec>> = std::iter::once(out_video.clone())
        .chain(
            [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264]
                .into_iter()
                .filter(|c| caps.video_caps_for(c).is_some())
                .map(Some),
        )
        .collect();
    let audio_targets: Vec<Option<AudioCodec>> = std::iter::once(out_audio.clone())
        .chain([AudioCodec::EAc3, AudioCodec::Aac, AudioCodec::Opus].into_iter().map(Some))
        .collect();

    let channels = audio.map_or(2, |a| caps.audio_sink.deliverable_channels(a.layout));

    for (vi, v) in video_targets.iter().enumerate() {
        for (ai, a) in audio_targets.iter().enumerate() {
            let Some(c) = best_container(caps, v.as_ref(), a.as_ref()) else { continue };

            let new_video = if vi == 0 {
                video_path.clone()
            } else {
                let target = v.clone().expect("generated targets are always Some");
                let dc = caps.video_caps_for(&target);
                VideoPath::Transcode(VideoTranscodeSpec {
                    codec: target,
                    max_width: video
                        .map_or(1920, |x| dc.map_or(x.width, |d| d.max_width.min(x.width))),
                    max_height: video
                        .map_or(1080, |x| dc.map_or(x.height, |d| d.max_height.min(x.height))),
                    max_bitrate_bps: bitrate_ceiling(caps, source.bitrate_bps),
                    tone_map_to_sdr: video.is_some_and(|x| {
                        x.color.hdr.is_hdr() && !caps.display.handles_hdr(x.color.hdr)
                    }),
                    deinterlace: video.is_some_and(|x| x.field_order.is_interlaced()),
                    burn_in_subtitles: false,
                })
            };
            let new_audio = if ai == 0 {
                audio_path.clone()
            } else {
                AudioPath::Transcode {
                    codec: a.clone().expect("generated targets are always Some"),
                    channels,
                }
            };
            return (ContainerPlan::Remux(c), new_video, new_audio);
        }
    }

    // Nothing in the client's container list can carry any codec combination we can produce. Signal
    // it rather than emitting a plan the client cannot open: an unplayable plan fails on the user's
    // TV mid-session, where a blocked plan fails at planning time with an explanation.
    ctx.reject(
        Tier::T3Adapted,
        RejectReason::ContainerUnsupported { container: source.container, client: caps.id.clone() },
    );
    (ContainerPlan::Unavailable, video_path, audio_path)
}

fn best_container(
    caps: &ClientCapabilities,
    video: Option<&VideoCodec>,
    audio: Option<&AudioCodec>,
) -> Option<Container> {
    let mut candidates: Vec<Container> = caps
        .containers
        .iter()
        .copied()
        .filter(|c| video.is_none_or(|v| c.accepts_video(v)))
        .filter(|c| audio.is_none_or(|a| c.accepts_audio(a)))
        .collect();
    candidates.sort_by_key(|c| c.remux_preference());
    candidates.first().copied()
}

/// The subtitle ladder from `docs/13` §5. Burn-in is the last resort, never the first response —
/// it destroys the text irreversibly for the session and forces a video transcode.
fn decide_subtitle(
    subtitle: Option<&SubtitleStream>,
    container: ContainerPlan,
    delivered: Option<Container>,
    video: Option<&VideoStream>,
    video_path: VideoPath,
    caps: &ClientCapabilities,
    ctx: &mut Ctx,
) -> (SubtitleDelivery, VideoPath) {
    let Some(s) = subtitle else {
        return (SubtitleDelivery::None, video_path);
    };

    // Captions ride inside the video elementary stream, so container rules never apply — but the
    // client must still be able to draw them. A client that cannot decode CEA-608 needs the captions
    // extracted to WebVTT server-side (`docs/13` §5), not handed captions it will silently ignore.
    if s.codec.is_in_band_caption() && !s.external && caps.subtitles.can_render(&s.codec) {
        return (SubtitleDelivery::InBand, video_path);
    }

    let target_container = match container {
        ContainerPlan::Original | ContainerPlan::Unavailable => None,
        ContainerPlan::Remux(c) => Some(c),
    };

    if caps.subtitles.can_render(&s.codec) {
        let carried = match target_container {
            None => !s.external,
            Some(c) => c.accepts_subtitle(&s.codec),
        };
        if carried {
            return (SubtitleDelivery::InBand, video_path);
        }
        if caps.subtitles.accepts_out_of_band {
            return (SubtitleDelivery::OutOfBand { as_format: s.codec.clone() }, video_path);
        }
    }

    // Not directly renderable. Text formats convert losslessly enough to be worth it; bitmap
    // formats cannot become text without OCR, which is an offline job, not a playback decision.
    if !s.codec.is_bitmap() && caps.subtitles.accepts_out_of_band {
        for target in [SubtitleCodec::WebVtt, SubtitleCodec::SubRip] {
            if caps.subtitles.can_render(&target) {
                return (SubtitleDelivery::OutOfBand { as_format: target }, video_path);
            }
        }
    }

    let why = if caps.subtitles.accepts_out_of_band {
        BurnInCause::ClientCannotRender
    } else {
        BurnInCause::NoCarriageAndNoOutOfBand
    };
    ctx.reject(
        Tier::T2Preserved,
        RejectReason::SubtitleBurnInRequired { format: s.codec.clone(), why },
    );

    let video_path = match video_path {
        VideoPath::Transcode(spec) => {
            VideoPath::Transcode(VideoTranscodeSpec { burn_in_subtitles: true, ..spec })
        }
        VideoPath::Drop => VideoPath::Drop,
        VideoPath::Copy => match pick_video_target_for(caps, delivered) {
            Some(codec) => VideoPath::Transcode(VideoTranscodeSpec {
                codec,
                // Clamp to the source: burning in subtitles must never become an upscale
                // (`docs/13` §3.1), which would cost bitrate for no picture detail.
                max_width: video.map_or(caps.display.width, |v| caps.display.width.min(v.width)),
                max_height: video
                    .map_or(caps.display.height, |v| caps.display.height.min(v.height)),
                max_bitrate_bps: bitrate_ceiling(caps, None),
                tone_map_to_sdr: false,
                deinterlace: false,
                burn_in_subtitles: true,
            }),
            None => VideoPath::Drop,
        },
    };
    (SubtitleDelivery::BurnedIn, video_path)
}
