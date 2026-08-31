//! Turning what mpv reported about a file into a fidelity tier.
//!
//! `lumen test` already answers "did it play". That is the floor, not the charter. The question this
//! product exists to answer is *how well* it played and, more importantly, **what it would cost on a
//! weaker endpoint** — because a remux that direct-plays on this desktop and needs a full transcode
//! in a browser is a fact about the library that no pass/fail column can carry.
//!
//! So each file is put through the real decision ladder (`lumen-playback`) against two declared
//! capability profiles (`lumen-caps`): a fully capable native client, and a browser. The result is
//! the T0–T5 tier from `docs/11` §1.1 plus every higher tier that was ruled out, with its reason.
//!
//! **This is modelled, not measured.** The stream description comes from a real demux of a real
//! file, so the input is observation; the endpoint is a profile, so the output is what those
//! capabilities *would* yield. The console and JSON both say so, because a tier presented as a
//! measurement when it is a projection is exactly the kind of quiet overclaim `docs/11` §G1 exists
//! to forbid.

use lumen_caps::ClientCapabilities;
use lumen_model::{
    AudioCodec, AudioStream, ChannelLayout, ChromaSubsampling, ColorInfo, ColorMatrix,
    ColorPrimaries, ColorRange, ColorTransfer, Container, HdrFormat, Integrity, Language,
    MediaSource, Rational, StreamFlags, SubtitleCodec, SubtitleStream, Transport, VideoCodec,
    VideoStream,
};
use lumen_playback::{Selection, Tier, TrackPreferences, plan, select};

use crate::scan::ScannedFile;
use crate::session::{FileResult, Outcome, TrackInfo};

/// What one capability profile would make of a file.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileOutcome {
    /// Profile name, as shown to the user.
    pub profile: &'static str,
    pub tier: Tier,
    /// No re-encoding of any kind — the metric `docs/13` §8 publishes per release.
    pub direct: bool,
    /// Every better outcome that was ruled out, in the order the ladder considered it.
    pub reasons: Vec<String>,
}

/// The tiers one file reaches across the profiles we model.
#[derive(Debug, Clone, PartialEq)]
pub struct Fidelity {
    /// A fully capable native client: Matroska, hardware HEVC/AV1, an HD AVR, an HDR display.
    pub native: ProfileOutcome,
    /// The honest floor (`docs/04` §7): fMP4/WebM only, stereo PCM, SDR 1080p, text subtitles.
    pub browser: ProfileOutcome,
}

impl Fidelity {
    /// True when the native profile plays this untouched and the browser cannot.
    pub fn native_only(&self) -> bool {
        self.native.direct && !self.browser.direct
    }
}

/// Model the tiers `r` reaches. `None` when the file did not open, or opened with no stream we can
/// describe — a tier for a file that never demuxed would be fiction.
pub fn assess(r: &FileResult, scanned: &ScannedFile) -> Option<Fidelity> {
    if r.outcome != Outcome::Played {
        return None;
    }
    let source = media_source(r, scanned)?;
    Some(Fidelity {
        native: evaluate("native", r, &source, &native_profile()),
        browser: evaluate("browser", r, &source, &ClientCapabilities::reference_browser()),
    })
}

/// A fully capable native client reading a file off local disk.
///
/// `reference_native` as published, with the network link removed: the bytes are on this machine, so
/// a 100 Mbps remux has no link to exceed, and the ladder's own rule is that an unmeasured link must
/// never trigger rejection.
///
/// `pub(crate)` so `calibration.rs` can ask the same declared capabilities what they claim about a
/// codec's hardware-decode support, without a second, drifting copy of "what the native profile is".
pub(crate) fn native_profile() -> ClientCapabilities {
    ClientCapabilities { network_bps: None, ..ClientCapabilities::reference_native() }
}

fn evaluate(
    name: &'static str,
    r: &FileResult,
    source: &MediaSource,
    caps: &ClientCapabilities,
) -> ProfileOutcome {
    // Prefer the tracks mpv actually chose: this file was really played, and describing a selection
    // that did not happen would be a worse answer than the one in front of us. Only when nothing is
    // marked selected — an older build, or a video-only file — does the automatic selector stand in.
    let selection = mpv_selection(&r.tracks)
        .unwrap_or_else(|| select(source, &TrackPreferences::default(), caps));
    let outcome = plan(source, selection, caps);
    ProfileOutcome {
        profile: name,
        tier: outcome.tier,
        direct: outcome.is_direct() && !outcome.is_blocked(),
        reasons: outcome.explain(),
    }
}

/// The selection mpv arrived at, expressed in our stream indices.
///
/// `None` when mpv marked nothing selected at all, which is the only case where guessing is better
/// than reporting.
fn mpv_selection(tracks: &[TrackInfo]) -> Option<Selection> {
    let pick = |kind: &str| tracks.iter().find(|t| t.kind == kind && t.selected).map(|t| t.id);
    let s = Selection { video: pick("video"), audio: pick("audio"), subtitle: pick("sub") };
    (s.video.is_some() || s.audio.is_some()).then_some(s)
}

/// Build the stream description the ladder plans against, from what the demuxer reported.
///
/// Returns `None` when there is no video and no audio track to describe.
pub fn media_source(r: &FileResult, scanned: &ScannedFile) -> Option<MediaSource> {
    let container = scanned.container.or_else(|| container_from_mpv(r.file_format.as_deref()))?;
    let mut source = MediaSource::new(container, Transport::Local);

    // Overall bitrate from size and duration rather than any declared value: remuxes very often
    // carry no bitrate field at all, and the measured figure is the one `is_remux_class` needs.
    source.duration_ns = r.duration.filter(|d| *d > 0.0).map(|d| (d * 1e9) as i64);
    source.bitrate_bps = r
        .duration
        .filter(|d| *d > 0.5)
        .map(|d| ((scanned.size as f64) * 8.0 / d) as u64)
        .filter(|b| *b > 0);
    source.integrity = Integrity::Intact;

    let bit_depth = bit_depth_from_pixel_format(r.pixel_format.as_deref());
    let color = color_info(r);

    for t in &r.tracks {
        match t.kind.as_str() {
            "video" => source.video.push(VideoStream {
                index: t.id,
                codec: video_codec(t.codec.as_deref()),
                profile: t.codec_profile.clone(),
                level: None,
                // Track geometry where the demuxer gave it, otherwise the decoded frame size.
                width: t.width.or(r.width).unwrap_or(0),
                height: t.height.or(r.height).unwrap_or(0),
                sample_aspect: Rational::new(1, 1),
                frame_rate: t.fps.or(r.fps).and_then(rational_fps),
                bit_depth,
                color,
                // mpv exposes no per-track field order, so this stays at the default rather than
                // being guessed. It only affects the deinterlace flag on a forced transcode.
                field_order: lumen_model::FieldOrder::default(),
                stereo_mode: lumen_model::StereoMode::default(),
                bitrate_bps: t.bitrate_bps,
                flags: flags_of(t),
                // mpv exposes no per-track crop or telecine detection; both stay at their honest
                // defaults rather than being guessed.
                crop: lumen_model::CropRect::default(),
                telecine: lumen_model::TelecinePattern::default(),
                chroma: chroma_from_pixel_format(r.pixel_format.as_deref()),
            }),
            "audio" => source.audio.push(AudioStream {
                index: t.id,
                codec: audio_codec(t.codec.as_deref(), t.codec_profile.as_deref()),
                layout: ChannelLayout::new(t.channels.unwrap_or(2)),
                sample_rate: t.sample_rate.unwrap_or(48_000),
                bit_depth: None,
                bitrate_bps: t.bitrate_bps,
                language: Language::new(t.lang.as_deref().unwrap_or("")),
                title: t.title.clone(),
                flags: flags_of(t),
                // Object detection needs the bitstream, which mpv does not expose. Left false: a
                // false claim of Atmos would be worse than a silent one, and the effect is to
                // report a slightly better tier for a track we cannot fully characterise.
                has_objects: false,
            }),
            "sub" => source.subtitles.push(SubtitleStream {
                index: t.id,
                codec: subtitle_codec(t.codec.as_deref()),
                language: Language::new(t.lang.as_deref().unwrap_or("")),
                title: t.title.clone(),
                flags: flags_of(t),
                external: t.external,
            }),
            _ => {}
        }
    }

    // A file mpv played with no track-list — an older build, or a stream it could not enumerate —
    // still has the selected-stream properties, which describe one video and one audio track.
    if source.video.is_empty() && source.audio.is_empty() {
        return None;
    }
    Some(source)
}

fn flags_of(t: &TrackInfo) -> StreamFlags {
    StreamFlags {
        default: t.default,
        forced: t.forced,
        enabled_default_true: true,
        hearing_impaired: t.hearing_impaired,
        visual_impaired: t.visual_impaired,
        original: false,
        // mpv has no commentary flag; the convention is a title. Checked because auto-selecting a
        // commentary track reads to users as a broken player.
        commentary: t
            .title
            .as_deref()
            .is_some_and(|s| s.to_ascii_lowercase().contains("commentary")),
    }
}

/// mpv reports frame rate as a double; the model keeps it rational, because rounding 24000/1001 to
/// 23.976 accumulates about one frame of drift every forty minutes.
fn rational_fps(fps: f64) -> Option<Rational> {
    if !fps.is_finite() || fps <= 0.0 || fps > 1000.0 {
        return None;
    }
    // NTSC rates land exactly on an integer numerator over 1001, so this recovers 24000/1001 rather
    // than approximating it.
    let num = (fps * 1001.0).round();
    (num > 0.0 && num < f64::from(u32::MAX)).then(|| Rational::new(num as u32, 1001))
}

/// `yuv420p10le` -> 10, `yuv420p` -> 8. Ten-bit is the giveaway for an HDR master, and it is what
/// separates a browser's 8-bit H.264 decoder from a native client's.
fn bit_depth_from_pixel_format(pf: Option<&str>) -> u8 {
    let Some(pf) = pf else { return 8 };
    let base = pf.trim_end_matches("le").trim_end_matches("be");
    let digits: String = base.chars().rev().take_while(char::is_ascii_digit).collect();
    let digits: String = digits.chars().rev().collect();
    match digits.parse::<u8>() {
        Ok(n) if (8..=16).contains(&n) => n,
        _ => 8,
    }
}

/// `yuv444p10le` -> 4:4:4, `yuv422p` -> 4:2:2, everything else (including no report at all) -> the
/// 4:2:0 default, which is both the overwhelming common case and the honest floor: nothing here
/// claims wider chroma support than what mpv actually reported.
fn chroma_from_pixel_format(pf: Option<&str>) -> ChromaSubsampling {
    let Some(pf) = pf else { return ChromaSubsampling::default() };
    if pf.starts_with("yuv444") || pf.starts_with("yuva444") || pf.starts_with("gbr") {
        ChromaSubsampling::Yuv444
    } else if pf.starts_with("yuv422") || pf.starts_with("yuva422") {
        ChromaSubsampling::Yuv422
    } else {
        ChromaSubsampling::default()
    }
}

fn color_info(r: &FileResult) -> ColorInfo {
    let transfer = match r.gamma.as_deref() {
        Some("pq") => ColorTransfer::Pq,
        Some("hlg") => ColorTransfer::Hlg,
        Some("bt.1886") | Some("bt.709") => ColorTransfer::Bt709,
        Some("srgb") => ColorTransfer::Srgb,
        Some("linear") => ColorTransfer::Linear,
        _ => ColorTransfer::Unspecified,
    };
    let primaries = match r.primaries.as_deref() {
        Some("bt.709") => ColorPrimaries::Bt709,
        Some("bt.2020") => ColorPrimaries::Bt2020,
        Some("bt.601-525") => ColorPrimaries::Bt601_525,
        Some("bt.601-625") => ColorPrimaries::Bt601_625,
        Some("dci-p3") => ColorPrimaries::DciP3,
        Some("display-p3") => ColorPrimaries::DisplayP3,
        _ => ColorPrimaries::Unspecified,
    };
    // mpv's `video-params/colormatrix` uses the same naming family as `primaries`/`gamma`.
    let matrix = match r.colormatrix.as_deref() {
        Some("bt.709") => ColorMatrix::Bt709,
        Some("bt.601") => ColorMatrix::Bt601,
        Some("bt.2020-ncl") => ColorMatrix::Bt2020Ncl,
        Some("bt.2020-cl") => ColorMatrix::Bt2020Cl,
        Some("ycgco") => ColorMatrix::YCgCo,
        Some("ictcp") => ColorMatrix::IcTcP,
        _ => ColorMatrix::Unspecified,
    };
    // The transfer function decides HDR, not the primaries: BT.2020 with a conventional gamma curve
    // is wide-gamut SDR, and conflating the two would misreport a distinction this product exists to
    // get right. Dolby Vision is not detectable from these properties, so PQ is reported as HDR10 —
    // which is exactly what a DV file's base layer is.
    let hdr = match transfer {
        ColorTransfer::Pq => HdrFormat::Hdr10,
        ColorTransfer::Hlg => HdrFormat::Hlg,
        _ => HdrFormat::Sdr,
    };
    ColorInfo { primaries, transfer, matrix, range: ColorRange::Unspecified, hdr, mastering: None }
}

/// mpv's `file-format` is FFmpeg's demuxer name, which is often a comma-separated family.
///
/// Only a fallback: the scanner sniffs the container from the bytes, and that answer wins. This
/// covers the case where the sniffer declined and mpv opened the file anyway.
pub fn container_from_mpv(format: Option<&str>) -> Option<Container> {
    let f = format?.to_ascii_lowercase();
    let has = |needle: &str| f.split(',').any(|part| part.trim() == needle);
    if has("webm") && !has("matroska") {
        return Some(Container::WebM);
    }
    if has("matroska") {
        return Some(Container::Matroska);
    }
    if has("mp4") || has("mov") || has("m4a") {
        return Some(Container::Mp4);
    }
    if has("mpegts") {
        return Some(Container::MpegTs);
    }
    if has("mpeg") {
        return Some(Container::MpegPs);
    }
    if has("avi") {
        return Some(Container::Avi);
    }
    if has("asf") {
        return Some(Container::Asf);
    }
    if has("flv") {
        return Some(Container::Flv);
    }
    if has("ogg") {
        return Some(Container::Ogg);
    }
    None
}

/// FFmpeg's video codec names.
///
/// An unknown name becomes `Other`, never an error: per `docs/12` §1 rule 2 an unrecognised codec
/// must not fail the file. It does mean no declared client can decode it, which is the honest
/// answer — we genuinely do not know that it can.
pub fn video_codec(name: Option<&str>) -> VideoCodec {
    match name.unwrap_or("").to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => VideoCodec::H264,
        "hevc" | "h265" | "hvc1" | "hev1" => VideoCodec::Hevc,
        "vvc" | "h266" => VideoCodec::Vvc,
        "av1" => VideoCodec::Av1,
        "vp8" => VideoCodec::Vp8,
        "vp9" => VideoCodec::Vp9,
        "mpeg1video" => VideoCodec::Mpeg1,
        "mpeg2video" => VideoCodec::Mpeg2,
        "mpeg4" | "msmpeg4v3" | "msmpeg4v2" | "div3" | "divx" | "xvid" => VideoCodec::Mpeg4Part2,
        // WMV3 is VC-1 Simple/Main profile in an ASF wrapper; treating it as anything else would
        // send it down a different decoder path for no reason.
        "vc1" | "wmv3" => VideoCodec::Vc1,
        "theora" => VideoCodec::Theora,
        "prores" => VideoCodec::ProRes,
        "prores_raw" | "aprn" | "aprh" => VideoCodec::ProResRaw,
        "dnxhd" | "dnxhr" => VideoCodec::DnxHd,
        "apv" => VideoCodec::Apv,
        "ffv1" => VideoCodec::Ffv1,
        "mjpeg" => VideoCodec::Mjpeg,
        "dvvideo" => VideoCodec::Dv,
        "rawvideo" | "v210" | "yuv4" => VideoCodec::Uncompressed,
        "h263" | "h263p" | "h263i" => VideoCodec::H263,
        "cinepak" => VideoCodec::Cinepak,
        "indeo2" | "indeo3" | "indeo4" | "indeo5" => VideoCodec::Indeo,
        "svq3" => VideoCodec::Svq3,
        "qtrle" => VideoCodec::QtRle,
        "utvideo" => VideoCodec::UtVideo,
        other => VideoCodec::Other(other.to_string()),
    }
}

/// FFmpeg's audio codec names, refined by profile where the build reports one.
///
/// The profile is what separates DTS-HD Master Audio from the DTS core it contains, and TrueHD from
/// its AC-3 core. That distinction decides whether an AVR gets a lossless bitstream or a lossy one,
/// which is the difference between T0 and T2 on exactly the files this product exists for.
pub fn audio_codec(name: Option<&str>, profile: Option<&str>) -> AudioCodec {
    let name = name.unwrap_or("").to_ascii_lowercase();
    let profile = profile.unwrap_or("").to_ascii_uppercase();
    if name.starts_with("pcm_") {
        return AudioCodec::Pcm;
    }
    if name.starts_with("dsd_") {
        return AudioCodec::Dsd;
    }
    if name.starts_with("adpcm_") {
        return AudioCodec::Adpcm;
    }
    match name.as_str() {
        "truehd" | "mlp" => AudioCodec::TrueHd,
        "dts" => {
            // FFmpeg profile strings: "DTS-HD MA", "DTS-HD MA + DTS:X", "DTS-HD HRA", "DTS".
            if profile.contains("DTS:X") || profile.contains("DTS-X") {
                AudioCodec::DtsX
            } else if profile.contains("MA") {
                AudioCodec::DtsHdMa
            } else if profile.contains("HRA") {
                AudioCodec::DtsHdHra
            } else {
                // No profile reported: the core is what we can be sure of. Claiming MA on a plain
                // DTS track would promise a lossless bitstream that is not there.
                AudioCodec::Dts
            }
        }
        "flac" => AudioCodec::Flac,
        "alac" => AudioCodec::Alac,
        "wavpack" => AudioCodec::WavPack,
        "ape" | "monkeysaudio" => AudioCodec::MonkeysAudio,
        "ac3" => AudioCodec::Ac3,
        "eac3" => AudioCodec::EAc3,
        "ac4" => AudioCodec::Ac4,
        "aac" | "aac_latm" => AudioCodec::Aac,
        "opus" => AudioCodec::Opus,
        "vorbis" => AudioCodec::Vorbis,
        "mp3" | "mp3float" => AudioCodec::Mp3,
        "mp2" | "mp2float" => AudioCodec::Mp2,
        "wmav1" | "wmav2" | "wmapro" => AudioCodec::Wma,
        "wmalossless" => AudioCodec::WmaLossless,
        other => AudioCodec::Other(other.to_string()),
    }
}

/// FFmpeg's subtitle codec names.
pub fn subtitle_codec(name: Option<&str>) -> SubtitleCodec {
    match name.unwrap_or("").to_ascii_lowercase().as_str() {
        "subrip" | "srt" => SubtitleCodec::SubRip,
        "ass" | "ssa" => SubtitleCodec::Ass,
        "webvtt" | "vtt" => SubtitleCodec::WebVtt,
        "ttml" | "stl" => SubtitleCodec::Ttml,
        "microdvd" => SubtitleCodec::MicroDvd,
        "subviewer" | "subviewer1" => SubtitleCodec::SubViewer,
        "mov_text" | "tx3g" => SubtitleCodec::MovText,
        "hdmv_pgs_subtitle" | "pgssub" => SubtitleCodec::Pgs,
        "dvd_subtitle" | "dvdsub" => SubtitleCodec::VobSub,
        "dvb_subtitle" | "dvbsub" => SubtitleCodec::DvbSub,
        "eia_608" | "cc_dec" => SubtitleCodec::Cea608,
        "eia_708" | "cea_708" => SubtitleCodec::Cea708,
        "dvb_teletext" | "teletext" => SubtitleCodec::Teletext,
        other => SubtitleCodec::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;
    use crate::scan::{ScanOptions, scan};
    use crate::session::parse_tracks;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let anchor = 0u8;
            let d = std::env::temp_dir().join(format!(
                "lumen-fid-{tag}-{}-{:x}",
                std::process::id(),
                std::ptr::from_ref(&anchor) as usize
            ));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A scanned Matroska file of `size` bytes, so `media_source` has a container and a size.
    fn scanned(tag: &str, size: usize) -> (TempDir, ScannedFile) {
        let d = TempDir::new(tag);
        let p = d.0.join("Some.Film.2019.2160p.BluRay.REMUX.mkv");
        let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3];
        bytes.extend(std::iter::repeat_n(0u8, size));
        std::fs::File::create(&p).unwrap().write_all(&bytes).unwrap();
        let found = scan(std::slice::from_ref(&d.0), &ScanOptions::default());
        let f = found.files.into_iter().next().expect("the scan must see the file");
        (d, f)
    }

    fn result(tracks: Vec<TrackInfo>) -> FileResult {
        let mut r = FileResult {
            path: PathBuf::from("x.mkv"),
            label: "x".into(),
            outcome: Outcome::Played,
            seconds_played: 1.0,
            file_format: Some("matroska,webm".into()),
            video_codec: None,
            audio_codec: None,
            width: Some(3840),
            height: Some(2160),
            fps: Some(24000.0 / 1001.0),
            duration: Some(7200.0),
            hwdec: Some("no".into()),
            pixel_format: Some("yuv420p10le".into()),
            primaries: Some("bt.2020".into()),
            gamma: Some("pq".into()),
            colormatrix: Some("bt.2020-ncl".into()),
            seekable: Some(true),
            audio_channels: Some("8".into()),
            track_counts: Default::default(),
            tracks: Vec::new(),
            fidelity: None,
            delayed_frames: None,
            dropped_frames: None,
        };
        r.tracks = tracks;
        r
    }

    fn track(kind: &str, id: u32, codec: &str) -> TrackInfo {
        TrackInfo {
            kind: kind.into(),
            id,
            codec: Some(codec.into()),
            selected: true,
            default: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_hdr_remux_direct_plays_natively_and_never_in_a_browser() {
        // The whole point of modelling two profiles. This file — HEVC Main 10, PQ, TrueHD, in
        // Matroska — is the product's central case, and a browser cannot open the container, let
        // alone bitstream the audio.
        let (_d, f) = scanned("remux", 4_000_000);
        let mut audio = track("audio", 2, "truehd");
        audio.channels = Some(8);
        audio.sample_rate = Some(48_000);
        let r = result(vec![track("video", 1, "hevc"), audio]);

        let fid = assess(&r, &f).expect("a played file with tracks must be assessable");
        assert_eq!(fid.native.tier, Tier::T0BitExact, "{:?}", fid.native.reasons);
        assert!(fid.native.direct);
        assert!(!fid.browser.direct, "a browser cannot open Matroska or bitstream TrueHD");
        assert!(
            !fid.browser.reasons.is_empty(),
            "guarantee G1: a degraded outcome must always carry its reasons"
        );
        assert!(fid.native_only());
    }

    #[test]
    fn a_plain_h264_stereo_file_is_direct_on_both_profiles() {
        let (_d, f) = scanned("plain", 200_000);
        let mut r = result(vec![track("video", 1, "h264"), {
            let mut a = track("audio", 2, "aac");
            a.channels = Some(2);
            a.sample_rate = Some(48_000);
            a
        }]);
        r.pixel_format = Some("yuv420p".into());
        r.primaries = Some("bt.709".into());
        r.gamma = Some("bt.1886".into());
        r.width = Some(1920);
        r.height = Some(1080);

        let fid = assess(&r, &f).unwrap();
        assert!(fid.native.direct);
        // The browser still remuxes Matroska into fMP4 and decodes AAC to PCM, so it is not T0 —
        // but nothing is re-encoded, which is what `direct` means.
        assert!(fid.browser.direct, "{:?}", fid.browser.reasons);
        assert!(fid.browser.tier <= Tier::T2Preserved, "{:?}", fid.browser.tier);
    }

    #[test]
    fn a_file_that_never_played_is_not_given_a_tier() {
        let (_d, f) = scanned("failed", 1_000);
        let mut r = result(vec![track("video", 1, "h264")]);
        r.outcome = Outcome::Failed("unrecognized file format".into());
        assert!(assess(&r, &f).is_none(), "a tier for a file that did not open would be fiction");

        let mut r = result(Vec::new());
        r.outcome = Outcome::Played;
        assert!(assess(&r, &f).is_none(), "no tracks means nothing to describe");
    }

    #[test]
    fn dts_without_a_profile_is_the_core_not_master_audio() {
        // Claiming MA on a track we cannot confirm would promise a lossless bitstream that may not
        // be there — and the AVR would be asked for an encoding the file cannot supply.
        assert_eq!(audio_codec(Some("dts"), None), AudioCodec::Dts);
        assert_eq!(audio_codec(Some("dts"), Some("DTS-HD MA")), AudioCodec::DtsHdMa);
        assert_eq!(audio_codec(Some("dts"), Some("DTS-HD MA + DTS:X")), AudioCodec::DtsX);
        assert_eq!(audio_codec(Some("dts"), Some("DTS-HD HRA")), AudioCodec::DtsHdHra);
        assert_eq!(audio_codec(Some("pcm_s24le"), None), AudioCodec::Pcm);
        assert_eq!(audio_codec(Some("truehd"), None), AudioCodec::TrueHd);
        assert_eq!(
            audio_codec(Some("nellymoser"), None),
            AudioCodec::Other("nellymoser".into()),
            "an unknown codec is representable, not an error"
        );
    }

    #[test]
    fn video_and_subtitle_names_map_to_the_model() {
        assert_eq!(video_codec(Some("hevc")), VideoCodec::Hevc);
        assert_eq!(video_codec(Some("wmv3")), VideoCodec::Vc1, "WMV3 is VC-1 SP/MP");
        assert_eq!(video_codec(Some("msmpeg4v3")), VideoCodec::Mpeg4Part2);
        assert_eq!(video_codec(None), VideoCodec::Other(String::new()));
        assert_eq!(subtitle_codec(Some("hdmv_pgs_subtitle")), SubtitleCodec::Pgs);
        assert_eq!(subtitle_codec(Some("ass")), SubtitleCodec::Ass);
        assert_eq!(subtitle_codec(Some("eia_608")), SubtitleCodec::Cea608);
    }

    #[test]
    fn legacy_and_previously_uncatalogued_codecs_now_map_to_a_real_variant() {
        // Proposal 4: these previously fell through to `Other`, which is correct for a codec this
        // product has never heard of but wrong for ones it now recognises by name.
        assert_eq!(video_codec(Some("cinepak")), VideoCodec::Cinepak);
        assert_eq!(video_codec(Some("indeo5")), VideoCodec::Indeo);
        assert_eq!(video_codec(Some("h263p")), VideoCodec::H263);
        assert_eq!(video_codec(Some("svq3")), VideoCodec::Svq3);
        assert_eq!(video_codec(Some("qtrle")), VideoCodec::QtRle);
        assert_eq!(video_codec(Some("utvideo")), VideoCodec::UtVideo);

        assert_eq!(audio_codec(Some("adpcm_ms"), None), AudioCodec::Adpcm);
        assert_eq!(audio_codec(Some("adpcm_ima_wav"), None), AudioCodec::Adpcm);
        assert_eq!(audio_codec(Some("wmalossless"), None), AudioCodec::WmaLossless);
        assert_eq!(audio_codec(Some("wmapro"), None), AudioCodec::Wma, "lossy WMA stays lossy");

        assert_eq!(subtitle_codec(Some("mov_text")), SubtitleCodec::MovText);
    }

    #[test]
    fn bit_depth_comes_out_of_the_pixel_format() {
        assert_eq!(bit_depth_from_pixel_format(Some("yuv420p")), 8);
        assert_eq!(bit_depth_from_pixel_format(Some("yuv420p10le")), 10);
        assert_eq!(bit_depth_from_pixel_format(Some("yuv444p12le")), 12);
        assert_eq!(bit_depth_from_pixel_format(Some("gbrp")), 8);
        assert_eq!(bit_depth_from_pixel_format(None), 8);
    }

    #[test]
    fn ntsc_frame_rates_survive_the_round_trip_exactly() {
        // 23.976 is not 24000/1001, and the difference is a frame of drift every forty minutes.
        let r = rational_fps(24000.0 / 1001.0).unwrap();
        assert_eq!((r.num, r.den), (24000, 1001));
        assert_eq!(rational_fps(25.0).map(|r| (r.num, r.den)), Some((25_025, 1001)));
        assert_eq!(rational_fps(0.0), None);
        assert_eq!(rational_fps(f64::NAN), None);
    }

    #[test]
    fn the_sniffed_container_wins_over_mpvs_demuxer_family() {
        // `matroska,webm` is ambiguous; the scanner read the bytes and knows which.
        assert_eq!(container_from_mpv(Some("matroska,webm")), Some(Container::Matroska));
        assert_eq!(container_from_mpv(Some("webm")), Some(Container::WebM));
        assert_eq!(container_from_mpv(Some("mov,mp4,m4a,3gp,3g2,mj2")), Some(Container::Mp4));
        assert_eq!(container_from_mpv(Some("mpegts")), Some(Container::MpegTs));
        assert_eq!(container_from_mpv(Some("avi")), Some(Container::Avi));
        assert_eq!(container_from_mpv(None), None);
    }

    #[test]
    fn hdr_is_decided_by_the_transfer_function_not_the_primaries() {
        let mut r = result(Vec::new());
        r.primaries = Some("bt.2020".into());
        r.gamma = Some("bt.1886".into());
        assert_eq!(color_info(&r).hdr, HdrFormat::Sdr, "wide-gamut SDR is not HDR");
        r.gamma = Some("pq".into());
        assert_eq!(color_info(&r).hdr, HdrFormat::Hdr10);
        r.gamma = Some("hlg".into());
        assert_eq!(color_info(&r).hdr, HdrFormat::Hlg);
    }

    #[test]
    fn chroma_is_read_from_the_pixel_format_and_defaults_honestly() {
        assert_eq!(chroma_from_pixel_format(Some("yuv420p")), ChromaSubsampling::Yuv420);
        assert_eq!(chroma_from_pixel_format(Some("yuv420p10le")), ChromaSubsampling::Yuv420);
        assert_eq!(chroma_from_pixel_format(Some("yuv422p")), ChromaSubsampling::Yuv422);
        assert_eq!(chroma_from_pixel_format(Some("yuv444p10le")), ChromaSubsampling::Yuv444);
        assert_eq!(chroma_from_pixel_format(Some("gbrp")), ChromaSubsampling::Yuv444);
        assert_eq!(
            chroma_from_pixel_format(None),
            ChromaSubsampling::default(),
            "unreported chroma is not a claim of anything wider than the honest floor"
        );
        assert_eq!(chroma_from_pixel_format(Some("nv12")), ChromaSubsampling::Yuv420);
    }

    #[test]
    fn colormatrix_is_read_the_same_way_as_primaries_and_gamma() {
        let mut r = result(Vec::new());
        r.colormatrix = Some("bt.2020-ncl".into());
        assert_eq!(color_info(&r).matrix, ColorMatrix::Bt2020Ncl);
        r.colormatrix = Some("bt.709".into());
        assert_eq!(color_info(&r).matrix, ColorMatrix::Bt709);
        r.colormatrix = None;
        assert_eq!(color_info(&r).matrix, ColorMatrix::Unspecified, "unknown is not a guess");
    }

    #[test]
    fn the_selection_follows_what_mpv_actually_chose() {
        let mut first = track("audio", 2, "aac");
        first.selected = false;
        let mut second = track("audio", 3, "truehd");
        second.selected = true;
        let s = mpv_selection(&[track("video", 1, "h264"), first, second]).unwrap();
        assert_eq!(s.audio, Some(3), "the track that played, not the first one listed");
        assert_eq!(s.video, Some(1));
        assert_eq!(s.subtitle, None);

        // Nothing selected at all: the automatic selector has to stand in.
        let mut none = track("video", 1, "h264");
        none.selected = false;
        assert!(mpv_selection(&[none]).is_none());
    }

    #[test]
    fn tracks_parse_out_of_a_real_track_list() {
        let text = r#"[
            {"id":1,"type":"video","codec":"hevc","codec-profile":"Main 10","selected":true,
             "default":true,"demux-w":3840,"demux-h":2160,"demux-fps":23.976023976023978},
            {"id":2,"type":"audio","codec":"dts","codec-profile":"DTS-HD MA","lang":"eng",
             "selected":true,"demux-channel-count":8,"demux-samplerate":48000},
            {"id":3,"type":"sub","codec":"hdmv_pgs_subtitle","lang":"eng","forced":true,
             "external":false,"selected":false}
        ]"#;
        let tracks = parse_tracks(&parse(text).unwrap());
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].codec_profile.as_deref(), Some("Main 10"));
        assert_eq!(tracks[1].channels, Some(8));
        assert_eq!(tracks[1].lang.as_deref(), Some("eng"));
        assert!(tracks[2].forced && !tracks[2].selected);

        // A DTS-HD MA track keeps its identity through the mapping, which is what lets the AVR be
        // offered the lossless bitstream rather than the core.
        assert_eq!(
            audio_codec(tracks[1].codec.as_deref(), tracks[1].codec_profile.as_deref()),
            AudioCodec::DtsHdMa
        );
    }
}
