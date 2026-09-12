//! Property tests for the playback ladder.
//!
//! `docs/04` §9 and `docs/13` §8 both make the same demand: for **any** (source × capability set),
//! the emitted plan must be executable by those capabilities. A ladder that can emit an unplayable
//! plan is worse than no ladder, because the failure surfaces mid-playback on a user's TV rather
//! than at planning time.
//!
//! These properties are the executable form of that requirement. `check_playable` is the referee —
//! written independently of the ladder so it cannot inherit the ladder's mistakes.

use lumen_caps::{
    AudioSinkCaps, ClientCapabilities, DisplayCaps, SubtitleCaps, TranscodePolicy, UserPolicy,
    VideoDecodeCaps,
};
use lumen_model::{
    AudioCodec, AudioStream, ChannelLayout, ChromaSubsampling, ColorInfo, ColorPrimaries,
    Container, CropRect, FieldOrder, HdrFormat, Integrity, Language, MediaSource, Rational,
    StereoMode, StreamFlags, SubtitleCodec, SubtitleStream, TelecinePattern, Transport, VideoCodec,
    VideoStream,
};
use lumen_playback::{
    AudioPath, ContainerPlan, PlaybackPlan, Selection, SubtitleDelivery, Tier, VideoPath, plan,
};
use proptest::prelude::*;

// ── The referee ──────────────────────────────────────────────────────────────────────────────────

/// Independently verify that `caps` can actually execute `plan`. Returns the first violation.
fn check_playable(
    p: &PlaybackPlan,
    source: &MediaSource,
    sel: Selection,
    caps: &ClientCapabilities,
) -> Result<(), String> {
    if p.is_blocked() {
        // A blocked plan is executable by definition, but it must say why.
        return if p.rejections.is_empty() {
            Err("blocked plan carries no reason (violates docs/11 §7)".into())
        } else {
            Ok(())
        };
    }

    // Container: the client must be able to open whatever it is handed.
    //
    // Carriage rules are checked only for remux targets. They describe what *we* may mux, not what
    // already exists: a file in the wild may pair a codec with a container our table calls illegal
    // (AV1 in MPEG-TS, say), and per `docs/12` Rule 2 that file still Direct Plays. Applying the
    // muxing table to Direct Play would reject working files.
    let remuxing = matches!(p.container, ContainerPlan::Remux(_));
    let delivered = match p.container {
        ContainerPlan::Original => source.container,
        ContainerPlan::Remux(c) => c,
        ContainerPlan::Unavailable => {
            return Err("Unavailable must be converted to a blocked plan before it escapes".into());
        }
    };
    if !caps.accepts_container(delivered) {
        return Err(format!("client cannot open delivered container {delivered:?}"));
    }

    // Video: a copied stream must be decodable as-is; a transcoded one must target a decodable codec
    // and stay inside the decoder's limits.
    let video = sel.video.and_then(|i| source.video.iter().find(|s| s.index == i));
    if let Some(v) = video {
        match &p.video {
            // Rung 9: audio-only is always executable.
            VideoPath::Drop => {}
            VideoPath::Copy => {
                let dc = caps
                    .video_caps_for(&v.codec)
                    .ok_or_else(|| format!("copied {:?} has no decoder", v.codec))?;
                if v.bit_depth > dc.max_bit_depth {
                    return Err(format!(
                        "copied {}-bit exceeds {}-bit",
                        v.bit_depth, dc.max_bit_depth
                    ));
                }
                if v.width > dc.max_width || v.height > dc.max_height {
                    return Err(format!(
                        "copied {}x{} exceeds {}x{}",
                        v.width, v.height, dc.max_width, dc.max_height
                    ));
                }
                if !dc.accepts_profile(v.profile.as_deref()) {
                    return Err("copied stream has an unaccepted profile".into());
                }
                if remuxing && !delivered.accepts_video(&v.codec) {
                    return Err(format!("{delivered:?} cannot carry {:?}", v.codec));
                }
            }
            VideoPath::Transcode(spec) => {
                let dc = caps
                    .video_caps_for(&spec.codec)
                    .ok_or_else(|| format!("transcode target {:?} has no decoder", spec.codec))?;
                if spec.max_width > dc.max_width || spec.max_height > dc.max_height {
                    return Err("transcode target exceeds decoder geometry".into());
                }
                if spec.max_width > v.width || spec.max_height > v.height {
                    return Err("transcode upscales, forbidden by docs/13 §3.1".into());
                }
                if !delivered.accepts_video(&spec.codec) {
                    return Err(format!("{delivered:?} cannot carry {:?}", spec.codec));
                }
            }
        }
    }

    // Audio: whatever reaches the sink must be something the sink accepts.
    let audio = sel.audio.and_then(|i| source.audio.iter().find(|s| s.index == i));
    if let Some(a) = audio {
        let sink = &caps.audio_sink;
        match &p.audio {
            AudioPath::None => {}
            AudioPath::Passthrough => {
                if !sink.can_passthrough(&a.codec) {
                    return Err(format!("passthrough of {:?} to a sink that rejects it", a.codec));
                }
                if remuxing && !delivered.accepts_audio(&a.codec) {
                    return Err(format!("{delivered:?} cannot carry {:?}", a.codec));
                }
            }
            AudioPath::ExclusiveBitPerfect => {
                if !sink.exclusive_available {
                    return Err("bit-perfect requested on a sink without exclusive access".into());
                }
                if a.layout.channels > sink.max_pcm_channels {
                    return Err("bit-perfect exceeds sink channel count".into());
                }
                if !sink.supports_sample_rate(a.sample_rate) {
                    return Err("bit-perfect at an unsupported sample rate".into());
                }
            }
            AudioPath::DecodeToLpcm { channels, sample_rate, resampled, .. } => {
                if *channels > sink.max_pcm_channels {
                    return Err(format!(
                        "LPCM {channels}ch exceeds sink {}",
                        sink.max_pcm_channels
                    ));
                }
                if !*resampled && !sink.supports_sample_rate(*sample_rate) {
                    return Err("claims no resample but the rate is unsupported".into());
                }
                if *resampled && !sink.supports_sample_rate(*sample_rate) {
                    return Err("resampled to a rate the sink still cannot take".into());
                }
            }
            AudioPath::CoreExtraction { core } => {
                if !sink.can_passthrough(core) {
                    return Err(format!("extracted core {core:?} rejected by the sink"));
                }
                if a.codec.extractable_core().as_ref() != Some(core) {
                    return Err("extracted a core the source does not contain".into());
                }
            }
            AudioPath::Transcode { codec, channels } => {
                if *channels > sink.max_pcm_channels && !sink.can_passthrough(codec) {
                    return Err("transcode exceeds sink channels with no passthrough".into());
                }
                if !delivered.accepts_audio(codec) {
                    return Err(format!("{delivered:?} cannot carry transcode target {codec:?}"));
                }
            }
        }
    }

    // Subtitles: never hand a client a format it cannot draw.
    let sub = sel.subtitle.and_then(|i| source.subtitles.iter().find(|s| s.index == i));
    if let Some(s) = sub {
        match &p.subtitle {
            SubtitleDelivery::None | SubtitleDelivery::BurnedIn => {}
            SubtitleDelivery::InBand => {
                if !caps.subtitles.can_render(&s.codec) {
                    return Err(format!("in-band {:?} the client cannot render", s.codec));
                }
                if remuxing
                    && !s.codec.is_in_band_caption()
                    && !delivered.accepts_subtitle(&s.codec)
                {
                    return Err(format!("{delivered:?} cannot carry {:?}", s.codec));
                }
            }
            SubtitleDelivery::OutOfBand { as_format } => {
                if !caps.subtitles.accepts_out_of_band {
                    return Err("out-of-band delivery to a client that refuses it".into());
                }
                if !caps.subtitles.can_render(as_format) {
                    return Err(format!("out-of-band {as_format:?} the client cannot render"));
                }
            }
        }
    }

    // Burn-in forces a video transcode; a plan claiming both burn-in and a copied stream is
    // internally inconsistent and would fail at execution time.
    if p.subtitle == SubtitleDelivery::BurnedIn && p.video.is_copy() {
        return Err("burn-in with a copied video stream is not executable".into());
    }
    Ok(())
}

// ── Generators ───────────────────────────────────────────────────────────────────────────────────

fn any_video_codec() -> impl Strategy<Value = VideoCodec> {
    prop_oneof![
        Just(VideoCodec::H264),
        Just(VideoCodec::Hevc),
        Just(VideoCodec::Av1),
        Just(VideoCodec::Vp9),
        Just(VideoCodec::Vc1),
        Just(VideoCodec::Mpeg2),
        Just(VideoCodec::ProRes),
        Just(VideoCodec::Other("V_EXOTIC".into())),
    ]
}

fn any_audio_codec() -> impl Strategy<Value = AudioCodec> {
    prop_oneof![
        Just(AudioCodec::TrueHd),
        Just(AudioCodec::DtsHdMa),
        Just(AudioCodec::DtsX),
        Just(AudioCodec::EAc3),
        Just(AudioCodec::Ac3),
        Just(AudioCodec::Flac),
        Just(AudioCodec::Pcm),
        Just(AudioCodec::Aac),
        Just(AudioCodec::Opus),
        Just(AudioCodec::Other("A_EXOTIC".into())),
    ]
}

fn any_subtitle_codec() -> impl Strategy<Value = SubtitleCodec> {
    prop_oneof![
        Just(SubtitleCodec::Ass),
        Just(SubtitleCodec::SubRip),
        Just(SubtitleCodec::WebVtt),
        Just(SubtitleCodec::Pgs),
        Just(SubtitleCodec::VobSub),
        Just(SubtitleCodec::Cea608),
    ]
}

fn any_hdr() -> impl Strategy<Value = HdrFormat> {
    prop_oneof![
        Just(HdrFormat::Sdr),
        Just(HdrFormat::Hdr10),
        Just(HdrFormat::Hdr10Plus),
        Just(HdrFormat::Hlg),
        Just(HdrFormat::DolbyVisionP5),
        Just(HdrFormat::DolbyVisionP8),
        Just(HdrFormat::DolbyVisionP7Fel),
    ]
}

prop_compose! {
    fn any_source()(
        container in prop_oneof![
            Just(Container::Matroska), Just(Container::Mp4), Just(Container::MpegTs),
            Just(Container::WebM), Just(Container::Avi),
        ],
        transport in prop_oneof![Just(Transport::Local), Just(Transport::NetworkShare), Just(Transport::Http)],
        vcodec in any_video_codec(),
        width in prop_oneof![Just(176u32), Just(720), Just(1920), Just(3840), Just(7680)],
        bit_depth in prop_oneof![Just(8u8), Just(10), Just(12)],
        hdr in any_hdr(),
        interlaced in any::<bool>(),
        acodec in any_audio_codec(),
        channels in prop_oneof![Just(1u8), Just(2), Just(6), Just(8), Just(24)],
        sample_rate in prop_oneof![Just(44_100u32), Just(48_000), Just(96_000), Just(192_000)],
        has_objects in any::<bool>(),
        scodec in proptest::option::of(any_subtitle_codec()),
        bitrate in prop_oneof![Just(None), Just(Some(2_000_000u64)), Just(Some(92_000_000))],
        integrity in prop_oneof![
            Just(Integrity::Intact), Just(Integrity::RecoveredComplete), Just(Integrity::RecoveredLossy)
        ],
    ) -> MediaSource {
        let mut s = MediaSource::new(container, transport);
        s.bitrate_bps = bitrate;
        s.integrity = integrity;
        s.video.push(VideoStream {
            index: 0,
            codec: vcodec,
            profile: None,
            level: None,
            width,
            height: width * 9 / 16,
            sample_aspect: Rational::new(1, 1),
            frame_rate: Some(Rational::NTSC_FILM),
            bit_depth,
            color: ColorInfo { hdr, ..ColorInfo::default() },
            field_order: if interlaced { FieldOrder::TopFieldFirst } else { FieldOrder::Progressive },
            stereo_mode: StereoMode::Mono,
            bitrate_bps: None,
            flags: StreamFlags::enabled(),
            crop: CropRect::default(),
            telecine: TelecinePattern::default(),
            chroma: ChromaSubsampling::default(),
        });
        s.audio.push(AudioStream {
            index: 1,
            codec: acodec,
            layout: ChannelLayout::new(channels),
            sample_rate,
            bit_depth: Some(24),
            bitrate_bps: None,
            language: Language::new("eng"),
            title: None,
            flags: StreamFlags::enabled(),
            has_objects,
        });
        if let Some(c) = scodec {
            s.subtitles.push(SubtitleStream {
                index: 2,
                codec: c,
                language: Language::new("eng"),
                title: None,
                flags: StreamFlags::enabled(),
                external: false,
            });
        }
        s
    }
}

prop_compose! {
    fn any_caps()(
        containers in proptest::collection::vec(
            prop_oneof![
                Just(Container::Matroska), Just(Container::Mp4), Just(Container::FragmentedMp4),
                Just(Container::MpegTs), Just(Container::WebM),
            ], 0..5),
        decoders in proptest::collection::vec(any_video_codec(), 0..5),
        max_bit_depth in prop_oneof![Just(8u8), Just(10), Just(12)],
        max_dim in prop_oneof![Just(1920u32), Just(3840), Just(7680)],
        hd_sink in any::<bool>(),
        max_pcm_channels in prop_oneof![Just(2u8), Just(6), Just(8)],
        exclusive in any::<bool>(),
        hdr_display in any::<bool>(),
        can_tone_map in any::<bool>(),
        full_subs in any::<bool>(),
        out_of_band in any::<bool>(),
        network in prop_oneof![Just(None), Just(Some(20_000_000u64)), Just(Some(940_000_000))],
        policy in prop_oneof![
            Just(TranscodePolicy::Allowed), Just(TranscodePolicy::AudioOnly), Just(TranscodePolicy::None)
        ],
        bit_perfect in any::<bool>(),
    ) -> ClientCapabilities {
        let mut sink = if hd_sink {
            AudioSinkCaps::hd_avr("generated sink")
        } else {
            AudioSinkCaps::stereo_pcm("generated sink")
        };
        sink.max_pcm_channels = max_pcm_channels;
        sink.exclusive_available = exclusive;

        let mut subs = if full_subs { SubtitleCaps::full() } else { SubtitleCaps::text_only() };
        subs.accepts_out_of_band = out_of_band;

        ClientCapabilities {
            id: "generated".into(),
            containers,
            video: decoders
                .into_iter()
                .map(|c| VideoDecodeCaps::hardware(c, max_bit_depth, max_dim, max_dim))
                .collect(),
            audio_sink: sink,
            display: if hdr_display { DisplayCaps::hdr_4k() } else { DisplayCaps::sdr_1080p() },
            subtitles: subs,
            can_tone_map,
            network_bps: network,
            policy: UserPolicy { bit_perfect, transcode: policy },
        }
    }
}

fn full_selection(s: &MediaSource) -> Selection {
    Selection {
        video: s.video.first().map(|v| v.index),
        audio: s.audio.first().map(|a| a.index),
        subtitle: s.subtitles.first().map(|x| x.index),
    }
}

// ── Properties ───────────────────────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Invariant 1: the plan is always executable by the capabilities it was planned against.
    #[test]
    fn plan_is_always_playable_by_the_caps_it_was_planned_for(
        source in any_source(), caps in any_caps()
    ) {
        let sel = full_selection(&source);
        let p = plan(&source, sel, &caps);
        if let Err(why) = check_playable(&p, &source, sel, &caps) {
            prop_assert!(false, "unplayable plan: {why}\nplan={p:#?}\ncaps={caps:#?}");
        }
    }

    /// Invariant 3: any tier worse than bit-exact carries at least one structured reason.
    /// This is guarantee G1 — no silent degradation — expressed as a property.
    #[test]
    fn degradation_is_never_silent(source in any_source(), caps in any_caps()) {
        let sel = full_selection(&source);
        let p = plan(&source, sel, &caps);
        if p.tier > Tier::T0BitExact {
            prop_assert!(
                !p.rejections.is_empty(),
                "tier {:?} with no explanation\nplan={p:#?}", p.tier
            );
            for line in p.explain() {
                prop_assert!(line.len() > 20, "unhelpful explanation: {line}");
            }
        }
    }

    /// Burn-in must never be chosen while out-of-band delivery was available — `docs/13` §5 treats
    /// burn-in as a last resort, and this is the property that keeps it one.
    #[test]
    fn burn_in_only_when_out_of_band_is_impossible(source in any_source(), caps in any_caps()) {
        let sel = full_selection(&source);
        let p = plan(&source, sel, &caps);
        if p.subtitle == SubtitleDelivery::BurnedIn {
            let s = sel.subtitle
                .and_then(|i| source.subtitles.iter().find(|x| x.index == i))
                .expect("burn-in implies a selected subtitle");
            let could_have_sent_it =
                caps.subtitles.accepts_out_of_band && caps.subtitles.can_render(&s.codec);
            prop_assert!(!could_have_sent_it, "burned in {:?} that could have been sent: {p:#?}", s.codec);
        }
    }

    /// Planning is deterministic: the same inputs must always produce the same plan, or a client and
    /// a server planning the same session would disagree.
    #[test]
    fn planning_is_deterministic(source in any_source(), caps in any_caps()) {
        let sel = full_selection(&source);
        prop_assert_eq!(plan(&source, sel, &caps), plan(&source, sel, &caps));
    }

    /// The ladder never re-encodes audio while the sink would have taken the bitstream untouched
    /// *and* a container was available to carry it. Guards the "no needless transcoding" claim at
    /// its most expensive point.
    ///
    /// The carriage qualifier is not a weakening: a sink can accept TrueHD while no container the
    /// client opens is able to carry it (a WebM-only client, say), and re-encoding is then the only
    /// option rather than a missed opportunity.
    ///
    /// Written as a guard rather than `prop_assume!` because the precondition holds for well under
    /// half of generated pairs, which exhausts proptest's global reject budget before the property
    /// has seen enough interesting cases.
    #[test]
    fn passthrough_is_taken_whenever_the_sink_and_a_container_allow_it(
        source in any_source(), caps in any_caps()
    ) {
        let sel = full_selection(&source);
        let Some(a) = sel.audio.and_then(|i| source.audio.iter().find(|x| x.index == i)) else {
            return Ok(());
        };
        if !caps.audio_sink.can_passthrough(&a.codec) {
            return Ok(());
        }
        let carriable = caps.accepts_container(source.container)
            || caps.containers.iter().any(|c| c.accepts_audio(&a.codec));
        if !carriable {
            return Ok(());
        }

        let p = plan(&source, sel, &caps);
        let delivered = match p.container {
            ContainerPlan::Original => Some(source.container),
            ContainerPlan::Remux(c) => Some(c),
            ContainerPlan::Unavailable => None,
        };
        // A remux target that cannot carry the codec is the one legitimate reason to give up the
        // bitstream; Direct Play imposes no carriage constraint (the file already exists).
        let carriage_blocked = matches!(p.container, ContainerPlan::Remux(_))
            && delivered.is_some_and(|d| !d.accepts_audio(&a.codec));
        prop_assert!(
            p.is_blocked() || p.audio == AudioPath::Passthrough || carriage_blocked,
            "sink accepts the bitstream but the ladder chose {:?}", p.audio
        );
    }
}

/// Generate capabilities with a fixed transcode policy.
///
/// Filtering a general generator with `prop_assume!` rejects two thirds of cases and exhausts
/// proptest's global reject budget before it finds anything interesting, so the policy is fixed at
/// construction instead.
fn caps_with_policy(policy: TranscodePolicy) -> impl Strategy<Value = ClientCapabilities> {
    any_caps().prop_map(move |c| ClientCapabilities {
        policy: UserPolicy { transcode: policy, ..c.policy },
        ..c
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Invariant 4: `TranscodePolicy::None` blocks rather than degrading. The user asked for
    /// bit-exact; handing them a transcode is exactly the failure the setting exists to prevent.
    #[test]
    fn never_transcode_policy_blocks_instead_of_degrading(
        source in any_source(), caps in caps_with_policy(TranscodePolicy::None)
    ) {
        let p = plan(&source, full_selection(&source), &caps);
        prop_assert!(
            p.is_blocked() || (p.is_direct() && p.container == ContainerPlan::Original),
            "policy=None produced a non-direct plan: {p:#?}"
        );
    }

    /// `TranscodePolicy::AudioOnly` protects the video stream absolutely — the remux-owner setting.
    #[test]
    fn audio_only_policy_never_touches_video(
        source in any_source(), caps in caps_with_policy(TranscodePolicy::AudioOnly)
    ) {
        let p = plan(&source, full_selection(&source), &caps);
        prop_assert!(
            p.is_blocked() || p.video.is_copy() || p.video == VideoPath::Drop,
            "video transcoded under AudioOnly: {p:#?}"
        );
    }

    /// Lossy recovery always surfaces as T4, whatever else the ladder decided, and always carries
    /// an explanation. A truncated file that direct-plays perfectly still owes the user the fact
    /// that content is missing.
    #[test]
    fn lossy_recovery_always_surfaces(
        source in any_source().prop_map(|s| MediaSource { integrity: Integrity::RecoveredLossy, ..s }),
        caps in any_caps(),
    ) {
        let p = plan(&source, full_selection(&source), &caps);
        prop_assert!(
            p.tier == Tier::T4Recovered || p.is_blocked(),
            "lossy recovery hidden behind tier {:?}", p.tier
        );
        prop_assert!(
            p.reason_keys().contains(&"SourceIncomplete") || p.is_blocked(),
            "damage not explained: {:?}", p.reason_keys()
        );
    }
}

// ── Worked examples: the headline scenarios, asserted concretely ─────────────────────────────────

fn uhd_remux() -> MediaSource {
    let mut s = MediaSource::new(Container::Matroska, Transport::NetworkShare);
    s.bitrate_bps = Some(92_000_000);
    s.video.push(VideoStream {
        index: 0,
        codec: VideoCodec::Hevc,
        profile: Some("Main 10".into()),
        level: Some(51),
        width: 3840,
        height: 2160,
        sample_aspect: Rational::new(1, 1),
        frame_rate: Some(Rational::NTSC_FILM),
        bit_depth: 10,
        color: ColorInfo { hdr: HdrFormat::Hdr10, ..ColorInfo::default() },
        field_order: FieldOrder::Progressive,
        stereo_mode: StereoMode::Mono,
        bitrate_bps: None,
        flags: StreamFlags::enabled(),
        crop: CropRect::default(),
        telecine: TelecinePattern::default(),
        chroma: ChromaSubsampling::default(),
    });
    s.audio.push(AudioStream {
        index: 1,
        codec: AudioCodec::TrueHd,
        layout: ChannelLayout::SURROUND_7_1,
        sample_rate: 48_000,
        bit_depth: Some(24),
        bitrate_bps: None,
        language: Language::new("eng"),
        title: None,
        flags: StreamFlags::enabled(),
        has_objects: true,
    });
    s.subtitles.push(SubtitleStream {
        index: 2,
        codec: SubtitleCodec::Pgs,
        language: Language::new("eng"),
        title: None,
        flags: StreamFlags::enabled(),
        external: false,
    });
    s
}

#[test]
fn uhd_remux_to_a_capable_native_client_is_bit_exact() {
    // Conformance vector `codec-video-hevc-uhd-remux` + `codec-audio-truehd-atmos-passthrough`.
    let p =
        plan(&uhd_remux(), full_selection(&uhd_remux()), &ClientCapabilities::reference_native());
    assert_eq!(p.tier, Tier::T0BitExact, "{p:#?}");
    assert_eq!(p.container, ContainerPlan::Original);
    assert_eq!(p.audio, AudioPath::Passthrough);
    assert_eq!(p.subtitle, SubtitleDelivery::InBand);
    assert!(p.is_direct());
}

#[test]
fn macos_style_sink_decodes_truehd_to_lpcm_and_says_why() {
    // docs/03 §5.4: CoreAudio has no HBR bitstream path, so Atmos objects are flattened. The
    // corpus encodes this as a platform override, and the ladder must produce exactly it.
    let mut caps = ClientCapabilities::reference_native();
    caps.audio_sink = AudioSinkCaps {
        passthrough_encodings: vec![],
        max_pcm_channels: 8,
        exclusive_available: true,
        ..AudioSinkCaps::hd_avr("MacBook Pro (HDMI)")
    };
    let src = uhd_remux();
    let p = plan(&src, full_selection(&src), &caps);

    assert_eq!(p.tier, Tier::T2Preserved, "{p:#?}");
    assert!(matches!(p.audio, AudioPath::DecodeToLpcm { channels: 8, objects_lost: true, .. }));
    assert!(p.video.is_copy(), "audio limits must never cost the video stream");
    assert!(p.reason_keys().contains(&"SinkLacksEncoding"));
    assert!(p.explain().iter().any(|m| m.contains("MacBook Pro")), "{:?}", p.explain());
}

#[test]
fn dts_hd_ma_extracts_its_core_rather_than_re_encoding() {
    // docs/13 §4: the core is an original bitstream. Re-encoding here would be a silent quality
    // loss the user did not need to take.
    let mut src = uhd_remux();
    src.audio[0] =
        AudioStream { codec: AudioCodec::DtsHdMa, has_objects: false, ..src.audio[0].clone() };
    let mut caps = ClientCapabilities::reference_native();
    caps.audio_sink = AudioSinkCaps {
        passthrough_encodings: vec![AudioCodec::Dts, AudioCodec::Ac3],
        max_pcm_channels: 2,
        ..AudioSinkCaps::hd_avr("Old AVR (DTS core only)")
    };

    let p = plan(&src, full_selection(&src), &caps);
    assert_eq!(p.audio, AudioPath::CoreExtraction { core: AudioCodec::Dts }, "{p:#?}");
    assert!(!p.audio.reencodes());
    assert_eq!(p.tier, Tier::T2Preserved);
}

#[test]
fn pgs_goes_out_of_band_to_a_browser_rather_than_being_burned_in() {
    // Conformance vector `subtitles-pgs-out-of-band`. Burn-in would force a 4K video transcode to
    // deliver subtitles — the exact reflex docs/13 §5 exists to prevent.
    let mut caps = ClientCapabilities::reference_browser();
    caps.subtitles = SubtitleCaps { accepts_out_of_band: true, ..SubtitleCaps::full() };
    let src = uhd_remux();
    let p = plan(&src, full_selection(&src), &caps);

    assert_eq!(p.subtitle, SubtitleDelivery::OutOfBand { as_format: SubtitleCodec::Pgs }, "{p:#?}");
    assert!(!p.reason_keys().contains(&"SubtitleBurnInRequired"));
}

#[test]
fn bit_perfect_policy_blocks_with_the_underlying_cause_still_visible() {
    // The user must learn *why* Direct Play was impossible, not merely that their setting stopped
    // the fallback — otherwise the setting is unactionable.
    let mut caps = ClientCapabilities::reference_native();
    caps.audio_sink = AudioSinkCaps::stereo_pcm("Bluetooth headphones");
    caps.policy = UserPolicy { bit_perfect: true, transcode: TranscodePolicy::None };
    let src = uhd_remux();
    let p = plan(&src, full_selection(&src), &caps);

    assert!(p.is_blocked());
    let keys = p.reason_keys();
    assert!(keys.contains(&"SinkLacksEncoding"), "root cause lost: {keys:?}");
    assert!(keys.contains(&"UserPolicy"), "policy not reported: {keys:?}");
}

#[test]
fn dv_profile_7_fel_plays_the_base_layer_and_labels_it_honestly() {
    // docs/11 §7: FEL is not reconstructible in open source. The requirement is an honest label on a
    // working base-layer playback, never a support claim.
    let mut src = uhd_remux();
    src.video[0].color.hdr = HdrFormat::DolbyVisionP7Fel;
    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());

    assert!(p.video.is_copy(), "base layer must direct play: {p:#?}");
    assert!(p.reason_keys().contains(&"EnhancementLayerUnsupported"));
    assert!(p.explain().iter().any(|m| m.contains("base layer")));
}

#[test]
fn hi10p_anime_direct_plays_on_a_software_decoder_and_notes_it() {
    // docs/11 §8: no GPU decodes H.264 High 10. Software decode is the correct outcome, not a
    // reason to transcode, and the note explains an otherwise-alarming CPU reading.
    let mut src = MediaSource::new(Container::Matroska, Transport::Local);
    src.video.push(VideoStream {
        index: 0,
        codec: VideoCodec::H264,
        profile: Some("High 10".into()),
        level: Some(51),
        width: 1920,
        height: 1080,
        sample_aspect: Rational::new(1, 1),
        frame_rate: Some(Rational::NTSC_FILM),
        bit_depth: 10,
        color: ColorInfo::default(),
        field_order: FieldOrder::Progressive,
        stereo_mode: StereoMode::Mono,
        bitrate_bps: None,
        flags: StreamFlags::enabled(),
        crop: CropRect::default(),
        telecine: TelecinePattern::default(),
        chroma: ChromaSubsampling::default(),
    });
    src.audio.push(AudioStream {
        index: 1,
        codec: AudioCodec::Flac,
        layout: ChannelLayout::STEREO,
        sample_rate: 48_000,
        bit_depth: Some(24),
        bitrate_bps: None,
        language: Language::new("jpn"),
        title: None,
        flags: StreamFlags::enabled(),
        has_objects: false,
    });

    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());
    assert!(p.video.is_copy(), "{p:#?}");
    assert!(p.tier <= Tier::T1FullFidelity, "tier {:?}", p.tier);
}

#[test]
fn a_level_above_the_decoders_ceiling_forces_a_transcode() {
    // The decoder can decode HEVC Main 10 in general, but not at this level -- e.g. a mobile SoC's
    // HEVC block that tops out at Level 5.0 seeing a Level 5.1 UHD Blu-ray remux.
    let mut caps = ClientCapabilities::reference_native();
    caps.video = vec![VideoDecodeCaps {
        max_level: Some(50),
        ..VideoDecodeCaps::hardware(VideoCodec::Hevc, 10, 3840, 2160)
    }];
    let src = uhd_remux();
    let p = plan(&src, full_selection(&src), &caps);

    assert!(!p.video.is_copy(), "a level past the decoder's ceiling must not direct play: {p:#?}");
    assert!(p.reason_keys().contains(&"VideoCodecUnsupported"), "{:?}", p.reason_keys());
}

#[test]
fn a_level_at_or_below_the_ceiling_direct_plays() {
    let mut caps = ClientCapabilities::reference_native();
    caps.video = vec![VideoDecodeCaps {
        max_level: Some(51),
        ..VideoDecodeCaps::hardware(VideoCodec::Hevc, 10, 3840, 2160)
    }];
    let src = uhd_remux();
    let p = plan(&src, full_selection(&src), &caps);
    assert!(p.video.is_copy(), "level 51 against a ceiling of 51 must direct play: {p:#?}");
}

#[test]
fn chroma_beyond_the_decoders_ceiling_forces_a_transcode() {
    // docs/11 §8: 4:2:2/4:4:4 profiles are the case hardware decoders most often lack entirely.
    let mut src = uhd_remux();
    src.video[0] = VideoStream { chroma: ChromaSubsampling::Yuv444, ..src.video[0].clone() };
    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());

    assert!(!p.video.is_copy(), "4:4:4 past a 4:2:0-only decoder must not direct play: {p:#?}");
    assert!(p.reason_keys().contains(&"ChromaSubsamplingUnsupported"), "{:?}", p.reason_keys());
}

#[test]
fn ordinary_420_chroma_direct_plays() {
    let src = uhd_remux();
    assert_eq!(src.video[0].chroma, ChromaSubsampling::Yuv420);
    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());
    assert!(p.video.is_copy(), "{p:#?}");
}

#[test]
fn bt2020_content_on_a_p3_gamut_display_tone_maps_but_keeps_the_bitstream() {
    // The reference native client can tone/gamut map, so this is a render-side adaptation, not a
    // reason to touch the video stream at all -- mirrors exactly how the HDR-format check behaves.
    let mut src = uhd_remux();
    src.video[0] = VideoStream {
        color: ColorInfo { primaries: ColorPrimaries::Bt2020, ..src.video[0].color },
        ..src.video[0].clone()
    };
    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());

    assert!(p.video.is_copy(), "gamut mapping is a render decision, not a stream one: {p:#?}");
    assert!(p.reason_keys().contains(&"GamutUnsupportedByDisplay"), "{:?}", p.reason_keys());
}

#[test]
fn bt2020_content_forces_a_transcode_when_the_client_cannot_gamut_map() {
    let mut caps = ClientCapabilities::reference_native();
    caps.can_tone_map = false;
    let mut src = uhd_remux();
    src.video[0] = VideoStream {
        color: ColorInfo { primaries: ColorPrimaries::Bt2020, ..src.video[0].color },
        ..src.video[0].clone()
    };
    let p = plan(&src, full_selection(&src), &caps);

    assert!(!p.video.is_copy(), "no gamut mapping available means the stream must adapt: {p:#?}");
    assert!(p.reason_keys().contains(&"GamutUnsupportedByDisplay"), "{:?}", p.reason_keys());
}

#[test]
fn p3_content_fits_the_reference_displays_gamut_exactly() {
    let mut src = uhd_remux();
    src.video[0] = VideoStream {
        color: ColorInfo { primaries: ColorPrimaries::DciP3, ..src.video[0].color },
        ..src.video[0].clone()
    };
    let p = plan(&src, full_selection(&src), &ClientCapabilities::reference_native());
    assert!(p.video.is_copy(), "{p:#?}");
    assert!(!p.reason_keys().contains(&"GamutUnsupportedByDisplay"), "{:?}", p.reason_keys());
}
